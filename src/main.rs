//! Blink Terminal: a terminal emulator for the Xbox (UWP, CoreWindow +
//! Direct3D 11) whose shell is Alpine Linux running under the blink x86-64
//! emulator (blink-xbox), for Claude Code.
//!
//! LocalState holds the guest: push the root filesystem as rootfs.tar
//! (tools/xbox.py push) and it's unpacked into LocalState\root on launch.
//! LocalState\command.txt optionally replaces the default command
//! (/bin/bash -l), one argument per line. log.txt is the app's own log.

#![windows_subsystem = "windows"]

mod atlas;
mod d3d;
mod keys;
mod term;

use std::cell::RefCell;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{implement, Ref, Result, HSTRING};
use windows::ApplicationModel::Core::{CoreApplication, CoreApplicationView, IFrameworkView, IFrameworkViewSource, IFrameworkViewSource_Impl, IFrameworkView_Impl};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Display::Core::HdmiDisplayInformation;
use windows::Graphics::Display::DisplayInformation;
use windows::System::Display::DisplayRequest;
use windows::System::VirtualKey;
use windows::UI::Core::{CharacterReceivedEventArgs, CoreProcessEventsOption, CoreVirtualKeyStates, CoreWindow, CoreWindowEventArgs, KeyEventArgs};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use atlas::{Atlas, Instance};
use term::{Cell, Color, Term};

static LOG: Mutex<Option<File>> = Mutex::new(None);

fn log_line(s: &str) {
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{s}");
            let _ = f.flush();
        }
    }
}

macro_rules! log {
    ($($arg:tt)*) => { crate::log_line(&format!($($arg)*)) };
}

fn local_state() -> Option<PathBuf> {
    let p = windows::Storage::ApplicationData::Current().ok()?.LocalFolder().ok()?.Path().ok()?;
    Some(PathBuf::from(p.to_string_lossy()))
}

fn open_log() {
    if let Some(dir) = local_state() {
        if let Ok(f) = File::create(dir.join("log.txt")) {
            *LOG.lock().unwrap() = Some(f);
        }
    }
    std::panic::set_hook(Box::new(|info| log_line(&format!("panic: {info}"))));
}

fn main() -> Result<()> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
    let source: IFrameworkViewSource = Source.into();
    CoreApplication::Run(&source)
}

#[implement(IFrameworkViewSource)]
struct Source;

impl IFrameworkViewSource_Impl for Source_Impl {
    fn CreateView(&self) -> Result<IFrameworkView> {
        Ok(View { window: RefCell::new(None) }.into())
    }
}

#[implement(IFrameworkView)]
struct View {
    window: RefCell<Option<CoreWindow>>,
}

impl IFrameworkView_Impl for View_Impl {
    fn Initialize(&self, _: Ref<'_, CoreApplicationView>) -> Result<()> {
        open_log();
        log!("blink-term {}", env!("CARGO_PKG_VERSION"));
        Ok(())
    }

    fn SetWindow(&self, window: Ref<'_, CoreWindow>) -> Result<()> {
        *self.window.borrow_mut() = Some(window.ok()?.clone());
        Ok(())
    }

    fn Load(&self, _: &HSTRING) -> Result<()> {
        Ok(())
    }

    fn Run(&self) -> Result<()> {
        let window = self.window.borrow().clone().unwrap_or(CoreWindow::GetForCurrentThread()?);
        let r = run(&window);
        if let Err(e) = &r {
            log!("fatal: {e:?}");
        }
        r
    }

    fn Uninitialize(&self) -> Result<()> {
        Ok(())
    }
}

