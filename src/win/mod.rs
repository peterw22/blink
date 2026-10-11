//! POSIX host layer for blink on Windows (the Xbox's UWP app and, for
//! testing, a plain Windows exe under Wine).
//!
//! blink's C is compiled against the POSIX headers in win/include (Linux
//! numbering for flags and structs) and calls the functions defined here.
//! Functions that aren't implemented yet are in `stubs` and abort with
//! their name, so each test run says what to write next.

/// BLINKWIN_TRACE=1 logs host-layer calls to stderr (see `trace!`).
pub(crate) fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BLINKWIN_TRACE").is_some_and(|v| v == "1"))
}

macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::win::tracing() {
            eprintln!("blinkwin: {}", format_args!($($arg)*));
        }
    };
}
pub(crate) use trace;

mod epoll;
mod errno;
mod fd;
mod fs;
mod io;
mod mem;
mod misc;
mod net;
pub mod rootfs;
mod signal;
mod stubs;
mod thread;
mod time;



pub use io::Tty;

/// Prepares the host layer; call before running blink. Descriptors 0-2
/// are the host process's standard handles.
pub fn init() {
    fs::init();
    io::init_std();
    net::init();
}

/// Prepares the host layer for an app without standard handles: the guest
/// starts in `root`, and descriptors 0-2 are a terminal that sends
/// `output` everything written to it.
pub fn init_terminal(root: &str, output: Box<dyn Fn(&[u8]) + Send + Sync>, rows: u16, cols: u16) -> std::sync::Arc<Tty> {
    fs::init();
    fs::set_cwd_path(root);
    // Windows has no /dev for blink's devfs to show
    fs::make_dev_dir(&format!("{}\\dev", root.trim_end_matches(['\\', '/'])));
    net::init();
    let tty = Tty::new(output, rows, cols);
    fs::set_terminal(tty.clone());
    for n in 0..3 {
        fd::set(n, fd::OpenFile::new(fd::Obj::Tty(tty.clone()), fd::O_RDWR), false);
    }
    tty
}

/// Called by a POSIX function that isn't implemented yet.
pub(crate) fn missing(name: &str) -> ! {
    eprintln!("blink-xbox: POSIX function {name}() isn't implemented on Windows yet");
    std::process::abort()
}
