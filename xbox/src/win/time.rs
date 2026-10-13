//! Clocks, sleeping and calendar time.

use super::{errno, thread};
use std::ffi::{c_int, c_void};
use std::sync::OnceLock;
use std::time::Duration;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes};

/// ucrt's struct timespec: 64-bit seconds, 32-bit nanoseconds (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

const UNIX_EPOCH_AS_FILETIME: i64 = 116_444_736_000_000_000;

fn filetime_100ns(ft: FILETIME) -> i64 {
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

/// Nanoseconds since the Unix epoch.
pub fn realtime_ns() -> i64 {
    let mut ft = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    // SAFETY: plain Win32 call
    unsafe { GetSystemTimePreciseAsFileTime(&mut ft) };
    (filetime_100ns(ft) - UNIX_EPOCH_AS_FILETIME) * 100
}

/// Nanoseconds on a monotonic clock.
pub fn monotonic_ns() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0;
        // SAFETY: plain Win32 call
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    });
    let mut c = 0;
    // SAFETY: plain Win32 call
    unsafe { QueryPerformanceCounter(&mut c) };
    ((c as i128 * 1_000_000_000) / freq as i128) as i64
}

fn cpu_ns(thread: bool) -> i64 {
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut a, mut b, mut k, mut u) = (z, z, z, z);
    // SAFETY: plain Win32 calls on pseudo-handles
    unsafe {
        if thread {
            GetThreadTimes(GetCurrentThread(), &mut a, &mut b, &mut k, &mut u);
        } else {
            GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
        }
    }
    (filetime_100ns(k) + filetime_100ns(u)) * 100
}

fn to_ts(ns: i64) -> Timespec {
    Timespec { tv_sec: ns.div_euclid(1_000_000_000), tv_nsec: ns.rem_euclid(1_000_000_000) as i32 }
}

fn clock_ns(clock: c_int) -> Option<i64> {
    Some(match clock {
        0 | 5 => realtime_ns(),                // REALTIME(_COARSE)
        1 | 4 | 6 | 7 => monotonic_ns(),       // MONOTONIC(_RAW/_COARSE), BOOTTIME
        2 => cpu_ns(false),                    // PROCESS_CPUTIME_ID
        3 => cpu_ns(true),                     // THREAD_CPUTIME_ID
        _ => return None,
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int {
    match clock_ns(clock) {
        Some(ns) => {
            unsafe { *ts = to_ts(ns) };
            0
        }
        None => errno::fail(errno::EINVAL),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_getres(clock: c_int, ts: *mut Timespec) -> c_int {
    if clock_ns(clock).is_none() {
        return errno::fail(errno::EINVAL);
    }
    if !ts.is_null() {
        unsafe { *ts = Timespec { tv_sec: 0, tv_nsec: 100 } };
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn clock_settime(_clock: c_int, _ts: *const Timespec) -> c_int {
    errno::fail(errno::EPERM)
}

fn sleep_ns(ns: i64, rem: *mut Timespec) -> c_int {
    let start = monotonic_ns();
    match thread::sleep(Duration::from_nanos(ns.max(0) as u64)) {
        Ok(()) => 0,
        Err(e) => {
            if !rem.is_null() {
                // SAFETY: caller's out-parameter
                unsafe { *rem = to_ts((ns - (monotonic_ns() - start)).max(0)) };
            }
            e
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> c_int {
    let r = unsafe { *req };
    if r.tv_nsec < 0 || r.tv_nsec >= 1_000_000_000 || r.tv_sec < 0 {
        return errno::fail(errno::EINVAL);
    }
    match sleep_ns(r.tv_sec * 1_000_000_000 + r.tv_nsec as i64, rem) {
        0 => 0,
        e => errno::fail(e),
    }
}

/// Returns an error number rather than setting errno, like POSIX.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_nanosleep(clock: c_int, flags: c_int, req: *const Timespec, rem: *mut Timespec) -> c_int {
    let Some(now) = clock_ns(clock) else { return errno::EINVAL };
    let r = unsafe { *req };
    let ns = r.tv_sec * 1_000_000_000 + r.tv_nsec as i64;
    let ns = if flags & 1 != 0 { ns - now } else { ns };
    sleep_ns(ns, if flags & 1 != 0 { std::ptr::null_mut() } else { rem })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gettimeofday(tv: *mut Timeval, _tz: *mut c_void) -> c_int {
    let ns = realtime_ns();
    if !tv.is_null() {
        unsafe { *tv = Timeval { tv_sec: ns.div_euclid(1_000_000_000), tv_usec: ns.rem_euclid(1_000_000_000) / 1000 } };
    }
    0
}

extern "C" {
    fn _gmtime64_s(tm: *mut c_void, t: *const i64) -> c_int;
    fn _localtime64_s(tm: *mut c_void, t: *const i64) -> c_int;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gmtime_r(t: *const i64, tm: *mut c_void) -> *mut c_void {
    if unsafe { _gmtime64_s(tm, t) } != 0 { std::ptr::null_mut() } else { tm }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn localtime_r(t: *const i64, tm: *mut c_void) -> *mut c_void {
    if unsafe { _localtime64_s(tm, t) } != 0 { std::ptr::null_mut() } else { tm }
}

#[repr(C)]
pub struct Itimerval {
    interval: Timeval,
    value: Timeval,
}

/// Interval timers would deliver SIGALRM to the host process, which has
/// no use for it with guests running in-process; report them as unarmed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setitimer(_which: c_int, _new: *const Itimerval, old: *mut Itimerval) -> c_int {
    if !old.is_null() {
        unsafe { std::ptr::write_bytes(old, 0, 1) };
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn getitimer(_which: c_int, cur: *mut Itimerval) -> c_int {
    unsafe { std::ptr::write_bytes(cur, 0, 1) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn alarm(_secs: u32) -> u32 {
    0
}

#[repr(C)]
pub struct Tms {
    utime: i64,
    stime: i64,
    cutime: i64,
    cstime: i64,
}

/// Clock ticks of 1/100 s (sysconf(_SC_CLK_TCK)).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn times(t: *mut Tms) -> i64 {
    if !t.is_null() {
        let cpu = cpu_ns(false) / 10_000_000;
        unsafe { *t = Tms { utime: cpu, stime: 0, cutime: 0, cstime: 0 } };
    }
    monotonic_ns() / 10_000_000
}

#[repr(C)]
pub struct Rusage {
    utime: Timeval,
    stime: Timeval,
    rest: [i64; 14],
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn getrusage(who: c_int, r: *mut Rusage) -> c_int {
    let ns = if who == 1 { cpu_ns(true) } else { cpu_ns(false) };
    unsafe {
        std::ptr::write_bytes(r, 0, 1);
        (*r).utime = Timeval { tv_sec: ns / 1_000_000_000, tv_usec: ns % 1_000_000_000 / 1000 };
    }
    0
}
