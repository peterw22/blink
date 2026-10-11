//! Reading and writing: regular files (overlapped, with our own offset so
//! pread/pwrite don't move it), the host's standard handles, in-memory
//! pipes and socketpairs (every guest process lives in this host process),
//! a terminal, poll() and ioctl().

use super::errno;
use super::fd::{self, Obj, OpenFile, O_APPEND, O_CLOEXEC, O_NONBLOCK, O_RDONLY, O_WRONLY};
use super::thread;
use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{GetFileSizeEx, GetFileType, ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{GetConsoleMode, GetNumberOfConsoleInputEvents, GetStdHandle};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

thread_local! {
    static EVENT: Cell<HANDLE> = const { Cell::new(std::ptr::null_mut()) };
}

fn event() -> HANDLE {
    EVENT.with(|e| {
        if e.get().is_null() {
            // SAFETY: plain Win32 call; manual-reset, unsignaled
            e.set(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) });
        }
        e.get()
    })
}

fn overlapped(off: i64) -> OVERLAPPED {
    // SAFETY: OVERLAPPED is POD
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.Anonymous.Anonymous.Offset = off as u32;
    ov.Anonymous.Anonymous.OffsetHigh = (off >> 32) as u32;
    ov.hEvent = event();
    ov
}

/// Reads at `off` from a handle opened for overlapped I/O.
pub fn read_at(h: HANDLE, buf: &mut [u8], off: i64) -> Result<usize, c_int> {
    let mut ov = overlapped(off);
    let mut n = 0u32;
    let len = buf.len().min(1 << 30) as u32;
    // SAFETY: buf and ov outlive the wait below
    let ok = unsafe { ReadFile(h, buf.as_mut_ptr(), len, std::ptr::null_mut(), &mut ov) };
    if ok == 0 {
        let e = unsafe { GetLastError() };
        if e != 997 {
            // ERROR_IO_PENDING
            return if e == 38 { Ok(0) } else { Err(errno::from_win32(e)) }; // EOF
        }
    }
    if unsafe { GetOverlappedResult(h, &ov, &mut n, 1) } == 0 {
        let e = unsafe { GetLastError() };
        return if e == 38 { Ok(0) } else { Err(errno::from_win32(e)) };
    }
    Ok(n as usize)
}

pub fn write_at(h: HANDLE, buf: &[u8], off: i64) -> Result<usize, c_int> {
    let mut ov = overlapped(off);
    let mut n = 0u32;
    let len = buf.len().min(1 << 30) as u32;
    let ok = unsafe { WriteFile(h, buf.as_ptr(), len, std::ptr::null_mut(), &mut ov) };
    if ok == 0 && unsafe { GetLastError() } != 997 {
        return Err(errno::from_win32(unsafe { GetLastError() }));
    }
    if unsafe { GetOverlappedResult(h, &ov, &mut n, 1) } == 0 {
        return Err(errno::from_win32(unsafe { GetLastError() }));
    }
    Ok(n as usize)
}

fn file_size(h: HANDLE) -> Result<i64, c_int> {
    let mut s = 0i64;
    if unsafe { GetFileSizeEx(h, &mut s) } == 0 {
        return Err(errno::from_win32(unsafe { GetLastError() }));
    }
    Ok(s)
}

fn std_read(h: HANDLE, buf: &mut [u8]) -> Result<usize, c_int> {
    let mut n = 0u32;
    // console reads block in Windows; poll first so pthread_kill can wake us
    if is_console(h) {
        thread::wait_for(None, || (console_pending(h) > 0).then_some(()))?;
    }
    if unsafe { ReadFile(h, buf.as_mut_ptr(), buf.len().min(1 << 30) as u32, &mut n, std::ptr::null_mut()) } == 0 {
        let e = unsafe { GetLastError() };
        return if e == 109 || e == 38 { Ok(0) } else { Err(errno::from_win32(e)) };
    }
    Ok(n as usize)
}