/// Swap chain size: the TV's mode when the HDMI API answers, else the window.
fn screen_size(window: &CoreWindow) -> (u32, u32) {
    if let Ok(mode) = HdmiDisplayInformation::GetForCurrentView().and_then(|h| h.GetCurrentDisplayMode()) {
        if let (Ok(w), Ok(h)) = (mode.ResolutionWidthInRawPixels(), mode.ResolutionHeightInRawPixels()) {
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
    }
    let scale = DisplayInformation::GetForCurrentView().and_then(|d| d.RawPixelsPerViewPixel()).unwrap_or(1.0);
    match window.Bounds() {
        Ok(b) if b.Width > 0.0 => ((b.Width as f64 * scale).round() as u32, (b.Height as f64 * scale).round() as u32),
        _ => (1920, 1080),
    }
}

/// The guest's terminal input, once blink has started.
static INPUT: OnceLock<Arc<blink_xbox::win::Tty>> = OnceLock::new();

fn send(bytes: &[u8]) {
    if let Some(t) = INPUT.get() {
        t.push_input(bytes);
    }
}

/// Columns the screen is laid out for; the font size follows from it.
const COLUMNS: u32 = 120;

fn run(window: &CoreWindow) -> Result<()> {
    window.Activate()?;
    let closed = Arc::new(AtomicBool::new(false));
    {
        let closed = closed.clone();
        window.Closed(&TypedEventHandler::<CoreWindow, CoreWindowEventArgs>::new(move |_, _| {
            closed.store(true, Ordering::SeqCst);
            Ok(())
        }))?;
    }
    let display_request = DisplayRequest::new().ok();
    if let Some(r) = &display_request {
        let _ = r.RequestActive();
    }
    let dispatcher = window.Dispatcher()?;

    let (w, h) = screen_size(window);
    let mut gpu = match d3d::Gpu::new(window, w, h) {
        Ok(g) => g,
        Err(e) if (w, h) != (1920, 1080) => {
            log!("{e}; retrying at 1920x1080");
            d3d::Gpu::new(window, 1920, 1080).map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e))?
        }
        Err(e) => return Err(windows::core::Error::new(windows::core::HRESULT(-1), e)),
    };
    log!("adapter: {}  swap chain {}x{}", gpu.adapter_name, gpu.width, gpu.height);

    // the font size that fits COLUMNS columns (monospace advance ~0.6 em)
    let font_px = (gpu.width as f32 / COLUMNS as f32 / 0.6).floor().max(10.0);
    let err = |e: String| windows::core::Error::new(windows::core::HRESULT(-1), e);
    let atlas = Atlas::bake(font_px).map_err(err)?;
    gpu.upload_atlas(&atlas.pixels, atlas.width, atlas.height)?;
    let cols = (gpu.width / atlas.cell_w) as usize;
    let rows = (gpu.height / atlas.cell_h) as usize;
    log!("font {font_px}px, cell {}x{}, grid {cols}x{rows}, atlas {}x{}", atlas.cell_w, atlas.cell_h, atlas.width, atlas.height);

    let term = Arc::new(Mutex::new(Term::new(rows, cols)));
    hook_keyboard(window, term.clone())?;
    {
        let term = term.clone();
        std::thread::spawn(move || session(term, rows as u16, cols as u16));
    }

    let mut instances = Vec::with_capacity(rows * cols * 2);
    let started = Instant::now();
    let mut last_blink = false;
    while !closed.load(Ordering::SeqCst) {
        dispatcher.ProcessEvents(CoreProcessEventsOption::ProcessAllIfPresent)?;
        let blink_on = (started.elapsed().as_millis() / 530) % 2 == 0;
        let redraw = {
            let mut t = term.lock().unwrap();
            let d = t.dirty || blink_on != last_blink;
            t.dirty = false;
            if d {
                instances.clear();
                draw(&t, &atlas, blink_on, &mut instances);
            }
            d
        };
        last_blink = blink_on;
        if redraw {
            gpu.render(rgba(term::DEFAULT_BG, 1.0), &instances)?;
        } else {
            std::thread::sleep(Duration::from_millis(8));
        }
    }
    Ok(())
}

fn rgba(c: [u8; 3], a: f32) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a]
}

fn resolve(c: Color, default: [u8; 3], bold: bool) -> [u8; 3] {
    match c {
        Color::Default => default,
        Color::Idx(i) if bold && i < 8 => term::palette(i + 8),
        Color::Idx(i) => term::palette(i),
        Color::Rgb(r, g, b) => [r, g, b],
    }
}

fn draw(t: &Term, atlas: &Atlas, blink_on: bool, out: &mut Vec<Instance>) {
    let (cw, ch) = (atlas.cell_w as f32, atlas.cell_h as f32);
    let rect = |out: &mut Vec<Instance>, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]| {
        out.push(Instance { pos: [x, y], size: [w, h], uv0: [0.0; 2], uv1: [0.0; 2], color: c, mode: [1.0, 0.0] });
    };
    for y in 0..t.rows {
        let line = t.line(y, 0);
        let py = y as f32 * ch;
        for (x, cell) in line.iter().enumerate() {
            if cell.wide == 2 {
                continue;
            }
            let Cell { ch: c, fg, bg, attr, wide } = *cell;
            let mut fgc = resolve(fg, term::DEFAULT_FG, attr & term::BOLD != 0);
            let mut bgc = resolve(bg, term::DEFAULT_BG, false);
            let mut has_bg = bg != Color::Default;
            if attr & term::INVERSE != 0 {
                std::mem::swap(&mut fgc, &mut bgc);
                has_bg = true;
            }
            let cursor_here = t.cursor_visible && blink_on && x == t.cx && y == t.cy;
            if cursor_here {
                std::mem::swap(&mut fgc, &mut bgc);
                has_bg = true;
            }
            let span = if wide == 1 { 2.0 } else { 1.0 };
            let px = x as f32 * cw;
            if has_bg {
                rect(out, px, py, cw * span, ch, rgba(bgc, 1.0));
            }
            let alpha = if attr & term::DIM != 0 { 0.6 } else { 1.0 };
            if c != ' ' && attr & term::HIDDEN == 0 {
                let uv = atlas.uv(c).or_else(|| atlas.uv('\u{fffd}')).or_else(|| atlas.uv('?'));
                if let Some((uv0, uv1)) = uv {
                    out.push(Instance { pos: [px, py], size: [cw * span, ch], uv0, uv1, color: rgba(fgc, alpha), mode: [0.0, 0.0] });
                }
            }
            let thick = (ch / 16.0).max(1.0).round();
            if attr & term::UNDERLINE != 0 {
                rect(out, px, py + ch - thick * 2.0, cw * span, thick, rgba(fgc, alpha));
            }
            if attr & term::STRIKE != 0 {
                rect(out, px, py + ch * 0.55, cw * span, thick, rgba(fgc, alpha));
            }
        }
    }
}

