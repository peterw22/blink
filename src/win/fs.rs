//! Files and directories by path: open, stat, readdir, link/unlink/rename,
//! symlinks, timestamps, the current directory.
//!
//! Paths arrive Linux-style from blink's hostfs (the guest root is a host
//! directory) and become Windows paths here. Symlinks can't be created in
//! a UWP app, so symlink() writes a marker file (SYSTEM attribute, starting
//! with LINK_MAGIC) that readlink()/stat() understand; real Windows or Wine
//! symlinks are read too.

use super::errno;
use super::fd::{self, Obj, OpenFile, O_ACCMODE, O_APPEND, O_CLOEXEC, O_CREAT, O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_PATH, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use super::time::Timespec;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::IO::DeviceIoControl;

pub const AT_FDCWD: c_int = -100;
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
const AT_REMOVEDIR: c_int = 0x200;
const AT_EMPTY_PATH: c_int = 0x1000;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const DELETE: u32 = 0x0001_0000;
const READ_ATTRIBUTES: u32 = 0x80;
const WRITE_ATTRIBUTES: u32 = 0x100;
const SHARE_ALL: u32 = 7;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const OPEN_REPARSE: u32 = 0x0020_0000;
const OVERLAPPED_IO: u32 = 0x4000_0000;
const ATTR_READONLY: u32 = 0x1;
const ATTR_SYSTEM: u32 = 0x4;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE: u32 = 0x400;

pub const LINK_MAGIC: &[u8] = b"!<blink-symlink>\n";

static CWD: Mutex<String> = Mutex::new(String::new());

pub fn init() {
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "C:\\".into());
    *CWD.lock().unwrap() = cwd;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

fn cstr(p: *const c_char) -> Result<String, c_int> {
    if p.is_null() {
        return Err(errno::EFAULT);
    }
    // SAFETY: blink passes NUL-terminated strings
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    if s.is_empty() { Err(errno::ENOENT) } else { Ok(s) }
}

fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with('\\') || (p.len() >= 2 && p.as_bytes()[1] == b':')
}

fn cwd() -> String {
    CWD.lock().unwrap().clone()
}

/// A Windows path as blink should see it: forward slashes, and no drive
/// letter when it's the current drive (resolve() puts it back).
pub fn to_posix(win: &str) -> String {
    let c = cwd();
    let p = if c.len() >= 2 && win.len() >= 2 && win.as_bytes()[1] == b':' && win[..1].eq_ignore_ascii_case(&c[..1]) {
        &win[2..]
    } else {
        win
    };
    let p = p.replace('\\', "/");
    if p.is_empty() { "/".into() } else { p }
}

/// Joins a Linux-style path onto the directory `dirfd` (or the cwd) and
/// returns it as a Windows path.
pub fn resolve(dirfd: c_int, path: &str) -> Result<String, c_int> {
    let mut p = if is_absolute(path) {
        if path.starts_with('/') || path.starts_with('\\') {
            // root of the current drive, as Windows would
            let c = cwd();
            let drive = if c.len() >= 2 && c.as_bytes()[1] == b':' { &c[..2] } else { "" };
            format!("{drive}{path}")
        } else {
            path.to_string()
        }
    } else {
        let base = if dirfd == AT_FDCWD {
            cwd()
        } else {
            match &fd::get(dirfd)?.obj {
                Obj::Dir { path, .. } => path.clone(),
                _ => return Err(errno::ENOTDIR),
            }
        };
        format!("{}\\{}", base.trim_end_matches(['\\', '/']), path)
    };
    p = p.replace('/', "\\");
    while p.len() > 3 && p.ends_with('\\') {
        p.pop();
    }
    Ok(p)
}

fn open_handle(path: &str, access: u32, disposition: u32, flags: u32) -> Result<HANDLE, c_int> {
    let w = wide(path);
    // SAFETY: plain Win32 call with a NUL-terminated wide path
    let h = unsafe { CreateFileW(w.as_ptr(), access, SHARE_ALL, std::ptr::null(), disposition, flags, std::ptr::null_mut()) };
    if h == INVALID_HANDLE_VALUE {
        Err(errno::from_win32(unsafe { GetLastError() }))
    } else {
        Ok(h)
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: owned handle
        unsafe { CloseHandle(self.0) };
    }
}

fn info(h: HANDLE) -> Result<BY_HANDLE_FILE_INFORMATION, c_int> {
    // SAFETY: zeroed POD out-parameter
    let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h, &mut i) } == 0 {
        return Err(errno::from_win32(unsafe { GetLastError() }));
    }
    Ok(i)
}