fn std_write(h: HANDLE, buf: &[u8]) -> Result<usize, c_int> {
    let mut n = 0u32;
    if unsafe { WriteFile(h, buf.as_ptr(), buf.len().min(1 << 30) as u32, &mut n, std::ptr::null_mut()) } == 0 {
        return Err(errno::from_win32(unsafe { GetLastError() }));
    }
    Ok(n as usize)
}

fn is_console(h: HANDLE) -> bool {
    let mut m = 0u32;
    unsafe { GetFileType(h) == 2 && GetConsoleMode(h, &mut m) != 0 }
}

fn console_pending(h: HANDLE) -> u32 {
    let mut n = 0u32;
    unsafe { GetNumberOfConsoleInputEvents(h, &mut n) };
    n
}

// --- in-memory pipes ---

const PIPE_CAP: usize = 1 << 20;

#[derive(Default)]
pub struct PipeBuf {
    data: Mutex<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
}

/// One end of a pipe (rx or tx) or of a socketpair (both).
pub struct PipeEnd {
    rx: Option<Arc<PipeBuf>>,
    tx: Option<Arc<PipeBuf>>,
}

impl PipeEnd {
    fn new(rx: Option<Arc<PipeBuf>>, tx: Option<Arc<PipeBuf>>) -> PipeEnd {
        if let Some(r) = &rx {
            r.readers.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(t) = &tx {
            t.writers.fetch_add(1, Ordering::AcqRel);
        }
        PipeEnd { rx, tx }
    }

    pub fn available(&self) -> usize {
        self.rx.as_ref().map_or(0, |r| r.data.lock().unwrap().len())
    }

    fn read(&self, buf: &mut [u8], nonblock: bool) -> Result<usize, c_int> {
        let Some(r) = &self.rx else { return Err(errno::EBADF) };
        let res = thread::wait_for(if nonblock { Some(std::time::Duration::ZERO) } else { None }, || {
            let mut d = r.data.lock().unwrap();
            if !d.is_empty() {
                let n = buf.len().min(d.len());
                for (i, b) in d.drain(..n).enumerate() {
                    buf[i] = b;
                }
                Some(n)
            } else if r.writers.load(Ordering::Acquire) == 0 {
                Some(0)
            } else {
                None
            }
        })?;
        match res {
            Some(n) => {
                thread::notify_all();
                Ok(n)
            }
            None => Err(errno::EAGAIN),
        }
    }

    fn write(&self, buf: &[u8], nonblock: bool) -> Result<usize, c_int> {
        let Some(t) = &self.tx else { return Err(errno::EBADF) };
        let res = thread::wait_for(if nonblock { Some(std::time::Duration::ZERO) } else { None }, || {
            if t.readers.load(Ordering::Acquire) == 0 {
                return Some(Err(errno::EPIPE));
            }
            let mut d = t.data.lock().unwrap();
            let room = PIPE_CAP.saturating_sub(d.len());
            if room == 0 {
                return None;
            }
            let n = buf.len().min(room);
            d.extend(&buf[..n]);
            Some(Ok(n))
        })?;
        thread::notify_all();
        res.unwrap_or(Err(errno::EAGAIN))
    }

    fn poll(&self, events: i16) -> i16 {
        let mut re = 0;
        if let Some(r) = &self.rx {
            let has = !r.data.lock().unwrap().is_empty();
            let hup = r.writers.load(Ordering::Acquire) == 0;
            if has || hup {
                re |= POLLIN;
            }
            if hup {
                re |= POLLHUP;
            }
        }
        if let Some(t) = &self.tx {
            if t.readers.load(Ordering::Acquire) == 0 {
                re |= POLLERR;
            } else if t.data.lock().unwrap().len() < PIPE_CAP {
                re |= POLLOUT;
            }
        }
        re & (events | POLLHUP | POLLERR)
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        if let Some(r) = &self.rx {
            r.readers.fetch_sub(1, Ordering::AcqRel);
        }
        if let Some(t) = &self.tx {
            t.writers.fetch_sub(1, Ordering::AcqRel);
        }
        thread::notify_all();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe2(fds: *mut c_int, flags: c_int) -> c_int {
    let b = Arc::new(PipeBuf::default());
    let flags = flags as u32;
    let cloexec = flags & O_CLOEXEC != 0;
    let r = fd::add(OpenFile::new(Obj::Pipe(PipeEnd::new(Some(b.clone()), None)), O_RDONLY | (flags & O_NONBLOCK)), cloexec);
    if r == -1 {
        return -1;
    }
    let w = fd::add(OpenFile::new(Obj::Pipe(PipeEnd::new(None, Some(b))), O_WRONLY | (flags & O_NONBLOCK)), cloexec);
    if w == -1 {
        fd::close(r);
        return -1;
    }
    unsafe {
        *fds = r;
        *fds.add(1) = w;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe(fds: *mut c_int) -> c_int {
    unsafe { pipe2(fds, 0) }
}

/// AF_UNIX socketpairs become a pair of in-memory pipes.
pub fn socketpair_fds(flags: u32, fds: *mut c_int) -> c_int {
    let (a, b) = (Arc::new(PipeBuf::default()), Arc::new(PipeBuf::default()));
    let cloexec = flags & O_CLOEXEC != 0;
    let nb = flags & O_NONBLOCK;
    let x = fd::add(OpenFile::new(Obj::Pipe(PipeEnd::new(Some(a.clone()), Some(b.clone()))), 2 | nb), cloexec);
    if x == -1 {
        return -1;
    }
    let y = fd::add(OpenFile::new(Obj::Pipe(PipeEnd::new(Some(b), Some(a))), 2 | nb), cloexec);
    if y == -1 {
        fd::close(x);
        return -1;
    }
    unsafe {
        *fds = x;
        *fds.add(1) = y;
    }
    0
}

// --- terminal ---

/// struct termios (Linux layout).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 32],
    pub ispeed: u32,
    pub ospeed: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

const ICRNL: u32 = 0o400;
const OPOST: u32 = 1;
const ONLCR: u32 = 4;
const ISIG: u32 = 1;
const ICANON: u32 = 2;
const ECHO: u32 = 0o10;
const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VSUSP: usize = 10;

impl Default for Termios {
    fn default() -> Termios {
        let mut cc = [0u8; 32];
        cc[VINTR] = 3;
        cc[VQUIT] = 0x1c;
        cc[VERASE] = 0x7f;
        cc[VKILL] = 0x15;
        cc[VEOF] = 4;
        cc[5] = 0; // VTIME
        cc[6] = 1; // VMIN
        cc[VSUSP] = 0x1a;
        Termios {
            iflag: ICRNL | 0o2000 | 0o40000, // ICRNL IXON IUTF8
            oflag: OPOST | ONLCR,
            cflag: 0o60 | 0o200 | 0o10017,   // CS8 CREAD B4000000-ish
            lflag: ISIG | ICANON | ECHO | 0o20 | 0o40 | 0o1000 | 0o4000 | 0o100000, // ECHOE ECHOK ECHOCTL ECHOKE IEXTEN
            line: 0,
            cc,
            ispeed: 0o10017,
            ospeed: 0o10017,
        }
    }
}

/// A terminal: line discipline over an input queue the host fills
/// (keyboard) and an output sink the host drains (screen).
pub struct Tty {
    pub termios: Mutex<Termios>,
    pub winsize: Mutex<Winsize>,
    input: Mutex<VecDeque<u8>>,
    line: Mutex<Vec<u8>>,
    output: Box<dyn Fn(&[u8]) + Send + Sync>,
    /// sends a signal to the foreground process group (^C etc.)
    pub on_signal: Mutex<Option<Box<dyn Fn(c_int) + Send + Sync>>>,
}

impl Tty {
    pub fn new(output: Box<dyn Fn(&[u8]) + Send + Sync>, rows: u16, cols: u16) -> Arc<Tty> {
        Arc::new(Tty {
            termios: Mutex::new(Termios::default()),
            winsize: Mutex::new(Winsize { rows, cols, xpixel: 0, ypixel: 0 }),
            input: Mutex::new(VecDeque::new()),
            line: Mutex::new(Vec::new()),
            output,
            on_signal: Mutex::new(None),
        })
    }

    /// Feeds keyboard input through the line discipline.
    pub fn push_input(&self, bytes: &[u8]) {
        let t = *self.termios.lock().unwrap();
        for &b0 in bytes {
            let b = if b0 == b'\r' && t.iflag & ICRNL != 0 { b'\n' } else { b0 };
            if t.lflag & ISIG != 0 {
                let sig = if b == t.cc[VINTR] {
                    2
                } else if b == t.cc[VQUIT] {
                    3
                } else if b == t.cc[VSUSP] {
                    20
                } else {
                    0
                };
                if sig != 0 {
                    if t.lflag & ECHO != 0 {
                        self.echo(&[b'^', b ^ 0x40, b'\r', b'\n']);
                    }
                    self.line.lock().unwrap().clear();
                    if let Some(f) = &*self.on_signal.lock().unwrap() {
                        f(sig);
                    }
                    continue;
                }
            }
            if t.lflag & ICANON != 0 {
                let mut line = self.line.lock().unwrap();
                if b == t.cc[VERASE] || b == 8 {
                    if line.pop().is_some() && t.lflag & ECHO != 0 {
                        self.echo(b"\x08 \x08");
                    }
                    continue;
                }
                if b == t.cc[VKILL] {
                    line.clear();
                    continue;
                }
                if b == t.cc[VEOF] {
                    let l = std::mem::take(&mut *line);
                    self.input.lock().unwrap().extend(l);
                    // an empty line reads as end of file: push a marker read() returns 0 for
                    continue;
                }
                line.push(b);
                if t.lflag & ECHO != 0 {
                    self.echo(&[b]);
                }
                if b == b'\n' {
                    let l = std::mem::take(&mut *line);
                    self.input.lock().unwrap().extend(l);
                }
            } else {
                if t.lflag & ECHO != 0 {
                    self.echo(&[b]);
                }
                self.input.lock().unwrap().push_back(b);
            }
        }
        thread::notify_all();
    }

    fn echo(&self, b: &[u8]) {
        self.write_out(b);
    }

    fn write_out(&self, buf: &[u8]) {
        let t = *self.termios.lock().unwrap();
        if t.oflag & OPOST != 0 && t.oflag & ONLCR != 0 && buf.contains(&b'\n') {
            let mut v = Vec::with_capacity(buf.len() + 16);
            for &b in buf {
                if b == b'\n' {
                    v.push(b'\r');
                }
                v.push(b);
            }
            (self.output)(&v);
        } else {
            (self.output)(buf);
        }
    }

    pub fn available(&self) -> usize {
        self.input.lock().unwrap().len()
    }

    fn read(&self, buf: &mut [u8], nonblock: bool) -> Result<usize, c_int> {
        let res = thread::wait_for(if nonblock { Some(std::time::Duration::ZERO) } else { None }, || {
            let mut q = self.input.lock().unwrap();
            if q.is_empty() {
                return None;
            }
            let canon = self.termios.lock().unwrap().lflag & ICANON != 0;
            let mut n = 0;
            while n < buf.len() {
                let Some(b) = q.pop_front() else { break };
                buf[n] = b;
                n += 1;
                if canon && b == b'\n' {
                    break;
                }
            }
            Some(n)
        })?;
        res.ok_or(errno::EAGAIN)
    }

    pub fn set_winsize(&self, rows: u16, cols: u16) {
        *self.winsize.lock().unwrap() = Winsize { rows, cols, xpixel: 0, ypixel: 0 };
        if let Some(f) = &*self.on_signal.lock().unwrap() {
            f(28); // SIGWINCH
        }
    }
}

// --- read/write dispatch ---

pub fn read_file(f: &OpenFile, buf: &mut [u8]) -> Result<usize, c_int> {
    if !f.can_read() {
        return Err(errno::EBADF);
    }
    match &f.obj {
        Obj::File { handle, pos } => {
            let mut p = pos.lock().unwrap();
            let n = read_at(*handle, buf, *p)?;
            *p += n as i64;
            Ok(n)
        }
        Obj::Dir { .. } => Err(errno::EISDIR),
        Obj::Std { handle } => std_read(*handle, buf),
        Obj::Pipe(p) => p.read(buf, f.nonblock()),
        Obj::Socket(s) => s.recv(buf, 0, f.nonblock()),
        Obj::Tty(t) => t.read(buf, f.nonblock()),
    }
}

pub fn write_file(f: &OpenFile, buf: &[u8]) -> Result<usize, c_int> {
    if !f.can_write() {
        return Err(errno::EBADF);
    }
    match &f.obj {
        Obj::File { handle, pos } => {
            let mut p = pos.lock().unwrap();
            if f.flags.load(Ordering::Relaxed) & O_APPEND != 0 {
                *p = file_size(*handle)?;
            }
            let n = write_at(*handle, buf, *p)?;
            *p += n as i64;
            Ok(n)
        }
        Obj::Dir { .. } => Err(errno::EISDIR),
        Obj::Std { handle } => std_write(*handle, buf),
        Obj::Pipe(p) => p.write(buf, f.nonblock()),
        Obj::Socket(s) => s.send(buf, 0, f.nonblock()),
        Obj::Tty(t) => {
            t.write_out(buf);
            Ok(buf.len())
        }
    }
}

fn rw_result(r: Result<usize, c_int>) -> isize {
    r.map_or_else(errno::fail_isize, |n| n as isize)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fdn: c_int, buf: *mut c_void, n: usize) -> isize {
    rw_result(fd::get(fdn).and_then(|f| read_file(&f, unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, n) })))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fdn: c_int, buf: *const c_void, n: usize) -> isize {
    rw_result(fd::get(fdn).and_then(|f| write_file(&f, unsafe { std::slice::from_raw_parts(buf as *const u8, n) })))
}

fn positioned(f: &OpenFile) -> Result<HANDLE, c_int> {
    match &f.obj {
        Obj::File { handle, .. } => Ok(*handle),
        Obj::Dir { .. } => Err(errno::EISDIR),
        _ => Err(errno::ESPIPE),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pread(fdn: c_int, buf: *mut c_void, n: usize, off: i64) -> isize {
    rw_result(fd::get(fdn).and_then(|f| {
        if !f.can_read() {
            return Err(errno::EBADF);
        }
        read_at(positioned(&f)?, unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, n) }, off)
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pwrite(fdn: c_int, buf: *const c_void, n: usize, off: i64) -> isize {
    rw_result(fd::get(fdn).and_then(|f| {
        if !f.can_write() {
            return Err(errno::EBADF);
        }
        write_at(positioned(&f)?, unsafe { std::slice::from_raw_parts(buf as *const u8, n) }, off)
    }))
}

#[repr(C)]
pub struct Iovec {
    base: *mut u8,
    len: usize,
}

fn iovs<'a>(iov: *const Iovec, n: c_int) -> &'a [Iovec] {
    if n <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(iov, n as usize) } }
}

/// Vectored I/O as a sequence of single transfers; stops at a short one.
fn vectored(iov: *const Iovec, n: c_int, mut each: impl FnMut(&Iovec, i64) -> Result<usize, c_int>) -> isize {
    let mut total = 0usize;
    for v in iovs(iov, n) {
        if v.len == 0 {
            continue;
        }
        match each(v, total as i64) {
            Ok(k) => {
                total += k;
                if k < v.len {
                    break;
                }
            }
            Err(e) => return if total > 0 { total as isize } else { errno::fail_isize(e) },
        }
    }
    total as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn readv(fdn: c_int, iov: *const Iovec, n: c_int) -> isize {
    let f = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail_isize(e),
    };
    vectored(iov, n, |v, _| read_file(&f, unsafe { std::slice::from_raw_parts_mut(v.base, v.len) }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn writev(fdn: c_int, iov: *const Iovec, n: c_int) -> isize {
    let f = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail_isize(e),
    };
    // one write for pipes/sockets/ttys so it isn't interleaved
    if !matches!(f.obj, Obj::File { .. }) {
        let mut all = Vec::new();
        for v in iovs(iov, n) {
            all.extend_from_slice(unsafe { std::slice::from_raw_parts(v.base, v.len) });
        }
        return rw_result(write_file(&f, &all));
    }
    vectored(iov, n, |v, _| write_file(&f, unsafe { std::slice::from_raw_parts(v.base, v.len) }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn preadv(fdn: c_int, iov: *const Iovec, n: c_int, off: i64) -> isize {
    let h = match fd::get(fdn).and_then(|f| positioned(&f)) {
        Ok(h) => h,
        Err(e) => return errno::fail_isize(e),
    };
    vectored(iov, n, |v, done| read_at(h, unsafe { std::slice::from_raw_parts_mut(v.base, v.len) }, off + done))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pwritev(fdn: c_int, iov: *const Iovec, n: c_int, off: i64) -> isize {
    let h = match fd::get(fdn).and_then(|f| positioned(&f)) {
        Ok(h) => h,
        Err(e) => return errno::fail_isize(e),
    };
    vectored(iov, n, |v, done| write_at(h, unsafe { std::slice::from_raw_parts(v.base, v.len) }, off + done))
}

#[unsafe(no_mangle)]
pub extern "C" fn lseek(fdn: c_int, off: i64, whence: c_int) -> i64 {
    let f = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail_i64(e),
    };
    match &f.obj {
        Obj::File { handle, pos } => {
            let mut p = pos.lock().unwrap();
            let base = match whence {
                0 => 0,
                1 => *p,
                2 => match file_size(*handle) {
                    Ok(s) => s,
                    Err(e) => return errno::fail_i64(e),
                },
                3 => off, // SEEK_DATA: no holes
                4 => match file_size(*handle) {
                    Ok(s) => return s, // SEEK_HOLE: the end
                    Err(e) => return errno::fail_i64(e),
                },
                _ => return errno::fail_i64(errno::EINVAL),
            };
            let n = if whence == 3 { off } else { base + off };
            if n < 0 {
                return errno::fail_i64(errno::EINVAL);
            }
            *p = n;
            n
        }
        Obj::Dir { .. } => 0,
        _ => errno::fail_i64(errno::ESPIPE),
    }
}

// --- poll ---

pub const POLLIN: i16 = 0x1;
pub const POLLPRI: i16 = 0x2;
pub const POLLOUT: i16 = 0x4;
pub const POLLERR: i16 = 0x8;
pub const POLLHUP: i16 = 0x10;
pub const POLLNVAL: i16 = 0x20;

#[repr(C)]
pub struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

fn std_ready(h: HANDLE, events: i16) -> i16 {
    let mut re = POLLOUT;
    match unsafe { GetFileType(h) } {
        2 => {
            if is_console(h) && console_pending(h) > 0 {
                re |= POLLIN;
            }
        }
        3 => {
            let mut avail = 0u32;
            if unsafe { PeekNamedPipe(h, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut()) } == 0 {
                re |= POLLIN | POLLHUP;
            } else if avail > 0 {
                re |= POLLIN;
            }
        }
        _ => re |= POLLIN,
    }
    re & (events | POLLHUP | POLLERR)
}

fn ready(f: &OpenFile, events: i16) -> i16 {
    match &f.obj {
        Obj::File { .. } | Obj::Dir { .. } => events & (POLLIN | POLLOUT),
        Obj::Std { handle } => std_ready(*handle, events),
        Obj::Pipe(p) => p.poll(events),
        Obj::Socket(s) => s.poll(events),
        Obj::Tty(t) => (if t.available() > 0 { POLLIN } else { 0 } | POLLOUT) & events,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn poll(fds: *mut PollFd, n: u64, timeout: c_int) -> c_int {
    let fds = if n == 0 { &mut [][..] } else { unsafe { std::slice::from_raw_parts_mut(fds, n as usize) } };
    let files: Vec<Option<Arc<OpenFile>>> = fds.iter().map(|p| if p.fd < 0 { None } else { fd::get(p.fd).ok() }).collect();
    let timeout = if timeout < 0 { None } else { Some(std::time::Duration::from_millis(timeout as u64)) };
    let r = thread::wait_for(timeout, || {
        let mut count = 0;
        for (p, f) in fds.iter_mut().zip(&files) {
            p.revents = match f {
                _ if p.fd < 0 => 0,
                None => POLLNVAL,
                Some(f) => ready(f, p.events),
            };
            if p.revents != 0 {
                count += 1;
            }
        }
        (count > 0).then_some(count)
    });
    match r {
        Ok(Some(c)) => c,
        Ok(None) => 0,
        Err(e) => errno::fail(e),
    }
}

// --- ioctl and termios ---

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TIOCSCTTY: u32 = 0x540E;
const TIOCGPGRP: u32 = 0x540F;
const TIOCSPGRP: u32 = 0x5410;
const TIOCOUTQ: u32 = 0x5411;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;
const FIONREAD: u32 = 0x541B;
const TIOCNOTTY: u32 = 0x5422;
const FIONBIO: u32 = 0x5421;
const FIONCLEX: u32 = 0x5450;
const FIOCLEX: u32 = 0x5451;

fn tty_of(f: &OpenFile) -> Option<&Arc<Tty>> {
    match &f.obj {
        Obj::Tty(t) => Some(t),
        _ => None,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioctl(fdn: c_int, req: u32, arg: *mut c_void) -> c_int {
    let f = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail(e),
    };
    match req {
        FIONREAD => {
            let n = match &f.obj {
                Obj::Pipe(p) => p.available(),
                Obj::Tty(t) => t.available(),
                Obj::Socket(s) => s.available(),
                Obj::File { handle, pos } => (file_size(*handle).unwrap_or(0) - *pos.lock().unwrap()).max(0) as usize,
                _ => 0,
            };
            unsafe { *(arg as *mut c_int) = n as c_int };
            0
        }
        FIONBIO => {
            let on = unsafe { *(arg as *const c_int) } != 0;
            let fl = f.flags.load(Ordering::Relaxed);
            f.flags.store(if on { fl | O_NONBLOCK } else { fl & !O_NONBLOCK }, Ordering::Relaxed);
            if let Obj::Socket(s) = &f.obj {
                s.set_nonblock(on);
            }
            0
        }
        FIOCLEX | FIONCLEX => fd::set_cloexec(fdn, req == FIOCLEX).map_or_else(errno::fail, |_| 0),
        _ => {
            let Some(t) = tty_of(&f) else { return errno::fail(errno::ENOTTY) };
            match req {
                TCGETS => {
                    unsafe { *(arg as *mut Termios) = *t.termios.lock().unwrap() };
                    0
                }
                TCSETS | TCSETSW | TCSETSF => {
                    *t.termios.lock().unwrap() = unsafe { *(arg as *const Termios) };
                    if req == TCSETSF {
                        t.input.lock().unwrap().clear();
                    }
                    0
                }
                TIOCGWINSZ => {
                    unsafe { *(arg as *mut Winsize) = *t.winsize.lock().unwrap() };
                    0
                }
                TIOCSWINSZ => {
                    let w = unsafe { *(arg as *const Winsize) };
                    t.set_winsize(w.rows, w.cols);
                    0
                }
                TIOCGPGRP => {
                    unsafe { *(arg as *mut c_int) = std::process::id() as c_int };
                    0
                }
                TIOCSPGRP | TIOCSCTTY | TIOCNOTTY => 0,
                TIOCOUTQ => {
                    unsafe { *(arg as *mut c_int) = 0 };
                    0
                }
                _ => errno::fail(errno::EINVAL),
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn isatty(fdn: c_int) -> c_int {
    match fd::get(fdn) {
        Ok(f) if tty_of(&f).is_some() => 1,
        Ok(f) => match &f.obj {
            Obj::Std { handle } if is_console(*handle) => 1,
            _ => errno::fail(errno::ENOTTY).max(0),
        },
        Err(e) => errno::fail(e).max(0),
    }
}

fn with_tty(fdn: c_int, f: impl FnOnce(&Tty) -> c_int) -> c_int {
    match fd::get(fdn) {
        Ok(of) => match tty_of(&of) {
            Some(t) => f(t),
            None => errno::fail(errno::ENOTTY),
        },
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcgetattr(fdn: c_int, t: *mut Termios) -> c_int {
    with_tty(fdn, |tty| {
        unsafe { *t = *tty.termios.lock().unwrap() };
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn tcsetattr(fdn: c_int, _act: c_int, t: *const Termios) -> c_int {
    with_tty(fdn, |tty| {
        *tty.termios.lock().unwrap() = unsafe { *t };
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn tcsendbreak(fdn: c_int, _d: c_int) -> c_int {
    with_tty(fdn, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn tcdrain(fdn: c_int) -> c_int {
    with_tty(fdn, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn tcflush(fdn: c_int, q: c_int) -> c_int {
    with_tty(fdn, |t| {
        if q != 1 {
            t.input.lock().unwrap().clear();
        }
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn tcflow(fdn: c_int, _a: c_int) -> c_int {
    with_tty(fdn, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn tcgetpgrp(fdn: c_int) -> c_int {
    with_tty(fdn, |_| std::process::id() as c_int)
}

#[unsafe(no_mangle)]
pub extern "C" fn tcsetpgrp(fdn: c_int, _p: c_int) -> c_int {
    with_tty(fdn, |_| 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn tcgetsid(fdn: c_int) -> c_int {
    with_tty(fdn, |_| std::process::id() as c_int)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfgetispeed(t: *const Termios) -> u32 {
    unsafe { (*t).ispeed }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfgetospeed(t: *const Termios) -> u32 {
    unsafe { (*t).ospeed }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfsetispeed(t: *mut Termios, s: u32) -> c_int {
    unsafe { (*t).ispeed = s };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cfsetospeed(t: *mut Termios, s: u32) -> c_int {
    unsafe { (*t).ospeed = s };
    0
}

/// Sets up descriptors 0-2 from the host's standard handles.
pub fn init_std() {
    for (fdn, which) in [(0, -10i32), (1, -11), (2, -12)] {
        // STD_INPUT_HANDLE etc.
        let h = unsafe { GetStdHandle(which as u32) };
        if h.is_null() || h as isize == -1 {
            continue;
        }
        let flags = if fdn == 0 { O_RDONLY } else { O_WRONLY };
        fd::set(fdn, OpenFile::new(Obj::Std { handle: h }, flags), false);
    }
}

#[allow(dead_code)]
fn _wait(h: HANDLE) {
    unsafe { WaitForSingleObject(h, INFINITE) };
}