fn hook_keyboard(window: &CoreWindow, term: Arc<Mutex<Term>>) -> Result<()> {
    let mods = |w: &CoreWindow| -> keys::Mods {
        let down = |k: VirtualKey| w.GetKeyState(k).map(|s| s.0 & CoreVirtualKeyStates::Down.0 != 0).unwrap_or(false);
        keys::Mods { shift: down(VirtualKey::Shift), ctrl: down(VirtualKey::Control), alt: down(VirtualKey::Menu) }
    };
    let alt_down = Arc::new(AtomicBool::new(false));
    {
        let term = term.clone();
        let alt_down = alt_down.clone();
        window.KeyDown(&TypedEventHandler::<CoreWindow, KeyEventArgs>::new(move |w, args| {
            let (Some(w), Ok(args)) = (w.as_ref(), args.ok()) else { return Ok(()) };
            let m = mods(w);
            alt_down.store(m.alt, Ordering::SeqCst);
            let app_cursor = term.lock().unwrap().app_cursor;
            if let Some(bytes) = keys::key_down(args.VirtualKey()?.0, m, app_cursor) {
                send(&bytes);
                args.SetHandled(true)?;
            }
            Ok(())
        }))?;
    }
    window.CharacterReceived(&TypedEventHandler::<CoreWindow, CharacterReceivedEventArgs>::new(move |w, args| {
        let (Some(w), Ok(args)) = (w.as_ref(), args.ok()) else { return Ok(()) };
        let code = args.KeyCode()?;
        if keys::handled_on_key_down(code) {
            return Ok(());
        }
        // UTF-16 surrogate halves arrive separately; keep the high one
        static HIGH: Mutex<Option<u32>> = Mutex::new(None);
        let c = if (0xd800..0xdc00).contains(&code) {
            *HIGH.lock().unwrap() = Some(code);
            return Ok(());
        } else if (0xdc00..0xe000).contains(&code) {
            match HIGH.lock().unwrap().take() {
                Some(hi) => char::from_u32(0x10000 + ((hi - 0xd800) << 10) + (code - 0xdc00)),
                None => None,
            }
        } else {
            char::from_u32(code)
        };
        if let Some(c) = c {
            send(&keys::character(c, mods(w)));
        }
        Ok(())
    }))?;
    Ok(())
}

/// Shows a line of the app's own text in the terminal.
fn say(term: &Mutex<Term>, s: &str) {
    log!("{s}");
    let mut t = term.lock().unwrap();
    t.feed(b"\x1b[33m");
    t.feed(s.as_bytes());
    t.feed(b"\x1b[0m\r\n");
}

fn session(term: Arc<Mutex<Term>>, rows: u16, cols: u16) {
    let Some(local) = local_state() else {
        say(&term, "no LocalState folder");
        return;
    };
    let root = local.join("root");
    let tar = local.join("rootfs.tar");
    if tar.exists() {
        say(&term, &format!("unpacking {} ...", tar.display()));
        match blink_xbox::win::rootfs::extract(&tar, &root) {
            Ok(st) => {
                say(&term, &format!("unpacked: {} files, {} directories, {} symlinks", st.files, st.dirs, st.links));
                let _ = std::fs::remove_file(&tar);
            }
            Err(e) => {
                say(&term, &format!("unpacking failed: {e}"));
                return;
            }
        }
    }
    if !root.join("bin").exists() {
        say(&term, "no Linux root filesystem yet: push one as rootfs.tar (tools/xbox.py push) and relaunch");
        return;
    }
    let argv = command(&local);
    say(&term, &format!("starting {}", argv.join(" ")));
    let env: Vec<String> = [
        "HOME=/root",
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "TERM=xterm-256color",
        "COLORTERM=truecolor",
        "LANG=C.UTF-8",
        "USER=root",
        "SHELL=/bin/bash",
        "DISABLE_AUTOUPDATER=1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out_term = term.clone();
    let output = Box::new(move |bytes: &[u8]| {
        let replies = {
            let mut t = out_term.lock().unwrap();
            t.feed(bytes);
            // until blink's terminal exists, replies wait in the buffer
            if INPUT.get().is_some() { std::mem::take(&mut t.responses) } else { Vec::new() }
        };
        if !replies.is_empty() {
            send(&replies);
        }
    });
    let s = blink_xbox::start(&root.to_string_lossy(), argv, env, output, rows, cols);
    let _ = INPUT.set(s.terminal.clone());
    // replies to anything the guest asked before INPUT was set
    let early = std::mem::take(&mut term.lock().unwrap().responses);
    send(&early);
    let code = s.wait();
    say(&term, &format!("[exited with status {code}]"));
}

fn command(local: &Path) -> Vec<String> {
    std::fs::read_to_string(local.join("command.txt"))
        .ok()
        .map(|t| t.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec!["/bin/bash".into(), "-l".into()])
}
