//! errno: ucrt's thread-local errno, with ucrt's numbers (win/include/errno.h
//! adds names ucrt lacks, from 200 up). blink translates them to Linux's.

#![allow(dead_code)] // the full table, for reference

use std::ffi::c_int;

pub const EPERM: c_int = 1;
pub const ENOENT: c_int = 2;
pub const ESRCH: c_int = 3;
pub const EINTR: c_int = 4;
pub const EIO: c_int = 5;
pub const ENXIO: c_int = 6;
pub const E2BIG: c_int = 7;
pub const ENOEXEC: c_int = 8;
pub const EBADF: c_int = 9;
pub const ECHILD: c_int = 10;
pub const EAGAIN: c_int = 11;
pub const ENOMEM: c_int = 12;
pub const EACCES: c_int = 13;
pub const EFAULT: c_int = 14;
pub const EBUSY: c_int = 16;
pub const EEXIST: c_int = 17;
pub const EXDEV: c_int = 18;
pub const ENODEV: c_int = 19;
pub const ENOTDIR: c_int = 20;
pub const EISDIR: c_int = 21;
pub const EINVAL: c_int = 22;
pub const ENFILE: c_int = 23;
pub const EMFILE: c_int = 24;
pub const ENOTTY: c_int = 25;
pub const EFBIG: c_int = 27;
pub const ENOSPC: c_int = 28;
pub const ESPIPE: c_int = 29;
pub const EROFS: c_int = 30;
pub const EMLINK: c_int = 31;
pub const EPIPE: c_int = 32;
pub const ERANGE: c_int = 34;
pub const EDEADLK: c_int = 36;
pub const ENAMETOOLONG: c_int = 38;
pub const ENOSYS: c_int = 40;
pub const ENOTEMPTY: c_int = 41;
pub const EADDRINUSE: c_int = 100;
pub const EADDRNOTAVAIL: c_int = 101;
pub const EAFNOSUPPORT: c_int = 102;
pub const EALREADY: c_int = 103;
pub const ECONNABORTED: c_int = 106;
pub const ECONNREFUSED: c_int = 107;
pub const ECONNRESET: c_int = 108;
pub const EDESTADDRREQ: c_int = 109;
pub const EHOSTUNREACH: c_int = 110;
pub const EINPROGRESS: c_int = 112;
pub const EISCONN: c_int = 113;
pub const ELOOP: c_int = 114;
pub const EMSGSIZE: c_int = 115;
pub const ENETDOWN: c_int = 116;
pub const ENETRESET: c_int = 117;
pub const ENETUNREACH: c_int = 118;
pub const ENOBUFS: c_int = 119;
pub const ENOPROTOOPT: c_int = 123;
pub const ENOTCONN: c_int = 126;
pub const ENOTSOCK: c_int = 128;
pub const ENOTSUP: c_int = 129;
pub const EOPNOTSUPP: c_int = 130;
pub const EOVERFLOW: c_int = 132;
pub const EPROTO: c_int = 134;
pub const EPROTONOSUPPORT: c_int = 135;
pub const EPROTOTYPE: c_int = 136;
pub const ETIMEDOUT: c_int = 138;

extern "C" {
    fn _errno() -> *mut c_int;
}

pub fn set(e: c_int) {
    // SAFETY: ucrt returns this thread's errno slot.
    unsafe { *_errno() = e }
}

pub fn get() -> c_int {
    // SAFETY: as above.
    unsafe { *_errno() }
}

/// Sets errno and returns -1, the usual POSIX failure.
pub fn fail(e: c_int) -> c_int {
    set(e);
    -1
}

pub fn fail_isize(e: c_int) -> isize {
    set(e);
    -1
}

pub fn fail_i64(e: c_int) -> i64 {
    set(e);
    -1
}

/// Translates a Win32 error code (GetLastError) to errno.
pub fn from_win32(code: u32) -> c_int {
    match code {
        2 | 3 | 15 | 123 | 161 => ENOENT, // file/path not found, bad drive/name/path
        5 | 19 | 1314 => EACCES,          // access denied, write protect, privilege
        6 => EBADF,
        8 | 14 => ENOMEM,
        17 => EXDEV,
        18 => ENOENT, // no more files
        32 | 33 => EBUSY,
        38 => 0, // EOF
        39 | 112 => ENOSPC,
        50 => ENOTSUP,
        80 | 183 => EEXIST,
        87 | 131 => EINVAL,
        109 | 232 | 233 => EPIPE, // broken pipe, pipe closing, not connected
        120 => ENOSYS,
        145 => ENOTEMPTY,
        206 => ENAMETOOLONG,
        267 => ENOTDIR,
        995 => EINTR,
        1921 => ELOOP,
        _ => EIO,
    }
}

/// Sets errno from GetLastError() and returns -1.
pub fn fail_last() -> c_int {
    // SAFETY: plain Win32 call.
    fail(from_win32(unsafe { windows_sys::Win32::Foundation::GetLastError() }))
}
