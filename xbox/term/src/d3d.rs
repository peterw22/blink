//! Direct3D 11 renderer on a UWP CoreWindow: a flip-model swap chain, the glyph atlas as an R8
//! texture and one instanced quad draw, mirroring the wgpu pipeline of the desktop version.

use windows::core::{Interface, Result, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::UI::Core::CoreWindow;

use crate::atlas::Instance;

const SHADER: &str = r#"
cbuffer Globals : register(b0) { float2 screen; float2 pad; };
Texture2D atlas : register(t0);
SamplerState samp : register(s0);

struct VsIn {
    uint vi : SV_VertexID;
    float2 pos : POS;
    float2 size : SIZE;
    float2 uv0 : UVA;
    float2 uv1 : UVB;
    float4 color : COLOR;
    float2 mode : MODE;
};
struct VsOut {
    float4 clip : SV_Position;
    float2 uv : TEXCOORD0;
    float4 color : COLOR0;
    float mode : TEXCOORD1;
};

static const float2 corners[6] = {
    float2(0, 0), float2(1, 0), float2(0, 1),
    float2(0, 1), float2(1, 0), float2(1, 1),
};

VsOut vs_main(VsIn i) {
    float2 c = corners[i.vi];
    float2 p = i.pos + c * i.size;
    VsOut o;
    o.clip = float4(p.x / screen.x * 2.0 - 1.0, 1.0 - p.y / screen.y * 2.0, 0.0, 1.0);
    o.uv = i.uv0 + c * (i.uv1 - i.uv0);
    o.color = i.color;
    o.mode = i.mode.x;
    return o;
}

float4 ps_main(VsOut i) : SV_Target {
    float a = atlas.Sample(samp, i.uv).r;
    float alpha = i.mode > 0.5 ? 1.0 : a;
    return float4(i.color.rgb, i.color.a * alpha);
}
"#;

pub struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    pub width: u32,
    pub height: u32,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    sampler: ID3D11SamplerState,
    globals: ID3D11Buffer,
    instances: ID3D11Buffer,
    instance_cap: usize,
    atlas: Option<ID3D11ShaderResourceView>,
    pub adapter_name: String,
}

fn compile(entry: &str, target: &str) -> std::result::Result<ID3DBlob, String> {
    let entry = format!("{entry}\0");
    let target = format!("{target}\0");
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(
            SHADER.as_ptr() as _,
            SHADER.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry.as_ptr()),
            PCSTR(target.as_ptr()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    match (r, code) {
        (Ok(()), Some(code)) => Ok(code),
        (r, _) => {
            let msg = errors.map(|e| unsafe { String::from_utf8_lossy(blob_bytes(&e)).into_owned() }).unwrap_or_default();
            Err(format!("D3DCompile {entry} failed: {r:?} {msg}"))
        }
    }
}

unsafe fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
}

fn elem(name: &'static [u8], format: DXGI_FORMAT) -> D3D11_INPUT_ELEMENT_DESC {
    D3D11_INPUT_ELEMENT_DESC {
        SemanticName: PCSTR(name.as_ptr()),
        SemanticIndex: 0,
        Format: format,
        InputSlot: 0,
        AlignedByteOffset: D3D11_APPEND_ALIGNED_ELEMENT,
        InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
        InstanceDataStepRate: 1,
    }
}

impl Gpu {
    pub fn new(window: &CoreWindow, width: u32, height: u32) -> std::result::Result<Self, String> {
        Self::create(window, width, height).map_err(|e| e.to_string())?
    }

