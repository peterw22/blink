//! The glyph atlas: every character the terminal can draw, rasterized once
//! with fontdue into a 2D grid of cell-sized slots (two cells wide for wide
//! characters), from a chain of fonts. Plus the quad format d3d.rs draws.

use bytemuck::{Pod, Zeroable};
use std::collections::HashMap;

/// One quad: a glyph from the atlas (mode 0) or a solid rectangle (mode 1).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
    pub color: [f32; 4],
    pub mode: [f32; 2],
}

pub const FONTS: [&[u8]; 4] = [
    include_bytes!("../fonts/NotoSansMono-Regular.ttf"),
    include_bytes!("../fonts/NotoSansSymbols2-Regular.ttf"),
    include_bytes!("../fonts/NotoSansMath-Regular.ttf"),
    include_bytes!("../fonts/NotoEmoji-Regular.ttf"),
];

/// Character ranges baked up front: what terminal programs (and Claude
/// Code's TUI) draw. Anything else shows as a placeholder.
const RANGES: &[(u32, u32)] = &[
    (0x20, 0x7e),
    (0xa0, 0x17f),     // Latin-1, Latin Extended-A
    (0x370, 0x3ff),    // Greek
    (0x400, 0x4ff),    // Cyrillic
    (0x2000, 0x206f),  // general punctuation
    (0x2070, 0x20cf),  // super/subscripts, currency
    (0x2100, 0x214f),  // letterlike
    (0x2190, 0x21ff),  // arrows
    (0x2200, 0x22ff),  // math operators
    (0x2300, 0x23ff),  // misc technical (⏺ ⎿ ⌘)
    (0x2460, 0x24ff),  // enclosed alphanumerics
    (0x2500, 0x259f),  // box drawing, block elements
    (0x25a0, 0x25ff),  // geometric shapes
    (0x2600, 0x26ff),  // misc symbols
    (0x2700, 0x27bf),  // dingbats (✻ ✶ ✢ ✔ ✗)
    (0x27f0, 0x27ff),  // arrows
    (0x2800, 0x28ff),  // braille (spinners)
    (0x2900, 0x297f),  // arrows
    (0x2b00, 0x2bff),  // misc symbols and arrows
    (0x1f300, 0x1f5ff), // emoji
    (0x1f600, 0x1f64f),
    (0x1f680, 0x1f6ff),
    (0x1f900, 0x1f9ff),
];

pub struct Atlas {
    pub cell_w: u32,
    pub cell_h: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    slots: HashMap<char, ([f32; 2], [f32; 2])>,
}

impl Atlas {
    /// Atlas uv rectangle for `c`.
    pub fn uv(&self, c: char) -> Option<([f32; 2], [f32; 2])> {
        self.slots.get(&c).copied()
    }

    pub fn bake(font_px: f32) -> Result<Atlas, String> {
        let fonts: Vec<fontdue::Font> = FONTS
            .iter()
            .map(|b| fontdue::Font::from_bytes(*b, fontdue::FontSettings { scale: font_px, ..Default::default() }))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let (m, _) = fonts[0].rasterize('M', font_px);
        let cell_w = m.advance_width.round().max(4.0) as u32;
        let lm = fonts[0].horizontal_line_metrics(font_px).ok_or("no line metrics")?;
        let cell_h = (lm.ascent - lm.descent + lm.line_gap).ceil().max(cell_w as f32 * 1.6) as u32;
        let baseline = ((cell_h as f32 - (lm.ascent - lm.descent)) / 2.0 + lm.ascent).round() as i32;

        // (char, font, cells wide)
        let mut chars = Vec::new();
        for &(a, b) in RANGES {
            for u in a..=b {
                let Some(c) = char::from_u32(u) else { continue };
                if let Some(fi) = fonts.iter().position(|f| f.lookup_glyph_index(c) != 0) {
                    let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1).clamp(1, 2) as u32;
                    chars.push((c, fi, w));
                }
            }
        }
        let width = 4096u32;
        let per_row = width / (cell_w * 2);
        let rows = (chars.len() as u32).div_ceil(per_row);
        let height = rows * cell_h;
        let mut pixels = vec![0u8; (width * height) as usize];
        let mut slots = HashMap::new();
        for (i, &(c, fi, w)) in chars.iter().enumerate() {
            let (sx, sy) = ((i as u32 % per_row) * cell_w * 2, (i as u32 / per_row) * cell_h);
            let slot_w = cell_w * w;
            let (gm, bitmap) = fonts[fi].rasterize(c, font_px);
            // the monospace font places itself; fallback fonts have other
            // advance widths, so center their glyphs in the slot
            let x0 = if fi == 0 { sx as i32 + gm.xmin } else { sx as i32 + (slot_w as i32 - gm.width as i32) / 2 };
            let y0 = sy as i32 + baseline - gm.height as i32 - gm.ymin;
            for y in 0..gm.height {
                for x in 0..gm.width {
                    let (px, py) = (x0 + x as i32, y0 + y as i32);
                    if px < sx as i32 || px >= (sx + slot_w) as i32 || py < sy as i32 || py >= (sy + cell_h) as i32 {
                        continue;
                    }
                    let d = &mut pixels[(py as u32 * width + px as u32) as usize];
                    *d = (*d).max(bitmap[y * gm.width + x]);
                }
            }
            let u0 = [sx as f32 / width as f32, sy as f32 / height as f32];
            let u1 = [(sx + slot_w) as f32 / width as f32, (sy + cell_h) as f32 / height as f32];
            slots.insert(c, (u0, u1));
        }
        Ok(Atlas { cell_w, cell_h, width, height, pixels, slots })
    }
}
