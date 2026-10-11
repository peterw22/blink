//! Sockets on Winsock. Linux socket addresses have Winsock's layout apart
//! from the family number; options are renumbered. Sockets are always
//! non-blocking inside Winsock, and blocking calls wait interruptibly
//! (see thread.rs) so pthread_kill can wake them. AF_UNIX socketpairs are
//! in-memory pipes (io.rs).

use super::errno;
use super::fd::{self, Obj, OpenFile, O_CLOEXEC, O_NONBLOCK, O_RDWR};
use super::thread;
use std::ffi::{c_char, c_int, c_void, CStr};
use windows_sys::Win32::Networking::WinSock as ws;

const AF_UNIX: c_int = 1;
const AF_INET: c_int = 2;
const AF_INET6_LINUX: u16 = 10;
const AF_INET6_WIN: u16 = 23;
const SOCK_NONBLOCK: c_int = 0o4000;
const SOCK_CLOEXEC: c_int = 0o2000000;
const MSG_DONTWAIT: c_int = 0x40;
const SOL_SOCKET_LINUX: c_int = 1;
const SOL_SOCKET_WIN: c_int = 0xffff;

pub fn init() {
    // SAFETY: WSADATA is a POD out-parameter
    let mut d: ws::WSADATA = unsafe { std::mem::zeroed() };
    unsafe { ws::WSAStartup(0x0202, &mut d) };
}

fn wsa_errno() -> c_int {
    // SAFETY: plain Winsock call
    match unsafe { ws::WSAGetLastError() } {
        10004 => errno::EINTR,
        10009 => errno::EBADF,
        10013 => errno::EACCES,
        10014 => errno::EFAULT,
        10022 => errno::EINVAL,
        10024 => errno::EMFILE,
        10035 => errno::EAGAIN,
        10036 => errno::EINPROGRESS,
        10037 => errno::EALREADY,
        10038 => errno::ENOTSOCK,
        10039 => errno::EDESTADDRREQ,
        10040 => errno::EMSGSIZE,
        10041 => errno::EPROTOTYPE,
        10042 => errno::ENOPROTOOPT,
        10043 => errno::EPROTONOSUPPORT,
        10044 | 10045 => errno::EOPNOTSUPP,
        10047 => errno::EAFNOSUPPORT,
        10048 => errno::EADDRINUSE,
        10049 => errno::EADDRNOTAVAIL,
        10050 => errno::ENETDOWN,
        10051 => errno::ENETUNREACH,
        10052 => errno::ENETRESET,
        10053 => errno::ECONNABORTED,
        10054 => errno::ECONNRESET,
        10055 => errno::ENOBUFS,
        10056 => errno::EISCONN,
        10057 => errno::ENOTCONN,
        10058 => errno::EPIPE,
        10060 => errno::ETIMEDOUT,
        10061 => errno::ECONNREFUSED,
        10065 => errno::EHOSTUNREACH,
        11001 | 11002 | 11003 | 11004 => errno::EHOSTUNREACH,
        _ => errno::EIO,
    }
}

/// A socket descriptor's Winsock socket.
pub struct Sock {
    s: ws::SOCKET,
    pub family: c_int,
    pub ty: c_int,
    pub proto: c_int,
}

impl Sock {
    pub fn close(&self) {
        // SAFETY: we own the socket
        unsafe { ws::closesocket(self.s) };
    }

    pub fn set_nonblock(&self, _on: bool) {
        // always non-blocking in Winsock; O_NONBLOCK is applied here
    }

