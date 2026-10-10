//! Rust front end for the C blink x86-64 Linux emulator.
//!
//! The emulator itself (decoder, interpreter, JIT, MMU, Linux syscall
//! emulation) stays in C. Rust drives it and, for the Xbox, will replace the
//! pieces that can't work inside a UWP app: host `fork`/`exec` (guest
//! processes become in-process structs on threads) and the POSIX host layer.

use std::ffi::{c_char, c_int, CString};

#[cfg(target_os = "linux")]
extern "C" {
    /// blink's `main()` from blink/blink.c, renamed by build.rs.
    fn blink_c_main(argc: c_int, argv: *mut *mut c_char, envp: *mut *mut c_char) -> c_int;
}

/// Runs blink with C-style arguments (`args[0]` is the emulator's own name,
/// then blink flags, then the guest program and its arguments). Returns the
/// guest's exit status. blink may also exit the process directly.
#[cfg(target_os = "linux")]
pub fn run<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
{
    let owned: Vec<CString> = args
        .into_iter()
        .map(|a| CString::new(a).expect("argument contains a NUL byte"))
        .collect();
    let mut argv: Vec<*mut c_char> = owned.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    extern "C" {
        static mut environ: *mut *mut c_char;
    }
    // SAFETY: argv is NUL-terminated and its strings outlive the call; blink
    // only reads argv/envp (it copies them onto the guest stack).
    unsafe { blink_c_main((argv.len() - 1) as c_int, argv.as_mut_ptr(), environ) }
}

#[cfg(not(target_os = "linux"))]
pub fn run<I, S>(_args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<Vec<u8>>,
{
    unimplemented!("the Windows host layer for blink is not written yet")
}
