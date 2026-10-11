//! mmap and friends on VirtualAlloc and file mapping views.
//!
//! blink in non-linear mode only needs anonymous memory (its page pool and
//! JIT), private read-only file views (the ELF loader) and, rarely, shared
//! mappings. Windows allocates at 64 KiB granularity and frees whole
//! allocations, so mappings are tracked here.

use super::errno;
use super::fd::{self, Obj};
use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::Memory::*;

const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const PROT_EXEC: c_int = 4;
const MAP_SHARED: c_int = 1;
const MAP_FIXED: c_int = 0x10;
const MAP_ANONYMOUS: c_int = 0x20;
const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;
const MADV_DONTNEED: c_int = 4;

enum Region {
    /// VirtualAlloc'd; `size` is the whole allocation
    Anon { size: usize },
    /// MapViewOfFile'd at `base`; the caller saw base + `skew`
    View { base: usize, size: usize },
}

static REGIONS: Mutex<BTreeMap<usize, Region>> = Mutex::new(BTreeMap::new());

fn page_prot(prot: c_int) -> u32 {
    match (prot & PROT_READ != 0, prot & PROT_WRITE != 0, prot & PROT_EXEC != 0) {
        (_, _, true) if prot & PROT_WRITE != 0 => PAGE_EXECUTE_READWRITE,
        (true, false, true) | (false, false, true) => PAGE_EXECUTE_READ,
        (_, true, false) => PAGE_READWRITE,
        (true, false, false) => PAGE_READONLY,
        _ => PAGE_NOACCESS,
    }
}

fn round_up(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fdn: c_int, off: i64) -> *mut c_void {
    if len == 0 {
        errno::set(errno::EINVAL);
        return MAP_FAILED;
    }
    let len = round_up(len, 4096);
    if flags & MAP_ANONYMOUS != 0 && flags & MAP_SHARED == 0 {
        return anon(addr, len, prot, flags);
    }
    // file (or shared anonymous) mappings: a section and a view
    let file = if flags & MAP_ANONYMOUS != 0 {
        None
    } else {
        match fd::get(fdn) {
            Ok(f) => match &f.obj {
                Obj::File { handle, .. } => Some(*handle),
                _ => {
                    errno::set(errno::ENODEV);
                    return MAP_FAILED;
                }
            },
            Err(e) => {
                errno::set(e);
                return MAP_FAILED;
            }
        }
    };
    let shared = flags & MAP_SHARED != 0;
    let writable = prot & PROT_WRITE != 0;
    // blink never runs code from file views (non-linear mode interprets or
    // JIT-copies guest code), and execute access needs FILE_EXECUTE handles
    let exec = false;
    // views start at 64 KiB boundaries; map from below and skew the result
    let gran = 65536i64;
    let base_off = off - off.rem_euclid(gran);
    let skew = (off - base_off) as usize;
    let mut view_len = len + skew;
    if let Some(h) = file {
        // views can't extend past the end of a file (Linux zero-fills the
        // rest of the last page; Windows maps whole pages, which does too)
        let mut size = 0i64;
        unsafe { windows_sys::Win32::Storage::FileSystem::GetFileSizeEx(h, &mut size) };
        if size <= base_off {
            errno::set(errno::ENXIO);
            return MAP_FAILED;
        }
        view_len = view_len.min((size - base_off) as usize);
    }
    let sec_prot = match (shared, writable || !shared, exec) {
        (_, _, true) => if shared && writable { PAGE_EXECUTE_READWRITE } else { PAGE_EXECUTE_WRITECOPY },
        (true, true, false) => PAGE_READWRITE,
        (false, _, false) => PAGE_WRITECOPY,
        (true, false, false) => PAGE_READONLY,
    };
    let max = (base_off as u64 + view_len as u64) as u64;
    let h: HANDLE = file.unwrap_or(-1isize as HANDLE);
    let (hi, lo) = if file.is_some() { (0, 0) } else { ((max >> 32) as u32, max as u32) };
    let _ = (hi, lo);
    let size = if file.is_some() { 0 } else { max };
    let section = unsafe { CreateFileMappingFromApp(h, std::ptr::null(), if file.is_none() { PAGE_READWRITE } else { sec_prot }, size, std::ptr::null()) };
    if section.is_null() {
        let e = unsafe { GetLastError() };
        super::trace!("mmap: CreateFileMapping(fd {fdn}, {len:#x} at {off:#x}) failed: {e}");
        errno::set(errno::from_win32(e));
        return MAP_FAILED;
    }
    let mut access = if shared {
        if writable { FILE_MAP_WRITE | FILE_MAP_READ } else { FILE_MAP_READ }
    } else {
        FILE_MAP_COPY
    };
    if exec {
        access |= FILE_MAP_EXECUTE;
    }
    // MAP_FIXED file views would need MapViewOfFile3FromApp; blink's
    // non-linear mode doesn't ask for them
    let _ = addr;
    let view = unsafe { MapViewOfFileFromApp(section, access, base_off as u64, view_len) };
    unsafe { CloseHandle(section) };
    if view.Value.is_null() {
        let e = unsafe { GetLastError() };
        super::trace!("mmap: MapViewOfFile(fd {fdn}, {view_len:#x} at {base_off:#x}) failed: {e}");
        errno::set(errno::from_win32(e));
        return MAP_FAILED;
    }
    let base = view.Value as usize;
    REGIONS.lock().unwrap().insert(base + skew, Region::View { base, size: view_len });
    (base + skew) as *mut c_void
}

