//! The file descriptor table. Guest processes all live in this one host
//! process, so there's one table; blink's VFS gives each guest process its
//! own view. Slots hold shared open files (dup shares offset and flags, as
//! on Linux) plus the per-descriptor close-on-exec flag.

use super::errno;
use std::ffi::c_int;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

pub const O_ACCMODE: u32 = 0o3;
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const O_PATH: u32 = 0o10000000;

/// What an open file is.
pub enum Obj {
    /// a regular file, opened overlapped; `pos` is the file offset
    File { handle: HANDLE, pos: Mutex<i64> },
    /// a directory; its Windows path anchors *at() calls
    Dir { path: String, handle: HANDLE },
    /// one of the host process's standard handles (a console, pipe or file
    /// the Windows side gave us), used synchronously
    Std { handle: HANDLE },
    /// an in-memory pipe end
    Pipe(super::io::PipeEnd),
    /// a Winsock socket
    Socket(super::net::Sock),
    /// the Xbox terminal (or a stand-in): see io.rs
    Tty(Arc<super::io::Tty>),
}

// SAFETY: handles are process-wide; each object serializes its own use.
unsafe impl Send for Obj {}
unsafe impl Sync for Obj {}

pub struct OpenFile {
    pub obj: Obj,
    /// O_ACCMODE | O_APPEND | O_NONBLOCK
    pub flags: AtomicU32,
}

impl OpenFile {
    pub fn new(obj: Obj, flags: u32) -> Arc<OpenFile> {
        Arc::new(OpenFile { obj, flags: AtomicU32::new(flags & (O_ACCMODE | O_APPEND | O_NONBLOCK)) })
    }

    pub fn nonblock(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_NONBLOCK != 0
    }

    pub fn can_read(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE != O_WRONLY
    }

    pub fn can_write(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & O_ACCMODE != O_RDONLY
    }
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        match &self.obj {
            Obj::File { handle, .. } | Obj::Dir { handle, .. } => {
                if !handle.is_null() {
                    // SAFETY: we own the handle; this is the last reference
                    unsafe { CloseHandle(*handle) };
                }
            }
            Obj::Socket(s) => s.close(),
            _ => {}
        }
    }
}

#[derive(Clone)]
struct Slot {
    file: Arc<OpenFile>,
    cloexec: bool,
}

static TABLE: Mutex<Vec<Option<Slot>>> = Mutex::new(Vec::new());

pub const OPEN_MAX: usize = 4096;

/// Installs `file` at the lowest free descriptor >= `min`.
pub fn add_at_least(file: Arc<OpenFile>, cloexec: bool, min: usize) -> c_int {
    let mut t = TABLE.lock().unwrap();
    let mut i = min;
    while i < t.len() && t[i].is_some() {
        i += 1;
    }
    if i >= OPEN_MAX {
        return errno::fail(errno::EMFILE);
    }
    if i >= t.len() {
        t.resize(i + 1, None);
    }
    t[i] = Some(Slot { file, cloexec });
    i as c_int
}

pub fn add(file: Arc<OpenFile>, cloexec: bool) -> c_int {
    add_at_least(file, cloexec, 0)
}

/// Puts `file` at exactly `fd`, closing what was there.
pub fn set(fd: c_int, file: Arc<OpenFile>, cloexec: bool) -> c_int {
    if fd < 0 || fd as usize >= OPEN_MAX {
        return errno::fail(errno::EBADF);
    }
    let old;
    {
        let mut t = TABLE.lock().unwrap();
        if fd as usize >= t.len() {
            t.resize(fd as usize + 1, None);
        }
        old = t[fd as usize].replace(Slot { file, cloexec });
    }
    drop(old); // may close a handle; outside the lock
    fd
}

pub fn get(fd: c_int) -> Result<Arc<OpenFile>, c_int> {
    let t = TABLE.lock().unwrap();
    match t.get(fd.max(-1) as usize).and_then(|s| s.as_ref()) {
        Some(s) if fd >= 0 => Ok(s.file.clone()),
        _ => Err(errno::EBADF),
    }
}

