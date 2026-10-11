//! epoll and eventfd, which Bun's event loop is built on. Every guest
//! process lives in this host process, so both are plain objects here:
//! an epoll instance is an interest list polled through `io::ready`, an
//! eventfd is a counter.

use super::errno;
use super::fd::{self, Obj, OpenFile, O_CLOEXEC, O_NONBLOCK, O_RDWR};
use super::io::{self, POLLERR, POLLHUP, POLLIN, POLLOUT};
use super::thread;
use std::ffi::c_int;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

const EPOLLIN: u32 = 0x1;
const EPOLLOUT: u32 = 0x4;
const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;
const EPOLLRDHUP: u32 = 0x2000;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLET: u32 = 1 << 31;
const EPOLL_CTL_ADD: c_int = 1;
const EPOLL_CTL_DEL: c_int = 2;
const EPOLL_CTL_MOD: c_int = 3;
const EFD_SEMAPHORE: c_int = 1;

/// struct epoll_event: packed on x86-64 Linux (12 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EpollEvent {
    events: u32,
    data: u64,
}

struct Interest {
    fd: c_int,
    file: Weak<OpenFile>,
    events: u32,
    data: u64,
    /// readiness last reported, for edge-triggered interests
    last: u32,
    /// one-shot interests are disabled after reporting, until EPOLL_CTL_MOD
    disabled: bool,
}

#[derive(Default)]
pub struct Epoll {
    list: Mutex<Vec<Interest>>,
}

impl Epoll {
    /// Collects up to `max` ready events.
    fn collect(&self, max: usize) -> Vec<EpollEvent> {
        let mut out = Vec::new();
        let mut list = self.list.lock().unwrap();
        list.retain(|i| i.file.strong_count() > 0);
        for i in list.iter_mut() {
            if out.len() >= max {
                break;
            }
            if i.disabled {
                continue;
            }
            let Some(f) = i.file.upgrade() else { continue };
            let mut want = 0i16;
            if i.events & EPOLLIN != 0 {
                want |= POLLIN;
            }
            if i.events & EPOLLOUT != 0 {
                want |= POLLOUT;
            }
            let r = io::ready(&f, want);
            let mut ev = 0u32;
            if r & POLLIN != 0 {
                ev |= EPOLLIN;
            }
            if r & POLLOUT != 0 {
                ev |= EPOLLOUT;
            }
            if r & POLLERR != 0 {
                ev |= EPOLLERR;
            }
            if r & POLLHUP != 0 {
                ev |= EPOLLHUP | (i.events & EPOLLRDHUP);
            }
            let fresh = if i.events & EPOLLET != 0 { ev & !i.last } else { ev };
            i.last = ev;
            if fresh != 0 {
                out.push(EpollEvent { events: if i.events & EPOLLET != 0 { ev } else { fresh }, data: i.data });
                if i.events & EPOLLONESHOT != 0 {
                    i.disabled = true;
                }
            }
        }
        out
    }