fn anon(addr: *mut c_void, len: usize, prot: c_int, flags: c_int) -> *mut c_void {
    if flags & MAP_FIXED != 0 && !addr.is_null() {
        // inside one of our allocations: recommit (and zero) in place
        let a = addr as usize;
        let inside = {
            let r = REGIONS.lock().unwrap();
            r.range(..=a).next_back().is_some_and(|(&b, reg)| matches!(reg, Region::Anon { size } if a + len <= b + size))
        };
        if inside {
            unsafe {
                VirtualFree(addr, len, MEM_DECOMMIT);
                if VirtualAllocFromApp(addr, len, MEM_COMMIT, page_prot(prot)).is_null() {
                    errno::set(errno::ENOMEM);
                    return MAP_FAILED;
                }
            }
            return addr;
        }
    }
    let hint = if flags & MAP_FIXED != 0 { addr as *const c_void } else { std::ptr::null() };
    let commit = if prot == 0 { MEM_RESERVE } else { MEM_RESERVE | MEM_COMMIT };
    let p = unsafe { VirtualAllocFromApp(hint, len, commit, page_prot(prot)) };
    if p.is_null() {
        errno::set(errno::ENOMEM);
        return MAP_FAILED;
    }
    REGIONS.lock().unwrap().insert(p as usize, Region::Anon { size: len });
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn munmap(addr: *mut c_void, len: usize) -> c_int {
    let a = addr as usize;
    let len = round_up(len.max(1), 4096);
    let mut r = REGIONS.lock().unwrap();
    let Some((&start, reg)) = r.range(..=a).next_back() else { return 0 };
    match *reg {
        Region::Anon { size } if a < start + size => {
            if a == start && len >= size {
                r.remove(&start);
                unsafe { VirtualFree(start as *mut c_void, 0, MEM_RELEASE) };
            } else {
                // part of an allocation: give the memory back, keep the range
                unsafe { VirtualFree(addr, len.min(start + size - a), MEM_DECOMMIT) };
            }
        }
        Region::View { base, size } if a < base + size => {
            if a == start {
                r.remove(&start);
                unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: base as *mut c_void }) };
            }
        }
        _ => {}
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int {
    let mut old = 0u32;
    if unsafe { VirtualProtectFromApp(addr, round_up(len.max(1), 4096), page_prot(prot), &mut old) } == 0 {
        // pages that were decommitted (munmap'd parts): commit them again
        if prot != 0 && !unsafe { VirtualAllocFromApp(addr, len, MEM_COMMIT, page_prot(prot)) }.is_null() {
            return 0;
        }
        return errno::fail(errno::from_win32(unsafe { GetLastError() }));
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int {
    if advice == MADV_DONTNEED {
        // Linux zero-fills private anonymous memory after DONTNEED
        let is_anon = {
            let r = REGIONS.lock().unwrap();
            r.range(..=(addr as usize)).next_back().is_some_and(|(_, reg)| matches!(reg, Region::Anon { .. }))
        };
        if is_anon {
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            unsafe { VirtualQuery(addr, &mut info, std::mem::size_of_val(&info)) };
            if info.State == MEM_COMMIT && info.Protect & (PAGE_READWRITE | PAGE_EXECUTE_READWRITE) != 0 {
                unsafe { std::ptr::write_bytes(addr as *mut u8, 0, len) };
            }
        }
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn msync(addr: *mut c_void, len: usize, _flags: c_int) -> c_int {
    unsafe { FlushViewOfFile(addr, len) };
    0
}

extern "C" {
    fn malloc(n: usize) -> *mut c_void;
}

/// Windows' malloc is 16-byte aligned, which is all blink asks for
/// (struct System and struct Machine); free() must work on the result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, size: usize) -> c_int {
    if align > 16 || !align.is_power_of_two() {
        return errno::EINVAL;
    }
    let p = unsafe { malloc(size.max(1)) };
    if p.is_null() {
        return errno::ENOMEM;
    }
    unsafe { *out = p };
    0
}

/// Allocates blink's JIT code region (read-write; blink mprotects pages to
/// read-execute as it fills them). UWP apps may only execute memory from
/// VirtualAllocFromApp. The region goes within +/-2 GiB of this image so
/// the JIT's calls into blink stay 5-byte rel32 calls.
#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_alloc_jit(size: usize) -> *mut c_void {
    let here = blinkwin_alloc_jit as usize;
    let step = 64 << 20;
    for i in 1..30usize {
        for dir in [1isize, -1] {
            let want = (here as isize + dir * (i * step) as isize) as usize & !0xffff;
            let p = unsafe { VirtualAllocFromApp(want as *const c_void, size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
            if !p.is_null() {
                REGIONS.lock().unwrap().insert(p as usize, Region::Anon { size });
                return p;
            }
        }
    }
    // anywhere; blink falls back to absolute calls
    let p = unsafe { VirtualAllocFromApp(std::ptr::null(), size, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
    if !p.is_null() {
        REGIONS.lock().unwrap().insert(p as usize, Region::Anon { size });
    }
    p
}
