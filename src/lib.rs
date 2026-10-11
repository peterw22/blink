//! Rust front end for the C blink x86-64 Linux emulator.
//!
//! The emulator itself (decoder, interpreter, JIT, MMU, Linux syscall
//! emulation) stays in C. Rust drives it and replaces the pieces that can't
//! work inside a UWP app on the Xbox: host `fork`/`exec` (guest processes run
//! as threads, see `procs`) and the POSIX host layer (`win`).

use std::ffi::{c_char, c_int, CString};

mod procs;
#[cfg(windows)]
pub mod win;

extern "C" {
    /// blink's `main()` from blink/blink.c, renamed by build.rs.
    fn blink_c_main(argc: c_int, argv: *mut *mut c_char, envp: *mut *mut c_char) -> c_int;
}

fn c_strings<I, S>(items: I) -> Vec<CString>
where
    I: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
{
    items.into_iter().map(|a| CString::new(a).expect("argument contains a NUL byte")).collect()
}

fn c_vector(strings: &[CString]) -> Vec<*mut c_char> {
    let mut v: Vec<*mut c_char> = strings.iter().map(|s| s.as_ptr() as *mut c_char).collect();
    v.push(std::ptr::null_mut());
    v
}

/// Runs blink with C-style arguments (`args[0]` is the emulator's own name,
/// then blink flags, then the guest program and its arguments) and the given
/// guest environment. Returns the guest's exit status; blink may also exit
/// the process directly.
pub fn run_with_env<A, S, E, T>(args: A, env: E) -> i32
where
    A: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
    E: IntoIterator<Item = T>,
    T: Into<Vec<u8>>,
{
    let args = c_strings(args);
    let env = c_strings(env);
    let mut argv = c_vector(&args);
    let mut envp = c_vector(&env);
    // SAFETY: argv and envp are NUL-terminated and their strings outlive the
    // call; blink only reads them (it copies them onto the guest stack).
    unsafe { blink_c_main((argv.len() - 1) as c_int, argv.as_mut_ptr(), envp.as_mut_ptr()) }
}

/// Runs blink with this process's environment (see `run_with_env`).
#[cfg(unix)]
pub fn run<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
{
    use std::os::unix::ffi::OsStringExt;
    run_with_env(args, std::env::vars_os().map(|(k, v)| {
        let mut kv = k.into_vec();
        kv.push(b'=');
        kv.extend(v.into_vec());
        kv
    }))
}

/// Like `run()`, but guest processes created by fork()/vfork() run as
/// threads of this host process instead of host processes, the way they
/// must on the Xbox. Forces blink's non-linear memory mode (`-m`).
#[cfg(unix)]
pub fn run_in_process<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
{
    let mut args: Vec<Vec<u8>> = args.into_iter().map(Into::into).collect();
    args.insert(1.min(args.len()), b"-m".to_vec());
    procs::install();
    run(args)
}

/// Runs blink on Windows, where guest processes can only run in-process
/// (blink's non-linear memory, `-m`, is forced). The guest gets `env` as its
/// environment; the host's Windows environment means nothing to it.
#[cfg(windows)]
pub fn run<A, S, E, T>(args: A, env: E) -> i32
where
    A: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
    E: IntoIterator<Item = T>,
    T: Into<Vec<u8>>,
{
    win::init();
    let mut args: Vec<Vec<u8>> = args.into_iter().map(Into::into).collect();
    args.insert(1.min(args.len()), b"-m".to_vec());
    procs::install();
    run_with_env(args, env)
}

/// A guest running in this app (Windows): its terminal and its thread.
#[cfg(windows)]
pub struct Session {
    /// keyboard input in (`push_input`), resizes (`set_winsize`)
    pub terminal: std::sync::Arc<win::Tty>,
    /// blink's thread (it ends inside blink; use `wait`)
    pub thread: std::thread::JoinHandle<i32>,
}

#[cfg(windows)]
impl Session {
    /// Waits for the guest's first process to exit; its exit status.
    pub fn wait(&self) -> i32 {
        procs::wait_root_exit()
    }
}

#[cfg(windows)]
extern "C" {
    fn SetEmbedded(on: bool);
}

/// Runs `argv` (a path inside the guest, then its arguments) in the Linux
/// root filesystem at `root`, on a new thread. Everything the guest writes
/// to its terminal goes to `output`.
#[cfg(windows)]
pub fn start(root: &str, argv: Vec<String>, env: Vec<String>, output: Box<dyn Fn(&[u8]) + Send + Sync>, rows: u16, cols: u16) -> Session {
    // the app outlives the guest
    unsafe { SetEmbedded(true) };
    let terminal = win::init_terminal(root, output, rows, cols);
    *terminal.on_signal.lock().unwrap() = Some(Box::new(procs::signal_foreground));
    procs::install();
    let mut args = vec!["blink".to_string(), "-m".into(), "-C".into(), root.to_string()];
    args.extend(argv);
    let thread = std::thread::Builder::new()
        .name("blink".into())
        .stack_size(16 << 20)
        .spawn(move || run_with_env(args, env))
        .expect("failed to start blink's thread");
    Session { terminal, thread }
}