    pub fn poll(&self, events: i16) -> i16 {
        // readable when any interest is ready (without consuming edges)
        let list = self.list.lock().unwrap();
        let any = list.iter().any(|i| {
            !i.disabled
                && i.file.upgrade().is_some_and(|f| {
                    let want = (if i.events & EPOLLIN != 0 { POLLIN } else { 0 }) | (if i.events & EPOLLOUT != 0 { POLLOUT } else { 0 });
                    io::ready(&f, want) != 0
                })
        });
        if any { events & POLLIN } else { 0 }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn epoll_create1(flags: c_int) -> c_int {
    fd::add(OpenFile::new(Obj::Epoll(Arc::new(Epoll::default())), O_RDWR), flags as u32 & O_CLOEXEC != 0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_ctl(epfd: c_int, op: c_int, fdn: c_int, ev: *const EpollEvent) -> c_int {
    let ep = match fd::get(epfd) {
        Ok(f) => match &f.obj {
            Obj::Epoll(e) => e.clone(),
            _ => return errno::fail(errno::EINVAL),
        },
        Err(e) => return errno::fail(e),
    };
    let file = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail(e),
    };
    if fdn == epfd {
        return errno::fail(errno::EINVAL);
    }
    let mut list = ep.list.lock().unwrap();
    let pos = list.iter().position(|i| i.fd == fdn && i.file.upgrade().is_some_and(|f| Arc::ptr_eq(&f, &file)));
    match op {
        EPOLL_CTL_ADD => {
            if pos.is_some() {
                return errno::fail(errno::EEXIST);
            }
            let e = unsafe { *ev };
            list.push(Interest { fd: fdn, file: Arc::downgrade(&file), events: e.events, data: e.data, last: 0, disabled: false });
        }
        EPOLL_CTL_MOD => {
            let Some(p) = pos else { return errno::fail(errno::ENOENT) };
            let e = unsafe { *ev };
            list[p].events = e.events;
            list[p].data = e.data;
            list[p].last = 0;
            list[p].disabled = false;
        }
        EPOLL_CTL_DEL => {
            let Some(p) = pos else { return errno::fail(errno::ENOENT) };
            list.remove(p);
        }
        _ => return errno::fail(errno::EINVAL),
    }
    drop(list);
    thread::notify_all();
    0
}

fn wait(epfd: c_int, out: *mut EpollEvent, max: c_int, timeout: Option<Duration>) -> c_int {
    if max <= 0 {
        return errno::fail(errno::EINVAL);
    }
    let ep = match fd::get(epfd) {
        Ok(f) => match &f.obj {
            Obj::Epoll(e) => e.clone(),
            _ => return errno::fail(errno::EINVAL),
        },
        Err(e) => return errno::fail(e),
    };
    match thread::wait_for(timeout, || {
        let v = ep.collect(max as usize);
        (!v.is_empty()).then_some(v)
    }) {
        Ok(Some(v)) => {
            for (i, e) in v.iter().enumerate() {
                unsafe { *out.add(i) = *e };
            }
            v.len() as c_int
        }
        Ok(None) => 0,
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_pwait(epfd: c_int, out: *mut EpollEvent, max: c_int, timeout: c_int, _mask: *const u8) -> c_int {
    wait(epfd, out, max, (timeout >= 0).then(|| Duration::from_millis(timeout as u64)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_wait(epfd: c_int, out: *mut EpollEvent, max: c_int, timeout: c_int) -> c_int {
    unsafe { epoll_pwait(epfd, out, max, timeout, std::ptr::null()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn epoll_pwait2(epfd: c_int, out: *mut EpollEvent, max: c_int, ts: *const super::time::Timespec, _mask: *const u8) -> c_int {
    let timeout = if ts.is_null() {
        None
    } else {
        let t = unsafe { *ts };
        Some(Duration::new(t.tv_sec.max(0) as u64, t.tv_nsec.max(0) as u32))
    };
    wait(epfd, out, max, timeout)
}

/// An eventfd's counter.
pub struct EventFd {
    count: Mutex<u64>,
    semaphore: bool,
}

impl EventFd {
    pub fn read(&self, buf: &mut [u8], nonblock: bool) -> Result<usize, c_int> {
        if buf.len() < 8 {
            return Err(errno::EINVAL);
        }
        let r = thread::wait_for(if nonblock { Some(Duration::ZERO) } else { None }, || {
            let mut c = self.count.lock().unwrap();
            if *c == 0 {
                return None;
            }
            let v = if self.semaphore { 1 } else { *c };
            *c -= v;
            Some(v)
        })?;
        match r {
            Some(v) => {
                buf[..8].copy_from_slice(&v.to_ne_bytes());
                thread::notify_all();
                Ok(8)
            }
            None => Err(errno::EAGAIN),
        }
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, c_int> {
        if buf.len() < 8 {
            return Err(errno::EINVAL);
        }
        let v = u64::from_ne_bytes(buf[..8].try_into().unwrap());
        if v == u64::MAX {
            return Err(errno::EINVAL);
        }
        {
            let mut c = self.count.lock().unwrap();
            *c = c.saturating_add(v).min(u64::MAX - 1);
        }
        thread::notify_all();
        Ok(8)
    }

    pub fn poll(&self, events: i16) -> i16 {
        let readable = *self.count.lock().unwrap() > 0;
        (if readable { POLLIN } else { 0 } | POLLOUT) & events
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn eventfd(initval: u32, flags: c_int) -> c_int {
    let e = EventFd { count: Mutex::new(initval as u64), semaphore: flags & EFD_SEMAPHORE != 0 };
    let nb = if flags as u32 & O_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
    fd::add(OpenFile::new(Obj::EventFd(Arc::new(e)), O_RDWR | nb), flags as u32 & O_CLOEXEC != 0)
}