fn reparse_tag(h: HANDLE) -> u32 {
    let mut t = FILE_ATTRIBUTE_TAG_INFO { FileAttributes: 0, ReparseTag: 0 };
    // SAFETY: correct size for the info class
    if unsafe { GetFileInformationByHandleEx(h, FileAttributeTagInfo, &mut t as *mut _ as *mut c_void, std::mem::size_of_val(&t) as u32) } == 0 {
        return 0;
    }
    t.ReparseTag
}

/// Symlink reparse tags. Mount points and junctions (0xA0000003) are
/// directories as far as the guest is concerned.
fn is_link_tag(tag: u32) -> bool {
    matches!(tag, 0xA000_000C | 0xA000_001D) // SYMLINK, LX_SYMLINK
}

/// Reads the target of a symlink (any kind) through an open handle.
/// Marker files are recognized by content (small, starting with
/// LINK_MAGIC): their SYSTEM attribute doesn't survive every copy.
fn link_target(h: HANDLE, attrs: u32, size: i64) -> Option<String> {
    if attrs & ATTR_REPARSE != 0 {
        let mut buf = vec![0u8; 16384];
        let mut n = 0u32;
        // FSCTL_GET_REPARSE_POINT
        let ok = unsafe { DeviceIoControl(h, 0x900A8, std::ptr::null(), 0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut n, std::ptr::null_mut()) };
        if ok == 0 || n < 8 {
            return None;
        }
        let tag = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let u16at = |o: usize| u16::from_le_bytes(buf[o..o + 2].try_into().unwrap()) as usize;
        let utf16 = |off: usize, len: usize| -> String {
            let w: Vec<u16> = buf[off..off + len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&w)
        };
        let target = match tag {
            0xA000_001D => String::from_utf8_lossy(&buf[12..8 + u16at(4)]).into_owned(), // LX: version, utf-8
            0xA000_000C => utf16(20 + u16at(12), u16at(14)),                            // SYMLINK print name
            _ => return None,
        };
        return Some(target.replace('\\', "/"));
    }
    if attrs & ATTR_DIRECTORY == 0 && size >= LINK_MAGIC.len() as i64 && size < 4096 {
        let data = read_small(h, 4096)?;
        return data.strip_prefix(LINK_MAGIC).map(|t| String::from_utf8_lossy(t).into_owned());
    }
    None
}

fn file_size(i: &BY_HANDLE_FILE_INFORMATION) -> i64 {
    ((i.nFileSizeHigh as i64) << 32) | i.nFileSizeLow as i64
}

/// Reads up to `max` bytes from the start of a (possibly overlapped) file.
fn read_small(h: HANDLE, max: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; max];
    let n = super::io::read_at(h, &mut buf, 0).ok()?;
    buf.truncate(n);
    Some(buf)
}

#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_nlink: u64,
    pub st_mode: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atim: Timespec,
    pub st_mtim: Timespec,
    pub st_ctim: Timespec,
}