    fn poll1(&self, events: i16, ms: i32) -> i16 {
        let mut p = ws::WSAPOLLFD { fd: self.s, events: 0, revents: 0 };
        if events & super::io::POLLIN != 0 {
            p.events |= ws::POLLRDNORM;
        }
        if events & super::io::POLLOUT != 0 {
            p.events |= ws::POLLWRNORM;
        }
        // SAFETY: one valid pollfd
        if unsafe { ws::WSAPoll(&mut p, 1, ms) } <= 0 {
            return 0;
        }
        let mut re = 0;
        if p.revents & (ws::POLLRDNORM | ws::POLLRDBAND) != 0 {
            re |= super::io::POLLIN;
        }
        if p.revents & ws::POLLWRNORM != 0 {
            re |= super::io::POLLOUT;
        }
        if p.revents & ws::POLLHUP != 0 {
            re |= super::io::POLLHUP | super::io::POLLIN;
        }
        if p.revents & ws::POLLERR != 0 {
            re |= super::io::POLLERR;
        }
        re
    }

    pub fn poll(&self, events: i16) -> i16 {
        self.poll1(events, 0) & (events | super::io::POLLHUP | super::io::POLLERR)
    }

    pub fn available(&self) -> usize {
        let mut n = 0u32;
        // FIONREAD
        unsafe { ws::ioctlsocket(self.s, 0x4004667f, &mut n) };
        n as usize
    }

    /// Runs a non-blocking operation, waiting for readiness when it would
    /// block (unless `nonblock`).
    fn blocking<T>(&self, nonblock: bool, events: i16, mut op: impl FnMut() -> Result<T, c_int>) -> Result<T, c_int> {
        loop {
            match op() {
                Err(errno::EAGAIN) if !nonblock => {
                    let s = self;
                    thread::wait_for(None, || (s.poll1(events, 0) != 0).then_some(()))?;
                }
                r => return r,
            }
        }
    }

    pub fn recv(&self, buf: &mut [u8], flags: c_int, nonblock: bool) -> Result<usize, c_int> {
        self.recvfrom(buf, flags, nonblock, std::ptr::null_mut(), std::ptr::null_mut())
    }

    pub fn send(&self, buf: &[u8], flags: c_int, nonblock: bool) -> Result<usize, c_int> {
        self.sendto(buf, flags, nonblock, std::ptr::null(), 0)
    }

    fn recvfrom(&self, buf: &mut [u8], flags: c_int, nonblock: bool, addr: *mut u8, len: *mut u32) -> Result<usize, c_int> {
        let wflags = flags & 0x3 | if flags & 0x100 != 0 { 8 } else { 0 }; // OOB PEEK, WAITALL
        let nb = nonblock || flags & MSG_DONTWAIT != 0;
        let mut storage = [0u8; 128];
        let mut slen = 128i32;
        let n = self.blocking(nb, super::io::POLLIN, || {
            let r = unsafe {
                ws::recvfrom(self.s, buf.as_mut_ptr(), buf.len().min(i32::MAX as usize) as i32, wflags, storage.as_mut_ptr() as *mut ws::SOCKADDR, &mut slen)
            };
            if r < 0 { Err(wsa_errno()) } else { Ok(r as usize) }
        })?;
        if !addr.is_null() && !len.is_null() {
            unsafe { write_addr(&storage, slen as u32, addr, len) };
        }
        Ok(n)
    }

    fn sendto(&self, buf: &[u8], flags: c_int, nonblock: bool, addr: *const u8, len: u32) -> Result<usize, c_int> {
        let nb = nonblock || flags & MSG_DONTWAIT != 0;
        let (a, alen) = if addr.is_null() { (Vec::new(), 0) } else { unsafe { read_addr(addr, len) } };
        self.blocking(nb, super::io::POLLOUT, || {
            let r = unsafe {
                ws::sendto(self.s, buf.as_ptr(), buf.len().min(i32::MAX as usize) as i32, flags & 1, if a.is_empty() { std::ptr::null() } else { a.as_ptr() as *const ws::SOCKADDR }, alen)
            };
            if r < 0 { Err(wsa_errno()) } else { Ok(r as usize) }
        })
    }
}

/// Copies a Linux sockaddr to Winsock form (just the family number).
unsafe fn read_addr(p: *const u8, len: u32) -> (Vec<u8>, i32) {
    let mut v = unsafe { std::slice::from_raw_parts(p, len as usize) }.to_vec();
    if v.len() >= 2 && u16::from_ne_bytes([v[0], v[1]]) == AF_INET6_LINUX {
        v[..2].copy_from_slice(&AF_INET6_WIN.to_ne_bytes());
    }
    let n = v.len() as i32;
    (v, n)
}

