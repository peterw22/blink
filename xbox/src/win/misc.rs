//! Process identity, limits, system information, randomness, and a few
//! string functions ucrt lacks.

use super::errno;
use std::ffi::{c_char, c_int, c_void};
use std::sync::Mutex;
use windows_sys::Win32::Security::Cryptography::BCryptGenRandom;
use windows_sys::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};

// The host process is "root": ids mean nothing on the Xbox, and a root
// guest can do what it likes inside its own rootfs.
#[unsafe(no_mangle)]
pub extern "C" fn getuid() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn geteuid() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn getgid() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn getegid() -> u32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn setuid(_u: u32) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn setgid(_g: u32) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn seteuid(_u: u32) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn setegid(_g: u32) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn getgroups(_n: c_int, _list: *mut u32) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn getpid() -> c_int {
    std::process::id() as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn getppid() -> c_int {
    1
}

// Guest process groups and sessions are in procs.rs; these answer for the
// host process itself.
#[unsafe(no_mangle)]
pub extern "C" fn getpgid(_pid: c_int) -> c_int {
    getpid()
}
#[unsafe(no_mangle)]
pub extern "C" fn getsid(_pid: c_int) -> c_int {
    getpid()
}
#[unsafe(no_mangle)]
pub extern "C" fn setsid() -> c_int {
    errno::fail(errno::EPERM)
}
#[unsafe(no_mangle)]
pub extern "C" fn setpgid(_p: c_int, _g: c_int) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn getpriority(_w: c_int, _who: u32) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn setpriority(_w: c_int, _who: u32, _p: c_int) -> c_int {
    0
}

// There are no host processes to create or wait for: blink's in-process
// guest processes (procs.rs) replace all of these.
#[unsafe(no_mangle)]
pub extern "C" fn fork() -> c_int {
    errno::fail(errno::ENOSYS)
}
#[unsafe(no_mangle)]
pub extern "C" fn execve(_p: *const c_char, _a: *const c_void, _e: *const c_void) -> c_int {
    errno::fail(errno::ENOSYS)
}
#[unsafe(no_mangle)]
pub extern "C" fn execv(_p: *const c_char, _a: *const c_void) -> c_int {
    errno::fail(errno::ENOSYS)
}
#[unsafe(no_mangle)]
pub extern "C" fn waitpid(_p: c_int, _s: *mut c_int, _o: c_int) -> c_int {
    errno::fail(errno::ECHILD)
}

#[repr(C)]
pub struct Rlimit {
    cur: u64,
    max: u64,
}

const RLIM_INFINITY: u64 = !0;
static LIMITS: Mutex<[(u64, u64); 16]> = Mutex::new([(RLIM_INFINITY, RLIM_INFINITY); 16]);

fn init_limits(l: &mut [(u64, u64); 16]) {
    if l[7].0 == RLIM_INFINITY {
        l[3] = (8 << 20, RLIM_INFINITY); // RLIMIT_STACK
        l[7] = (1024, super::fd::OPEN_MAX as u64); // RLIMIT_NOFILE
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn getrlimit(res: c_int, r: *mut Rlimit) -> c_int {
    let mut l = LIMITS.lock().unwrap();
    init_limits(&mut l);
    let Some(&(cur, max)) = l.get(res as usize) else { return errno::fail(errno::EINVAL) };
    unsafe { *r = Rlimit { cur, max } };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn setrlimit(res: c_int, r: *const Rlimit) -> c_int {
    let mut l = LIMITS.lock().unwrap();
    init_limits(&mut l);
    let Some(slot) = l.get_mut(res as usize) else { return errno::fail(errno::EINVAL) };
    *slot = unsafe { ((*r).cur, (*r).max) };
    0
}

fn total_memory() -> (u64, u64) {
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut m) };
    (m.ullTotalPhys, m.ullAvailPhys)
}

#[unsafe(no_mangle)]
pub extern "C" fn sysconf(name: c_int) -> i32 {
    match name {
        30 => 4096,                                            // _SC_PAGESIZE
        83 | 84 => super::thread::ncpu() as i32,               // _SC_NPROCESSORS_*
        2 => 100,                                              // _SC_CLK_TCK
        4 => 1024,                                             // _SC_OPEN_MAX
        0 => 2 << 20,                                          // _SC_ARG_MAX
        85 => (total_memory().0 / 4096).min(i32::MAX as u64) as i32, // _SC_PHYS_PAGES
        3 => 65536,                                            // _SC_NGROUPS_MAX
        70 => 1024,                                            // _SC_GETPW_R_SIZE_MAX
        180 => 64,                                             // _SC_HOST_NAME_MAX
        _ => errno::fail(errno::EINVAL),
    }
}

#[repr(C)]
pub struct Sysinfo {
    uptime: i64,
    loads: [u64; 3],
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    pad: u16,
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sysinfo(si: *mut Sysinfo) -> c_int {
    let (total, avail) = total_memory();
    unsafe {
        std::ptr::write_bytes(si, 0, 1);
        (*si).uptime = (GetTickCount64() / 1000) as i64;
        (*si).totalram = total;
        (*si).freeram = avail;
        (*si).procs = 1;
        (*si).mem_unit = 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn getrandom(buf: *mut u8, n: usize, _flags: u32) -> isize {
    // BCRYPT_USE_SYSTEM_PREFERRED_RNG
    let mut done = 0;
    while done < n {
        let k = (n - done).min(u32::MAX as usize);
        if unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.add(done), k as u32, 2) } != 0 {
            return errno::fail_isize(errno::EIO);
        }
        done += k;
    }
    n as isize
}

extern "C" {
    fn malloc(n: usize) -> *mut c_void;
    fn strlen(s: *const c_char) -> usize;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strndup(s: *const c_char, n: usize) -> *mut c_char {
    let mut len = 0;
    while len < n && unsafe { *s.add(len) } != 0 {
        len += 1;
    }
    let p = unsafe { malloc(len + 1) as *mut c_char };
    if p.is_null() {
        errno::set(errno::ENOMEM);
        return p;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s, p, len);
        *p.add(len) = 0;
    }
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strdup(s: *const c_char) -> *mut c_char {
    unsafe { strndup(s, usize::MAX) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn stpcpy(d: *mut c_char, s: *const c_char) -> *mut c_char {
    let n = unsafe { strlen(s) };
    unsafe {
        std::ptr::copy_nonoverlapping(s, d, n + 1);
        d.add(n)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strtok_r(s: *mut c_char, delim: *const c_char, save: *mut *mut c_char) -> *mut c_char {
    let is_delim = |c: c_char| {
        let mut d = delim;
        while unsafe { *d } != 0 {
            if unsafe { *d } == c {
                return true;
            }
            d = unsafe { d.add(1) };
        }
        false
    };
    let mut p = if s.is_null() { unsafe { *save } } else { s };
    if p.is_null() {
        return p;
    }
    while unsafe { *p } != 0 && is_delim(unsafe { *p }) {
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } == 0 {
        unsafe { *save = p };
        return std::ptr::null_mut();
    }
    let tok = p;
    while unsafe { *p } != 0 && !is_delim(unsafe { *p }) {
        p = unsafe { p.add(1) };
    }
    if unsafe { *p } != 0 {
        unsafe {
            *p = 0;
            *save = p.add(1);
        }
    } else {
        unsafe { *save = p };
    }
    tok
}