fn ft_to_ts(ft: FILETIME) -> Timespec {
    let t = (((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64) - 116_444_736_000_000_000;
    Timespec { tv_sec: t.div_euclid(10_000_000), tv_nsec: (t.rem_euclid(10_000_000) * 100) as i32 }
}

pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;

/// Fills `st` for a file/directory handle.
pub fn stat_handle(h: HANDLE, st: &mut Stat) -> Result<(), c_int> {
    let i = info(h)?;
    let size = ((i.nFileSizeHigh as i64) << 32) | i.nFileSizeLow as i64;
    let mut mode;
    let mut size = size;
    if i.dwFileAttributes & ATTR_REPARSE != 0 && is_link_tag(reparse_tag(h)) {
        mode = S_IFLNK | 0o777;
        size = link_target(h, i.dwFileAttributes, size).map_or(0, |t| t.len() as i64);
    } else if i.dwFileAttributes & ATTR_DIRECTORY != 0 {
        mode = S_IFDIR | 0o755;
    } else {
        // Windows has no execute bit; everything in a rootfs may need it
        mode = S_IFREG | 0o755;
        if size < 4096 {
            if let Some(t) = link_target(h, i.dwFileAttributes, size) {
                mode = S_IFLNK | 0o777;
                size = t.len() as i64;
            }
        }
    }
    if i.dwFileAttributes & ATTR_READONLY != 0 && mode & S_IFLNK != S_IFLNK {
        mode &= !0o222;
    }
    *st = Stat {
        st_dev: i.dwVolumeSerialNumber as u64,
        st_ino: ((i.nFileIndexHigh as u64) << 32) | i.nFileIndexLow as u64,
        st_nlink: i.nNumberOfLinks as u64,
        st_mode: mode,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        st_size: size,
        st_blksize: 4096,
        st_blocks: (size + 511) / 512,
        st_atim: ft_to_ts(i.ftLastAccessTime),
        st_mtim: ft_to_ts(i.ftLastWriteTime),
        st_ctim: ft_to_ts(i.ftLastWriteTime),
    };
    Ok(())
}

fn stat_path(path: &str, follow: bool, st: &mut Stat) -> Result<(), c_int> {
    let h = Handle(open_handle(path, READ_ATTRIBUTES | GENERIC_READ, OPEN_EXISTING, BACKUP_SEMANTICS | if follow { 0 } else { OPEN_REPARSE })
        .or_else(|_| open_handle(path, READ_ATTRIBUTES, OPEN_EXISTING, BACKUP_SEMANTICS | if follow { 0 } else { OPEN_REPARSE }))?);
    stat_handle(h.0, st)
}

fn std_mode(h: HANDLE) -> u32 {
    // SAFETY: plain Win32 call
    match unsafe { GetFileType(h) } {
        2 => S_IFCHR | 0o620, // FILE_TYPE_CHAR (console)
        3 => S_IFIFO | 0o600, // FILE_TYPE_PIPE
        _ => S_IFREG | 0o644,
    }
}

pub fn fstat_file(f: &OpenFile, st: &mut Stat) -> Result<(), c_int> {
    // SAFETY: zeroed POD
    *st = unsafe { std::mem::zeroed() };
    st.st_blksize = 4096;
    match &f.obj {
        Obj::File { handle, .. } | Obj::Dir { handle, .. } => stat_handle(*handle, st),
        Obj::Std { handle } => {
            if stat_handle(*handle, st).is_err() || std_mode(*handle) & S_IFREG != S_IFREG {
                st.st_mode = std_mode(*handle);
            }
            Ok(())
        }
        Obj::Pipe(_) => {
            st.st_mode = S_IFIFO | 0o600;
            Ok(())
        }
        Obj::Socket(_) => {
            st.st_mode = S_IFSOCK | 0o777;
            Ok(())
        }
        Obj::Tty(_) => {
            st.st_mode = S_IFCHR | 0o620;
            st.st_rdev = (136 << 8) | 0; // /dev/pts/0
            Ok(())
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstatat(dirfd: c_int, path: *const c_char, st: *mut Stat, flags: c_int) -> c_int {
    let st = unsafe { &mut *st };
    if flags & AT_EMPTY_PATH != 0 && !path.is_null() && unsafe { *path } == 0 {
        return unsafe { fstat(dirfd, st) };
    }
    let r = cstr(path).and_then(|p| resolve(dirfd, &p)).and_then(|p| {
        let r = stat_path(&p, flags & AT_SYMLINK_NOFOLLOW == 0, st);
        super::trace!("fstatat({dirfd}, {p:?}) -> {:?} mode={:o}", r, st.st_mode);
        r
    });
    r.map_or_else(errno::fail, |_| 0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat(fdn: c_int, st: *mut Stat) -> c_int {
    match fd::get(fdn) {
        Ok(f) => fstat_file(&f, unsafe { &mut *st }).map_or_else(errno::fail, |_| 0),
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat(path: *const c_char, st: *mut Stat) -> c_int {
    unsafe { fstatat(AT_FDCWD, path, st, 0) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat(path: *const c_char, st: *mut Stat) -> c_int {
    unsafe { fstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW) }
}

/// Opens `path` (already resolved) the way openat() would.
pub fn open_path(path: &str, flags: u32) -> Result<std::sync::Arc<OpenFile>, c_int> {
    let acc = flags & O_ACCMODE;
    let mut access = READ_ATTRIBUTES | WRITE_ATTRIBUTES;
    if flags & O_PATH == 0 {
        access |= match acc {
            O_RDONLY => GENERIC_READ,
            O_WRONLY => GENERIC_WRITE,
            _ => GENERIC_READ | GENERIC_WRITE,
        };
    }
    let disposition = if flags & O_CREAT != 0 {
        if flags & O_EXCL != 0 {
            CREATE_NEW
        } else if flags & O_TRUNC != 0 {
            CREATE_ALWAYS
        } else {
            OPEN_ALWAYS
        }
    } else if flags & O_TRUNC != 0 && acc != O_RDONLY {
        TRUNCATE_EXISTING
    } else {
        OPEN_EXISTING
    };
    let nofollow = if flags & O_NOFOLLOW != 0 { OPEN_REPARSE } else { 0 };
    let h = match open_handle(path, access, disposition, BACKUP_SEMANTICS | OVERLAPPED_IO | nofollow) {
        Ok(h) => h,
        // directories can't be opened for writing
        Err(errno::EACCES) if acc != O_RDONLY => match open_handle(path, READ_ATTRIBUTES, OPEN_EXISTING, BACKUP_SEMANTICS) {
            Ok(h) => {
                let dir = info(h).map(|i| i.dwFileAttributes & ATTR_DIRECTORY != 0).unwrap_or(false);
                unsafe { CloseHandle(h) };
                return Err(if dir { errno::EISDIR } else { errno::EACCES });
            }
            Err(_) => return Err(errno::EACCES),
        },
        Err(e) => return Err(e),
    };
    let i = match info(h) {
        Ok(i) => i,
        Err(e) => {
            unsafe { CloseHandle(h) };
            return Err(e);
        }
    };
    if nofollow != 0 && ((i.dwFileAttributes & ATTR_REPARSE != 0 && is_link_tag(reparse_tag(h))) || link_target(h, i.dwFileAttributes, file_size(&i)).is_some()) && flags & O_PATH == 0 {
        unsafe { CloseHandle(h) };
        return Err(errno::ELOOP);
    }
    if i.dwFileAttributes & ATTR_DIRECTORY != 0 {
        return Ok(OpenFile::new(Obj::Dir { path: path.to_string(), handle: h }, flags));
    }
    if flags & O_DIRECTORY != 0 {
        unsafe { CloseHandle(h) };
        return Err(errno::ENOTDIR);
    }
    Ok(OpenFile::new(Obj::File { handle: h, pos: Mutex::new(0) }, flags & !(O_ACCMODE) | if flags & O_PATH != 0 { O_RDONLY } else { acc } | (flags & O_APPEND)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn openat(dirfd: c_int, path: *const c_char, flags: c_int, _mode: u32) -> c_int {
    let flags = flags as u32;
    let r = cstr(path).and_then(|p| resolve(dirfd, &p)).and_then(|p| {
        let r = open_path(&p, flags);
        super::trace!("openat({dirfd}, {p:?}, {flags:#o}) -> {:?}", r.as_ref().map(|_| ()));
        r
    });
    match r {
        Ok(f) => fd::add(f, flags & O_CLOEXEC != 0),
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: u32) -> c_int {
    unsafe { openat(AT_FDCWD, path, flags, mode) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn creat(path: *const c_char, mode: u32) -> c_int {
    unsafe { openat(AT_FDCWD, path, (O_CREAT | O_WRONLY | O_TRUNC) as c_int, mode) }
}

fn with_path(dirfd: c_int, path: *const c_char, f: impl FnOnce(&str) -> Result<c_int, c_int>) -> c_int {
    let r = cstr(path).and_then(|p| resolve(dirfd, &p)).and_then(|p| {
        let r = f(&p);
        super::trace!("{p:?} -> {r:?}");
        r
    });
    match r {
        Ok(r) => r,
        Err(e) => errno::fail(e),
    }
}

fn win_ok(ok: i32) -> Result<c_int, c_int> {
    if ok != 0 { Ok(0) } else { Err(errno::from_win32(unsafe { GetLastError() })) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, _mode: u32) -> c_int {
    with_path(dirfd, path, |p| win_ok(unsafe { CreateDirectoryW(wide(p).as_ptr(), std::ptr::null()) }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkdir(path: *const c_char, mode: u32) -> c_int {
    unsafe { mkdirat(AT_FDCWD, path, mode) }
}

/// Deletes with POSIX semantics (the name goes away even while open),
/// falling back to the classic Windows calls.
fn delete(path: &str, dir: bool) -> Result<c_int, c_int> {
    if let Ok(h) = open_handle(path, DELETE | READ_ATTRIBUTES, OPEN_EXISTING, BACKUP_SEMANTICS | OPEN_REPARSE) {
        let h = Handle(h);
        let is_dir = info(h.0).map(|i| i.dwFileAttributes & ATTR_DIRECTORY != 0 && i.dwFileAttributes & ATTR_REPARSE == 0).unwrap_or(false);
        if is_dir != dir {
            return Err(if dir { errno::ENOTDIR } else { errno::EISDIR });
        }
        // FILE_DISPOSITION_DELETE | POSIX_SEMANTICS | IGNORE_READONLY
        let flags: u32 = 1 | 2 | 0x10;
        if unsafe { SetFileInformationByHandle(h.0, FileDispositionInfoEx, &flags as *const u32 as *const c_void, 4) } != 0 {
            return Ok(0);
        }
        let del: u8 = 1;
        if unsafe { SetFileInformationByHandle(h.0, FileDispositionInfo, &del as *const u8 as *const c_void, 1) } != 0 {
            return Ok(0);
        }
    }
    let w = wide(path);
    if dir {
        win_ok(unsafe { RemoveDirectoryW(w.as_ptr()) })
    } else {
        win_ok(unsafe { DeleteFileW(w.as_ptr()) })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    with_path(dirfd, path, |p| delete(p, flags & AT_REMOVEDIR != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    unsafe { unlinkat(AT_FDCWD, path, 0) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    unsafe { unlinkat(AT_FDCWD, path, AT_REMOVEDIR) }
}

fn rename_paths(from: &str, to: &str) -> Result<c_int, c_int> {
    // POSIX rename: atomically replaces `to`, even if it's open
    if let Ok(h) = open_handle(from, DELETE | READ_ATTRIBUTES, OPEN_EXISTING, BACKUP_SEMANTICS | OPEN_REPARSE) {
        let h = Handle(h);
        let name: Vec<u16> = to.encode_utf16().collect();
        // FILE_RENAME_INFO: Flags(u32) + pad, RootDirectory, FileNameLength, FileName[]
        let mut buf = vec![0u8; 24 + name.len() * 2 + 2];
        let flags: u32 = 1 | 2; // REPLACE_IF_EXISTS | POSIX_SEMANTICS
        buf[0..4].copy_from_slice(&flags.to_le_bytes());
        buf[16..20].copy_from_slice(&((name.len() * 2) as u32).to_le_bytes());
        for (i, c) in name.iter().enumerate() {
            buf[20 + i * 2..22 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        if unsafe { SetFileInformationByHandle(h.0, FileRenameInfoEx, buf.as_ptr() as *const c_void, buf.len() as u32) } != 0 {
            return Ok(0);
        }
    }
    // MOVEFILE_REPLACE_EXISTING
    win_ok(unsafe { MoveFileExW(wide(from).as_ptr(), wide(to).as_ptr(), 1) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn renameat(olddir: c_int, old: *const c_char, newdir: c_int, new: *const c_char) -> c_int {
    let r = cstr(old)
        .and_then(|o| resolve(olddir, &o))
        .and_then(|o| cstr(new).and_then(|n| resolve(newdir, &n)).map(|n| (o, n)))
        .and_then(|(o, n)| rename_paths(&o, &n));
    r.map_or_else(errno::fail, |r| r)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn linkat(olddir: c_int, old: *const c_char, newdir: c_int, new: *const c_char, _flags: c_int) -> c_int {
    let r = cstr(old)
        .and_then(|o| resolve(olddir, &o))
        .and_then(|o| cstr(new).and_then(|n| resolve(newdir, &n)).map(|n| (o, n)))
        .and_then(|(o, n)| win_ok(unsafe { CreateHardLinkW(wide(&n).as_ptr(), wide(&o).as_ptr(), std::ptr::null()) }));
    r.map_or_else(errno::fail, |r| r)
}

/// Creates a symlink marker file holding the target verbatim.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn symlinkat(target: *const c_char, dirfd: c_int, path: *const c_char) -> c_int {
    let t = match cstr(target) {
        Ok(t) => t,
        Err(e) => return errno::fail(e),
    };
    with_path(dirfd, path, |p| {
        let w = wide(p);
        // FILE_ATTRIBUTE_SYSTEM; CREATE_NEW
        let h = unsafe { CreateFileW(w.as_ptr(), GENERIC_WRITE, SHARE_ALL, std::ptr::null(), CREATE_NEW, ATTR_SYSTEM, std::ptr::null_mut()) };
        if h == INVALID_HANDLE_VALUE {
            return Err(errno::from_win32(unsafe { GetLastError() }));
        }
        let h = Handle(h);
        let mut data = LINK_MAGIC.to_vec();
        data.extend_from_slice(t.as_bytes());
        let mut n = 0u32;
        win_ok(unsafe { WriteFile(h.0, data.as_ptr(), data.len() as u32, &mut n, std::ptr::null_mut()) })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn readlinkat(dirfd: c_int, path: *const c_char, buf: *mut c_char, size: usize) -> isize {
    let r = cstr(path).and_then(|p| resolve(dirfd, &p)).and_then(|p| {
        let h = Handle(open_handle(&p, READ_ATTRIBUTES | GENERIC_READ, OPEN_EXISTING, BACKUP_SEMANTICS | OPEN_REPARSE | OVERLAPPED_IO)?);
        let i = info(h.0)?;
        link_target(h.0, i.dwFileAttributes, file_size(&i)).ok_or(errno::EINVAL)
    });
    match r {
        Ok(t) => {
            let n = t.len().min(size);
            unsafe { std::ptr::copy_nonoverlapping(t.as_ptr(), buf as *mut u8, n) };
            n as isize
        }
        Err(e) => errno::fail_isize(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn readlink(path: *const c_char, buf: *mut c_char, size: usize) -> isize {
    unsafe { readlinkat(AT_FDCWD, path, buf, size) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn faccessat(dirfd: c_int, path: *const c_char, mode: c_int, _flags: c_int) -> c_int {
    with_path(dirfd, path, |p| {
        let mut st: Stat = unsafe { std::mem::zeroed() };
        stat_path(p, true, &mut st)?;
        if mode & 2 != 0 && st.st_mode & 0o200 == 0 {
            return Err(errno::EACCES);
        }
        Ok(0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    unsafe { faccessat(AT_FDCWD, path, mode, 0) }
}

// ownership and permission bits aren't kept on Windows
#[unsafe(no_mangle)]
pub extern "C" fn fchmodat(_d: c_int, _p: *const c_char, _m: u32, _f: c_int) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn fchmod(_fd: c_int, _m: u32) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn fchownat(_d: c_int, _p: *const c_char, _u: u32, _g: u32, _f: c_int) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn fchown(_fd: c_int, _u: u32, _g: u32) -> c_int {
    0
}

static UMASK: Mutex<u32> = Mutex::new(0o022);

#[unsafe(no_mangle)]
pub extern "C" fn umask(m: u32) -> u32 {
    std::mem::replace(&mut *UMASK.lock().unwrap(), m & 0o777)
}

const UTIME_NOW: i32 = (1 << 30) - 1;
const UTIME_OMIT: i32 = (1 << 30) - 2;

fn ts_to_ft(ts: &Timespec) -> Option<FILETIME> {
    if ts.tv_nsec == UTIME_OMIT {
        return None;
    }
    let t = if ts.tv_nsec == UTIME_NOW { super::time::realtime_ns() / 100 } else { ts.tv_sec * 10_000_000 + ts.tv_nsec as i64 / 100 };
    let t = t + 116_444_736_000_000_000;
    Some(FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 })
}

fn set_times(h: HANDLE, ts: *const Timespec) -> Result<c_int, c_int> {
    let now = Timespec { tv_sec: 0, tv_nsec: UTIME_NOW };
    let (a, m) = if ts.is_null() { (now, now) } else { unsafe { (*ts, *ts.add(1)) } };
    let (a, m) = (ts_to_ft(&a), ts_to_ft(&m));
    let p = |f: &Option<FILETIME>| f.as_ref().map_or(std::ptr::null(), |f| f as *const FILETIME);
    win_ok(unsafe { SetFileTime(h, std::ptr::null(), p(&a), p(&m)) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn utimensat(dirfd: c_int, path: *const c_char, ts: *const Timespec, flags: c_int) -> c_int {
    if path.is_null() {
        return unsafe { futimens(dirfd, ts) };
    }
    with_path(dirfd, path, |p| {
        let nofollow = if flags & AT_SYMLINK_NOFOLLOW != 0 { OPEN_REPARSE } else { 0 };
        let h = Handle(open_handle(p, WRITE_ATTRIBUTES, OPEN_EXISTING, BACKUP_SEMANTICS | nofollow)?);
        set_times(h.0, ts)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn futimens(fdn: c_int, ts: *const Timespec) -> c_int {
    match fd::handle(fdn) {
        Ok(h) => set_times(h, ts).map_or_else(errno::fail, |r| r),
        Err(e) => errno::fail(e),
    }
}

fn set_size(h: HANDLE, len: i64) -> Result<c_int, c_int> {
    if len < 0 {
        return Err(errno::EINVAL);
    }
    win_ok(unsafe { SetFileInformationByHandle(h, FileEndOfFileInfo, &len as *const i64 as *const c_void, 8) })
}

#[unsafe(no_mangle)]
pub extern "C" fn ftruncate(fdn: c_int, len: i64) -> c_int {
    match fd::handle(fdn) {
        Ok(h) => set_size(h, len).map_or_else(errno::fail, |r| r),
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn truncate(path: *const c_char, len: i64) -> c_int {
    with_path(AT_FDCWD, path, |p| {
        let h = Handle(open_handle(p, GENERIC_WRITE, OPEN_EXISTING, BACKUP_SEMANTICS)?);
        set_size(h.0, len)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn fsync(fdn: c_int) -> c_int {
    match fd::handle(fdn) {
        Ok(h) => {
            unsafe { FlushFileBuffers(h) };
            0
        }
        Err(_) if fd::get(fdn).is_ok() => 0,
        Err(e) => errno::fail(e),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fdatasync(fdn: c_int) -> c_int {
    fsync(fdn)
}

#[unsafe(no_mangle)]
pub extern "C" fn sync() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: usize) -> *mut c_char {
    let c = to_posix(&cwd());
    if c.len() + 1 > size {
        errno::set(errno::ERANGE);
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::copy_nonoverlapping(c.as_ptr(), buf as *mut u8, c.len());
        *buf.add(c.len()) = 0;
    }
    buf
}

fn set_cwd(path: String) -> Result<c_int, c_int> {
    let mut st: Stat = unsafe { std::mem::zeroed() };
    stat_path(&path, true, &mut st)?;
    if st.st_mode & 0o170000 != S_IFDIR {
        return Err(errno::ENOTDIR);
    }
    *CWD.lock().unwrap() = path;
    Ok(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn chdir(path: *const c_char) -> c_int {
    with_path(AT_FDCWD, path, |p| set_cwd(p.to_string()))
}

#[unsafe(no_mangle)]
pub extern "C" fn fchdir(fdn: c_int) -> c_int {
    match fd::get(fdn).map(|f| match &f.obj {
        Obj::Dir { path, .. } => Ok(path.clone()),
        _ => Err(errno::ENOTDIR),
    }) {
        Ok(Ok(p)) => set_cwd(p).map_or_else(errno::fail, |r| r),
        Ok(Err(e)) | Err(e) => errno::fail(e),
    }
}

extern "C" {
    fn malloc(n: usize) -> *mut c_void;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realpath(path: *const c_char, out: *mut c_char) -> *mut c_char {
    let r = cstr(path).and_then(|p| resolve(AT_FDCWD, &p)).and_then(|p| {
        let w = wide(&p);
        let mut buf = vec![0u16; 32768];
        let n = unsafe { GetFullPathNameW(w.as_ptr(), buf.len() as u32, buf.as_mut_ptr(), std::ptr::null_mut()) };
        if n == 0 {
            return Err(errno::ENOENT);
        }
        let full = from_wide(&buf);
        let mut st: Stat = unsafe { std::mem::zeroed() };
        stat_path(&full, true, &mut st)?;
        Ok(to_posix(&full))
    });
    match r {
        Ok(s) => {
            let dst = if out.is_null() { unsafe { malloc(s.len() + 1) as *mut c_char } } else { out };
            unsafe {
                std::ptr::copy_nonoverlapping(s.as_ptr(), dst as *mut u8, s.len().min(4095));
                *dst.add(s.len().min(4095)) = 0;
            }
            dst
        }
        Err(e) => {
            errno::set(e);
            std::ptr::null_mut()
        }
    }
}

#[repr(C)]
pub struct Statvfs {
    bsize: u64,
    frsize: u64,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    favail: u64,
    fsid: u64,
    flag: u64,
    namemax: u64,
}

fn statvfs_path(path: &str, out: &mut Statvfs) -> Result<c_int, c_int> {
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    win_ok(unsafe { GetDiskFreeSpaceExW(wide(path).as_ptr(), &mut avail, &mut total, &mut free) })?;
    *out = Statvfs {
        bsize: 4096,
        frsize: 4096,
        blocks: total / 4096,
        bfree: free / 4096,
        bavail: avail / 4096,
        files: 1 << 20,
        ffree: 1 << 19,
        favail: 1 << 19,
        fsid: 0,
        flag: 0,
        namemax: 255,
    };
    Ok(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn statvfs(path: *const c_char, out: *mut Statvfs) -> c_int {
    with_path(AT_FDCWD, path, |p| statvfs_path(p, unsafe { &mut *out }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstatvfs(fdn: c_int, out: *mut Statvfs) -> c_int {
    let p = match fd::get(fdn).map(|f| match &f.obj {
        Obj::Dir { path, .. } => path.clone(),
        _ => cwd(),
    }) {
        Ok(p) => p,
        Err(e) => return errno::fail(e),
    };
    statvfs_path(&p, unsafe { &mut *out }).map_or_else(errno::fail, |r| r)
}

// --- directory streams ---

#[repr(C)]
pub struct Dirent {
    d_ino: u64,
    d_off: i64,
    d_reclen: u16,
    d_type: u8,
    d_name: [u8; 256],
}

pub struct DirStream {
    fd: c_int,
    entries: Vec<(String, u8)>,
    pos: usize,
    loaded: bool,
    ent: Dirent,
}

fn list_dir(path: &str) -> Result<Vec<(String, u8)>, c_int> {
    let mut out = vec![(".".to_string(), 4u8), ("..".to_string(), 4u8)];
    let pat = wide(&format!("{}\\*", path.trim_end_matches('\\')));
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let h = unsafe { FindFirstFileExW(pat.as_ptr(), FindExInfoBasic, &mut data as *mut _ as *mut c_void, FindExSearchNameMatch, std::ptr::null(), 0) };
    if h == INVALID_HANDLE_VALUE {
        let e = unsafe { GetLastError() };
        return if e == 2 || e == 18 { Ok(out) } else { Err(errno::from_win32(e)) };
    }
    loop {
        let name = from_wide(&data.cFileName);
        if name != "." && name != ".." {
            let a = data.dwFileAttributes;
            let t = if a & ATTR_REPARSE != 0 && is_link_tag(data.dwReserved0) {
                10 // DT_LNK
            } else if a & ATTR_DIRECTORY != 0 {
                4 // DT_DIR
            } else if a & ATTR_SYSTEM != 0 {
                0 // maybe a symlink marker; let the caller stat it
            } else {
                8 // DT_REG
            };
            out.push((name, t));
        }
        if unsafe { FindNextFileW(h, &mut data) } == 0 {
            break;
        }
    }
    unsafe { FindClose(h) };
    Ok(out)
}

fn name_ino(name: &str, dir: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    name.hash(&mut h);
    h.finish() | 1
}

#[unsafe(no_mangle)]
pub extern "C" fn fdopendir(fdn: c_int) -> *mut DirStream {
    match fd::get(fdn) {
        Ok(f) if matches!(f.obj, Obj::Dir { .. }) => Box::into_raw(Box::new(DirStream {
            fd: fdn,
            entries: Vec::new(),
            pos: 0,
            loaded: false,
            ent: unsafe { std::mem::zeroed() },
        })),
        Ok(_) => {
            errno::set(errno::ENOTDIR);
            std::ptr::null_mut()
        }
        Err(e) => {
            errno::set(e);
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn opendir(path: *const c_char) -> *mut DirStream {
    let fdn = unsafe { openat(AT_FDCWD, path, (O_RDONLY | O_DIRECTORY | O_CLOEXEC) as c_int, 0) };
    if fdn == -1 {
        return std::ptr::null_mut();
    }
    let d = fdopendir(fdn);
    if d.is_null() {
        fd::close(fdn);
    }
    d
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn readdir(d: *mut DirStream) -> *mut Dirent {
    let d = unsafe { &mut *d };
    let dir = match fd::get(d.fd).map(|f| match &f.obj {
        Obj::Dir { path, .. } => Some(path.clone()),
        _ => None,
    }) {
        Ok(Some(p)) => p,
        _ => {
            errno::set(errno::EBADF);
            return std::ptr::null_mut();
        }
    };
    if !d.loaded {
        match list_dir(&dir) {
            Ok(e) => d.entries = e,
            Err(e) => {
                errno::set(e);
                return std::ptr::null_mut();
            }
        }
        d.loaded = true;
    }
    let Some((name, t)) = d.entries.get(d.pos) else { return std::ptr::null_mut() };
    d.pos += 1;
    let bytes = name.as_bytes();
    let n = bytes.len().min(255);
    d.ent.d_name[..n].copy_from_slice(&bytes[..n]);
    d.ent.d_name[n] = 0;
    d.ent.d_type = *t;
    d.ent.d_ino = name_ino(name, &dir);
    d.ent.d_off = d.pos as i64;
    d.ent.d_reclen = std::mem::size_of::<Dirent>() as u16;
    &mut d.ent
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn closedir(d: *mut DirStream) -> c_int {
    let d = unsafe { Box::from_raw(d) };
    fd::close(d.fd)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rewinddir(d: *mut DirStream) {
    let d = unsafe { &mut *d };
    d.pos = 0;
    d.loaded = false;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn telldir(d: *mut DirStream) -> c_int {
    unsafe { (*d).pos as c_int }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn seekdir(d: *mut DirStream, pos: c_int) {
    unsafe { (*d).pos = pos.max(0) as usize };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn dirfd(d: *mut DirStream) -> c_int {
    unsafe { (*d).fd }
}

/// The directory a descriptor names, for fstatvfs etc.
pub fn dir_path(fdn: c_int) -> Option<String> {
    match &fd::get(fdn).ok()?.obj {
        Obj::Dir { path, .. } => Some(path.clone()),
        _ => None,
    }
}

#[allow(dead_code)]
fn _unused(_: u32) -> u32 {
    O_RDWR
}