/// Writes a Winsock sockaddr back in Linux form.
unsafe fn write_addr(w: &[u8], wlen: u32, out: *mut u8, outlen: *mut u32) {
    let mut v = w[..(wlen as usize).min(w.len())].to_vec();
    if v.len() >= 2 && u16::from_ne_bytes([v[0], v[1]]) == AF_INET6_WIN {
        v[..2].copy_from_slice(&AF_INET6_LINUX.to_ne_bytes());
    }
    unsafe {
        let n = v.len().min(*outlen as usize);
        std::ptr::copy_nonoverlapping(v.as_ptr(), out, n);
        *outlen = v.len() as u32;
    }
}

fn sock_of(fdn: c_int) -> Result<(std::sync::Arc<OpenFile>, ws::SOCKET), c_int> {
    let f = fd::get(fdn)?;
    let s = match &f.obj {
        Obj::Socket(s) => s.s,
        _ => return Err(errno::ENOTSOCK),
    };
    Ok((f, s))
}

fn add_socket(s: ws::SOCKET, family: c_int, ty: c_int, proto: c_int, flags: c_int) -> c_int {
    let mut on = 1u32;
    // FIONBIO
    unsafe { ws::ioctlsocket(s, 0x8004667eu32 as i32, &mut on) };
    let nb = if flags & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
    fd::add(OpenFile::new(Obj::Socket(Sock { s, family, ty, proto }), O_RDWR | nb), flags & SOCK_CLOEXEC != 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_socket(family: c_int, ty: c_int, proto: c_int) -> c_int {
    let base = ty & 0xf;
    let wfam = match family {
        AF_INET => 2,
        10 => 23,
        AF_UNIX => return errno::fail(errno::EAFNOSUPPORT),
        _ => return errno::fail(errno::EAFNOSUPPORT),
    };
    let s = unsafe { ws::socket(wfam, base, proto) };
    if s == ws::INVALID_SOCKET {
        return errno::fail(wsa_errno());
    }
    add_socket(s, family, base, proto, ty)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_socketpair(family: c_int, ty: c_int, _proto: c_int, fds: *mut c_int) -> c_int {
    if family != AF_UNIX {
        return errno::fail(errno::EOPNOTSUPP);
    }
    let flags = (if ty & SOCK_NONBLOCK != 0 { O_NONBLOCK } else { 0 }) | (if ty & SOCK_CLOEXEC != 0 { O_CLOEXEC } else { 0 });
    super::io::socketpair_fds(flags, fds)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_bind(fdn: c_int, addr: *const u8, len: u32) -> c_int {
    let (_f, s) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail(e),
    };
    let (a, n) = unsafe { read_addr(addr, len) };
    if unsafe { ws::bind(s, a.as_ptr() as *const ws::SOCKADDR, n) } != 0 { errno::fail(wsa_errno()) } else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_connect(fdn: c_int, addr: *const u8, len: u32) -> c_int {
    let (f, s) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail(e),
    };
    let (a, n) = unsafe { read_addr(addr, len) };
    if unsafe { ws::connect(s, a.as_ptr() as *const ws::SOCKADDR, n) } == 0 {
        return 0;
    }
    let e = wsa_errno();
    if e != errno::EAGAIN && e != errno::EINPROGRESS {
        return errno::fail(e);
    }
    if f.nonblock() {
        return errno::fail(errno::EINPROGRESS);
    }
    let Obj::Socket(sock) = &f.obj else { unreachable!() };
    if let Err(e) = thread::wait_for(None, || (sock.poll1(super::io::POLLOUT, 0) != 0).then_some(())) {
        return errno::fail(e);
    }
    let mut err = 0i32;
    let mut l = 4i32;
    // SO_ERROR
    unsafe { ws::getsockopt(s, SOL_SOCKET_WIN, 0x1007, &mut err as *mut i32 as *mut u8, &mut l) };
    if err != 0 {
        unsafe { ws::WSASetLastError(err) };
        return errno::fail(wsa_errno());
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_listen(fdn: c_int, backlog: c_int) -> c_int {
    match sock_of(fdn) {
        Ok((_, s)) => {
            if unsafe { ws::listen(s, backlog) } != 0 { errno::fail(wsa_errno()) } else { 0 }
        }
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_accept4(fdn: c_int, addr: *mut u8, len: *mut u32, flags: c_int) -> c_int {
    let (f, s) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail(e),
    };
    let Obj::Socket(sock) = &f.obj else { unreachable!() };
    let mut storage = [0u8; 128];
    let mut slen = 128i32;
    let r = sock.blocking(f.nonblock(), super::io::POLLIN, || {
        slen = 128;
        let c = unsafe { ws::accept(s, storage.as_mut_ptr() as *mut ws::SOCKADDR, &mut slen) };
        if c == ws::INVALID_SOCKET { Err(wsa_errno()) } else { Ok(c) }
    });
    match r {
        Ok(c) => {
            if !addr.is_null() && !len.is_null() {
                unsafe { write_addr(&storage, slen as u32, addr, len) };
            }
            add_socket(c, sock.family, sock.ty, sock.proto, flags)
        }
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_accept(fdn: c_int, addr: *mut u8, len: *mut u32) -> c_int {
    unsafe { blinkwin_accept4(fdn, addr, len, 0) }
}

#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_shutdown(fdn: c_int, how: c_int) -> c_int {
    match sock_of(fdn) {
        Ok((_, s)) => {
            if unsafe { ws::shutdown(s, how) } != 0 { errno::fail(wsa_errno()) } else { 0 }
        }
        Err(e) => errno::fail(e),
    }
}

unsafe fn name_call(fdn: c_int, addr: *mut u8, len: *mut u32, peer: bool) -> c_int {
    let (_f, s) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail(e),
    };
    let mut storage = [0u8; 128];
    let mut slen = 128i32;
    let r = unsafe {
        if peer {
            ws::getpeername(s, storage.as_mut_ptr() as *mut ws::SOCKADDR, &mut slen)
        } else {
            ws::getsockname(s, storage.as_mut_ptr() as *mut ws::SOCKADDR, &mut slen)
        }
    };
    if r != 0 {
        return errno::fail(wsa_errno());
    }
    unsafe { write_addr(&storage, slen as u32, addr, len) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_getsockname(fdn: c_int, addr: *mut u8, len: *mut u32) -> c_int {
    unsafe { name_call(fdn, addr, len, false) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_getpeername(fdn: c_int, addr: *mut u8, len: *mut u32) -> c_int {
    unsafe { name_call(fdn, addr, len, true) }
}

/// Maps a Linux (level, option) to Winsock's, or None to accept silently.
fn sockopt(level: c_int, opt: c_int) -> Option<(c_int, c_int)> {
    match (level, opt) {
        (SOL_SOCKET_LINUX, 2) => Some((SOL_SOCKET_WIN, 4)),       // SO_REUSEADDR
        (SOL_SOCKET_LINUX, 3) => Some((SOL_SOCKET_WIN, 0x1008)),  // SO_TYPE
        (SOL_SOCKET_LINUX, 4) => Some((SOL_SOCKET_WIN, 0x1007)),  // SO_ERROR
        (SOL_SOCKET_LINUX, 6) => Some((SOL_SOCKET_WIN, 0x20)),    // SO_BROADCAST
        (SOL_SOCKET_LINUX, 7) => Some((SOL_SOCKET_WIN, 0x1001)),  // SO_SNDBUF
        (SOL_SOCKET_LINUX, 8) => Some((SOL_SOCKET_WIN, 0x1002)),  // SO_RCVBUF
        (SOL_SOCKET_LINUX, 9) => Some((SOL_SOCKET_WIN, 8)),       // SO_KEEPALIVE
        (SOL_SOCKET_LINUX, 10) => Some((SOL_SOCKET_WIN, 0x100)),  // SO_OOBINLINE
        (SOL_SOCKET_LINUX, 13) => Some((SOL_SOCKET_WIN, 0x80)),   // SO_LINGER
        (SOL_SOCKET_LINUX, 30) => Some((SOL_SOCKET_WIN, 2)),      // SO_ACCEPTCONN
        (6, 1) => Some((6, 1)),                                   // TCP_NODELAY
        (6, 4) => Some((6, 3)),                                   // TCP_KEEPIDLE
        (6, 5) => Some((6, 17)),                                  // TCP_KEEPINTVL
        (6, 6) => Some((6, 16)),                                  // TCP_KEEPCNT
        (41, 26) => Some((41, 27)),                               // IPV6_V6ONLY
        (0, 1) => Some((0, 3)),                                   // IP_TOS
        (0, 2) => Some((0, 4)),                                   // IP_TTL
        _ => None,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_setsockopt(fdn: c_int, level: c_int, opt: c_int, val: *const u8, len: u32) -> c_int {
    let (_f, s) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail(e),
    };
    let Some((l, o)) = sockopt(level, opt) else { return 0 };
    if unsafe { ws::setsockopt(s, l, o, val, len as i32) } != 0 { errno::fail(wsa_errno()) } else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_getsockopt(fdn: c_int, level: c_int, opt: c_int, val: *mut u8, len: *mut u32) -> c_int {
    let (f, s) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail(e),
    };
    let Obj::Socket(sock) = &f.obj else { unreachable!() };
    let put = |v: c_int| unsafe {
        if *len >= 4 {
            *(val as *mut c_int) = v;
            *len = 4;
        }
        0
    };
    match (level, opt) {
        (SOL_SOCKET_LINUX, 39) => return put(sock.family), // SO_DOMAIN
        (SOL_SOCKET_LINUX, 38) => return put(sock.proto),  // SO_PROTOCOL
        (SOL_SOCKET_LINUX, 3) => return put(sock.ty),      // SO_TYPE
        _ => {}
    }
    let Some((l, o)) = sockopt(level, opt) else { return put(0) };
    let mut n = unsafe { *len } as i32;
    if unsafe { ws::getsockopt(s, l, o, val, &mut n) } != 0 {
        return errno::fail(wsa_errno());
    }
    unsafe { *len = n as u32 };
    if (level, opt) == (SOL_SOCKET_LINUX, 4) {
        // SO_ERROR holds a Winsock error; report it as errno
        let e = unsafe { *(val as *const i32) };
        if e != 0 {
            unsafe { ws::WSASetLastError(e) };
            unsafe { *(val as *mut i32) = wsa_errno() };
        }
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_sendto(fdn: c_int, buf: *const u8, n: usize, flags: c_int, addr: *const u8, len: u32) -> isize {
    let (f, _) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail_isize(e),
    };
    let Obj::Socket(s) = &f.obj else { unreachable!() };
    s.sendto(unsafe { std::slice::from_raw_parts(buf, n) }, flags, f.nonblock(), addr, len).map_or_else(errno::fail_isize, |n| n as isize)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_recvfrom(fdn: c_int, buf: *mut u8, n: usize, flags: c_int, addr: *mut u8, len: *mut u32) -> isize {
    let (f, _) = match sock_of(fdn) {
        Ok(x) => x,
        Err(e) => return errno::fail_isize(e),
    };
    let Obj::Socket(s) = &f.obj else { unreachable!() };
    s.recvfrom(unsafe { std::slice::from_raw_parts_mut(buf, n) }, flags, f.nonblock(), addr, len).map_or_else(errno::fail_isize, |n| n as isize)
}

#[repr(C)]
pub struct Msghdr {
    name: *mut u8,
    namelen: u32,
    iov: *mut super::io::Iovec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
}

#[repr(C)]
struct RawIovec {
    base: *mut u8,
    len: usize,
}

/// Gathers the iovecs and sends once. Ancillary data isn't supported.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_sendmsg(fdn: c_int, m: *const Msghdr, flags: c_int) -> isize {
    let m = unsafe { &*m };
    let iov = unsafe { std::slice::from_raw_parts(m.iov as *const RawIovec, m.iovlen) };
    let mut all = Vec::new();
    for v in iov {
        all.extend_from_slice(unsafe { std::slice::from_raw_parts(v.base, v.len) });
    }
    let f = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail_isize(e),
    };
    match &f.obj {
        Obj::Socket(s) => s.sendto(&all, flags, f.nonblock(), m.name, m.namelen).map_or_else(errno::fail_isize, |n| n as isize),
        Obj::Pipe(_) => super::io::write_file(&f, &all).map_or_else(errno::fail_isize, |n| n as isize),
        _ => errno::fail_isize(errno::ENOTSOCK),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_recvmsg(fdn: c_int, m: *mut Msghdr, flags: c_int) -> isize {
    let m = unsafe { &mut *m };
    let total: usize = unsafe { std::slice::from_raw_parts(m.iov as *const RawIovec, m.iovlen) }.iter().map(|v| v.len).sum();
    let mut buf = vec![0u8; total];
    let f = match fd::get(fdn) {
        Ok(f) => f,
        Err(e) => return errno::fail_isize(e),
    };
    let r = match &f.obj {
        Obj::Socket(s) => s.recvfrom(&mut buf, flags, f.nonblock(), m.name, &mut m.namelen),
        Obj::Pipe(_) => {
            m.namelen = 0;
            super::io::read_file(&f, &mut buf)
        }
        _ => Err(errno::ENOTSOCK),
    };
    let n = match r {
        Ok(n) => n,
        Err(e) => return errno::fail_isize(e),
    };
    let mut off = 0;
    for v in unsafe { std::slice::from_raw_parts(m.iov as *const RawIovec, m.iovlen) } {
        let k = v.len.min(n - off);
        unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr().add(off), v.base, k) };
        off += k;
    }
    m.controllen = 0;
    m.flags = 0;
    n as isize
}

#[unsafe(no_mangle)]
pub extern "C" fn blinkwin_sockatmark(_fd: c_int) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_gethostname(buf: *mut c_char, n: usize) -> c_int {
    let name = b"xbox\0";
    if n < name.len() {
        return errno::fail(errno::EINVAL);
    }
    unsafe { std::ptr::copy_nonoverlapping(name.as_ptr(), buf as *mut u8, name.len()) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_inet_ntop(af: c_int, src: *const u8, dst: *mut c_char, size: u32) -> *const c_char {
    let s = match af {
        AF_INET => {
            let b = unsafe { std::slice::from_raw_parts(src, 4) };
            std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string()
        }
        10 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(unsafe { std::slice::from_raw_parts(src, 16) });
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => {
            errno::set(errno::EAFNOSUPPORT);
            return std::ptr::null();
        }
    };
    if s.len() + 1 > size as usize {
        errno::set(errno::ENOSPC);
        return std::ptr::null();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, s.len());
        *dst.add(s.len()) = 0;
    }
    dst
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn blinkwin_inet_pton(af: c_int, src: *const c_char, dst: *mut u8) -> c_int {
    let s = unsafe { CStr::from_ptr(src) }.to_string_lossy();
    match af {
        AF_INET => match s.parse::<std::net::Ipv4Addr>() {
            Ok(a) => {
                unsafe { std::ptr::copy_nonoverlapping(a.octets().as_ptr(), dst, 4) };
                1
            }
            Err(_) => 0,
        },
        10 => match s.parse::<std::net::Ipv6Addr>() {
            Ok(a) => {
                unsafe { std::ptr::copy_nonoverlapping(a.octets().as_ptr(), dst, 16) };
                1
            }
            Err(_) => 0,
        },
        _ => errno::fail(errno::EAFNOSUPPORT),
    }
}
