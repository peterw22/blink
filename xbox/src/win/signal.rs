//! POSIX signals, emulated in-process (Windows has none). Handlers and masks
//! are bookkeeping; raise() runs a handler on the calling thread. blink's
//! real use of host signals on Linux is waking blocked threads, which
//! pthread_kill() does here (see thread.rs).

use super::{errno, thread};
use std::ffi::{c_int, c_void};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

pub const SIGKILL: c_int = 9;
pub const SIGSTOP: c_int = 19;
const NSIG: usize = 65;
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;
const SA_SIGINFO: c_int = 4;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigSet {
    sig: [u64; 1],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigAction {
    handler: usize,
    mask: SigSet,
    flags: c_int,
}

#[repr(C)]
struct SigInfo {
    signo: c_int,
    errno: c_int,
    code: c_int,
    pid: c_int,
    uid: u32,
    addr: *mut c_void,
    status: c_int,
    value: *mut c_void,
}

static ACTIONS: Mutex<[SigAction; NSIG]> =
    Mutex::new([SigAction { handler: SIG_DFL, mask: SigSet { sig: [0] }, flags: 0 }; NSIG]);

fn valid(sig: c_int) -> bool {
    (1..NSIG as c_int).contains(&sig)
}

fn bit(sig: c_int) -> u64 {
    1u64 << (sig - 1)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigemptyset(s: *mut SigSet) -> c_int {
    unsafe { (*s).sig[0] = 0 };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigfillset(s: *mut SigSet) -> c_int {
    unsafe { (*s).sig[0] = !0 };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaddset(s: *mut SigSet, sig: c_int) -> c_int {
    if !valid(sig) {
        return errno::fail(errno::EINVAL);
    }
    unsafe { (*s).sig[0] |= bit(sig) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigdelset(s: *mut SigSet, sig: c_int) -> c_int {
    if !valid(sig) {
        return errno::fail(errno::EINVAL);
    }
    unsafe { (*s).sig[0] &= !bit(sig) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigismember(s: *const SigSet, sig: c_int) -> c_int {
    if !valid(sig) {
        return errno::fail(errno::EINVAL);
    }
    (unsafe { (*s).sig[0] } & bit(sig) != 0) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaction(sig: c_int, act: *const SigAction, old: *mut SigAction) -> c_int {
    if !valid(sig) {
        return errno::fail(errno::EINVAL);
    }
    let mut a = ACTIONS.lock().unwrap();
    if !old.is_null() {
        unsafe { *old = a[sig as usize] };
    }
    if !act.is_null() {
        if sig == SIGKILL || sig == SIGSTOP {
            return errno::fail(errno::EINVAL);
        }
        a[sig as usize] = unsafe { *act };
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_signal(sig: c_int, handler: usize) -> usize {
    let act = SigAction { handler, mask: SigSet { sig: [0] }, flags: 0 };
    let mut old = act;
    // SAFETY: both point to locals
    if unsafe { sigaction(sig, &act, &mut old) } == -1 {
        return usize::MAX; // SIG_ERR
    }
    old.handler
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_sigmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int {
    let t = thread::current();
    let cur = t.sigmask.load(Ordering::Relaxed);
    if !old.is_null() {
        unsafe { (*old).sig[0] = cur };
    }
    if !set.is_null() {
        let s = unsafe { (*set).sig[0] };
        let new = match how {
            0 => cur | s,  // SIG_BLOCK
            1 => cur & !s, // SIG_UNBLOCK
            2 => s,        // SIG_SETMASK
            _ => return errno::EINVAL,
        };
        t.sigmask.store(new, Ordering::Relaxed);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigprocmask(how: c_int, set: *const SigSet, old: *mut SigSet) -> c_int {
    match unsafe { pthread_sigmask(how, set, old) } {
        0 => 0,
        e => errno::fail(e),
    }
}

/// Waits for an interruption (pthread_kill), as a signal would end it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigsuspend(_mask: *const SigSet) -> c_int {
    let _ = thread::wait_for::<()>(None, || None);
    errno::fail(errno::EINTR)
}

#[unsafe(no_mangle)]
pub extern "C" fn pause() -> c_int {
    let _ = thread::wait_for::<()>(None, || None);
    errno::fail(errno::EINTR)
}

/// Runs `sig`'s disposition on the calling thread.
#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_raise(sig: c_int) -> c_int {
    if !valid(sig) {
        return errno::fail(errno::EINVAL);
    }
    let a = ACTIONS.lock().unwrap()[sig as usize];
    match a.handler {
        SIG_IGN => {}
        SIG_DFL => match sig {
            // ignored by default: SIGCHLD SIGCONT SIGURG SIGWINCH
            17 | 18 | 23 | 28 => {}
            _ => {
                eprintln!("blink-xbox: terminated by signal {sig}");
                std::process::exit(128 + sig);
            }
        },
        h if a.flags & SA_SIGINFO != 0 => {
            let mut si = SigInfo {
                signo: sig,
                errno: 0,
                code: -6,
                pid: std::process::id() as c_int,
                uid: 0,
                addr: std::ptr::null_mut(),
                status: 0,
                value: std::ptr::null_mut(),
            };
            // SAFETY: a handler blink installed with SA_SIGINFO
            let f: extern "C" fn(c_int, *mut SigInfo, *mut c_void) = unsafe { std::mem::transmute(h) };
            f(sig, &mut si, std::ptr::null_mut());
        }
        h => {
            // SAFETY: a handler blink installed
            let f: extern "C" fn(c_int) = unsafe { std::mem::transmute(h) };
            f(sig);
        }
    }
    0
}

/// Only this process exists on the host; guest kill() goes to the
/// process table (procs.rs) instead.
#[unsafe(no_mangle)]
pub extern "C" fn kill(pid: c_int, sig: c_int) -> c_int {
    let me = std::process::id() as c_int;
    if pid != me && pid != 0 && pid != -1 {
        return errno::fail(errno::ESRCH);
    }
    if sig == 0 { 0 } else { blinkwin_raise(sig) }
}

#[unsafe(no_mangle)]
pub extern "C" fn __libc_current_sigrtmin() -> c_int {
    34
}

#[unsafe(no_mangle)]
pub extern "C" fn __libc_current_sigrtmax() -> c_int {
    64
}