    fn create(window: &CoreWindow, width: u32, height: u32) -> Result<std::result::Result<Self, String>> {
        unsafe {
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
            let device: ID3D11Device = device.unwrap();
            let ctx: ID3D11DeviceContext = ctx.unwrap();

            let dxgi_device: IDXGIDevice = device.cast()?;
            let adapter = dxgi_device.GetAdapter()?;
            let desc = adapter.GetDesc()?;
            let n = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            let adapter_name = String::from_utf16_lossy(&desc.Description[..n]);
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let sc_desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: false.into(),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
            };
            let swap = factory.CreateSwapChainForCoreWindow(&device, window, &sc_desc, None)?;

            let vs_blob = match compile("vs_main", "vs_4_0") {
                Ok(b) => b,
                Err(e) => return Ok(Err(e)),
            };
            let ps_blob = match compile("ps_main", "ps_4_0") {
                Ok(b) => b,
                Err(e) => return Ok(Err(e)),
            };
            let mut vs = None;
            device.CreateVertexShader(blob_bytes(&vs_blob), None, Some(&mut vs))?;
            let mut ps = None;
            device.CreatePixelShader(blob_bytes(&ps_blob), None, Some(&mut ps))?;
            let elems = [
                elem(b"POS\0", DXGI_FORMAT_R32G32_FLOAT),
                elem(b"SIZE\0", DXGI_FORMAT_R32G32_FLOAT),
                elem(b"UVA\0", DXGI_FORMAT_R32G32_FLOAT),
                elem(b"UVB\0", DXGI_FORMAT_R32G32_FLOAT),
                elem(b"COLOR\0", DXGI_FORMAT_R32G32B32A32_FLOAT),
                elem(b"MODE\0", DXGI_FORMAT_R32G32_FLOAT),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elems, blob_bytes(&vs_blob), Some(&mut layout))?;

            let mut blend_desc = D3D11_BLEND_DESC::default();
            blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_SRC_ALPHA,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&blend_desc, Some(&mut blend))?;
            let raster_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;
            let sampler_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                ComparisonFunc: D3D11_COMPARISON_NEVER,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&sampler_desc, Some(&mut sampler))?;
            let globals = Self::make_buffer(&device, 16, D3D11_BIND_CONSTANT_BUFFER)?;
            let instance_cap = 4096;
            let instances = Self::make_buffer(&device, instance_cap * std::mem::size_of::<Instance>(), D3D11_BIND_VERTEX_BUFFER)?;

            let mut gpu = Gpu {
                device,
                ctx,
                swap,
                rtv: None,
                width,
                height,
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                layout: layout.unwrap(),
                blend: blend.unwrap(),
                raster: raster.unwrap(),
                sampler: sampler.unwrap(),
                globals,
                instances,
                instance_cap,
                atlas: None,
                adapter_name,
            };
            gpu.make_rtv()?;
            Ok(Ok(gpu))
        }
    }

    fn make_buffer(device: &ID3D11Device, size: usize, bind: D3D11_BIND_FLAG) -> Result<ID3D11Buffer> {
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: size as u32,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: bind.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut buf = None;
        unsafe { device.CreateBuffer(&desc, None, Some(&mut buf))? };
        Ok(buf.unwrap())
    }

    fn make_rtv(&mut self) -> Result<()> {
        unsafe {
            let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            self.rtv = rtv;
        }
        Ok(())
    }

    /// Upload a single-channel glyph texture of `width` x `height`.
    pub fn upload_atlas(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<()> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr() as _, SysMemPitch: width, SysMemSlicePitch: 0 };
        unsafe {
            let mut tex = None;
            self.device.CreateTexture2D(&desc, Some(&data), Some(&mut tex))?;
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex.unwrap(), None, Some(&mut srv))?;
            self.atlas = srv;
        }
        Ok(())
    }

    unsafe fn write<T: bytemuck::Pod>(&self, buf: &ID3D11Buffer, data: &[T]) -> Result<()> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        self.ctx.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
        let bytes: &[u8] = bytemuck::cast_slice(data);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.pData as *mut u8, bytes.len());
        self.ctx.Unmap(buf, 0);
        Ok(())
    }

    /// Draw one frame and present it, waiting for vsync.
    pub fn render(&mut self, clear: [f32; 4], instances: &[Instance]) -> Result<()> {
        unsafe {
            if instances.len() > self.instance_cap {
                self.instance_cap = instances.len().next_power_of_two();
                self.instances = Self::make_buffer(&self.device, self.instance_cap * std::mem::size_of::<Instance>(), D3D11_BIND_VERTEX_BUFFER)?;
            }
            self.write(&self.globals, &[self.width as f32, self.height as f32, 0.0, 0.0])?;
            if !instances.is_empty() {
                self.write(&self.instances, instances)?;
            }
            let ctx = &self.ctx;
            let rtv = self.rtv.clone();
            ctx.OMSetRenderTargets(Some(&[rtv.clone()]), None);
            ctx.ClearRenderTargetView(rtv.as_ref().unwrap(), &clear);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: self.width as f32, Height: self.height as f32, MinDepth: 0.0, MaxDepth: 1.0 }]));
            ctx.RSSetState(&self.raster);
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            let stride = std::mem::size_of::<Instance>() as u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(self.instances.clone())), Some(&stride), Some(&offset));
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.globals.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetShaderResources(0, Some(&[self.atlas.clone()]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            if !instances.is_empty() {
                ctx.DrawInstanced(6, instances.len() as u32, 0, 0);
            }
            self.swap.Present(1, DXGI_PRESENT(0)).ok()?;
            // flip-model swap chains unbind the back buffer on present
            ctx.OMSetRenderTargets(Some(&[rtv]), None);
        }
        Ok(())
    }

    /// Release GPU memory the driver holds on to while the app is suspended.
    pub fn trim(&self) {
        if let Ok(d) = self.device.cast::<IDXGIDevice3>() {
            unsafe { d.Trim() };
        }
    }
}