pub fn remove(fd: c_int) -> Result<Arc<OpenFile>, c_int> {
    let mut t = TABLE.lock().unwrap();
    match t.get_mut(fd.max(-1) as usize).and_then(|s| s.take()) {
        Some(s) if fd >= 0 => Ok(s.file),
        _ => Err(errno::EBADF),
    }
}

pub fn get_cloexec(fd: c_int) -> Result<bool, c_int> {
    let t = TABLE.lock().unwrap();
    t.get(fd.max(-1) as usize).and_then(|s| s.as_ref()).map(|s| s.cloexec).ok_or(errno::EBADF)
}

pub fn set_cloexec(fd: c_int, on: bool) -> Result<(), c_int> {
    let mut t = TABLE.lock().unwrap();
    match t.get_mut(fd.max(-1) as usize).and_then(|s| s.as_mut()) {
        Some(s) => {
            s.cloexec = on;
            Ok(())
        }
        None => Err(errno::EBADF),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn close(fd: c_int) -> c_int {
    match remove(fd) {
        Ok(f) => {
            drop(f);
            0
        }
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dup(fd: c_int) -> c_int {
    match get(fd) {
        Ok(f) => add(f, false),
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dup3(fd: c_int, newfd: c_int, flags: c_int) -> c_int {
    if fd == newfd {
        return errno::fail(errno::EINVAL);
    }
    match get(fd) {
        Ok(f) => set(newfd, f, flags as u32 & O_CLOEXEC != 0),
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dup2(fd: c_int, newfd: c_int) -> c_int {
    if fd == newfd {
        return if get(fd).is_ok() { fd } else { errno::fail(errno::EBADF) };
    }
    dup3(fd, newfd, 0)
}

const F_DUPFD: c_int = 0;
const F_GETFD: c_int = 1;
const F_SETFD: c_int = 2;
const F_GETFL: c_int = 3;
const F_SETFL: c_int = 4;
const F_GETLK: c_int = 5;
const F_SETLK: c_int = 6;
const F_SETLKW: c_int = 7;
const F_DUPFD_CLOEXEC: c_int = 1030;

#[repr(C)]
struct Flock {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: c_int,
}

/// The third argument is passed as a pointer-sized integer (varargs).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcntl(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    let f = match get(fd) {
        Ok(f) => f,
        Err(e) => return errno::fail(e),
    };
    match cmd {
        F_DUPFD => add_at_least(f, false, arg as usize),
        F_DUPFD_CLOEXEC => add_at_least(f, true, arg as usize),
        F_GETFD => get_cloexec(fd).map_or(-1, |c| c as c_int),
        F_SETFD => set_cloexec(fd, arg & 1 != 0).map_or(-1, |_| 0),
        F_GETFL => f.flags.load(Ordering::Relaxed) as c_int,
        F_SETFL => {
            let keep = f.flags.load(Ordering::Relaxed) & O_ACCMODE;
            f.flags.store(keep | (arg as u32 & (O_APPEND | O_NONBLOCK)), Ordering::Relaxed);
            if let Obj::Socket(s) = &f.obj {
                s.set_nonblock(arg as u32 & O_NONBLOCK != 0);
            }
            0
        }
        // advisory locks: every guest process is in this host process, and
        // Windows' own locks are mandatory; report locks as always free
        F_GETLK => {
            unsafe { (*(arg as *mut Flock)).l_type = 2 }; // F_UNLCK
            0
        }
        F_SETLK | F_SETLKW => 0,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn flock(fd: c_int, _op: c_int) -> c_int {
    if get(fd).is_err() { errno::fail(errno::EBADF) } else { 0 }
}

/// The Windows handle behind a descriptor (files, directories, std handles).
pub fn handle(fd: c_int) -> Result<HANDLE, c_int> {
    match &get(fd)?.obj {
        Obj::File { handle, .. } | Obj::Dir { handle, .. } | Obj::Std { handle } => Ok(*handle),
        _ => Err(errno::EINVAL),
    }
}
