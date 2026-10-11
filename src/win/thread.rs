//! pthreads on Windows: threads from CreateThread, mutexes and condition
//! variables from SRW locks, once from INIT_ONCE.
//!
//! blink interrupts a thread blocked in a host call with pthread_kill(t,
//! SIGSYS), expecting the call to fail with EINTR. Every blocking call in
//! this layer waits through `Interruptible`, which pthread_kill wakes.

use super::errno;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    AcquireSRWLockExclusive, CreateThread, ExitThread, GetCurrentThreadId, INFINITE,
    InitOnceExecuteOnce, ReleaseSRWLockExclusive, SleepConditionVariableSRW, SwitchToThread,
    TryAcquireSRWLockExclusive, WaitForSingleObject, WakeAllConditionVariable, WakeConditionVariable,
    CONDITION_VARIABLE, INIT_ONCE, SRWLOCK,
};

/// What blink knows as a pthread_t: an id in `registry()`.
pub struct Thread {
    pub id: u64,
    handle: Mutex<HANDLE>,
    /// set by pthread_kill; consumed by the interrupted blocking call
    interrupted: AtomicBool,
    pub sigmask: AtomicU64,
}

// SAFETY: the HANDLE is only used for waiting/closing, behind a mutex.
unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

fn registry() -> &'static Mutex<HashMap<u64, Arc<Thread>>> {
    static R: OnceLock<Mutex<HashMap<u64, Arc<Thread>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CURRENT: Cell<Option<&'static Thread>> = const { Cell::new(None) };
}

fn register(handle: HANDLE) -> Arc<Thread> {
    let t = Arc::new(Thread {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        handle: Mutex::new(handle),
        interrupted: AtomicBool::new(false),
        sigmask: AtomicU64::new(0),
    });
    registry().lock().unwrap().insert(t.id, t.clone());
    t
}

/// The calling thread (registering threads this layer didn't create).
pub fn current() -> &'static Thread {
    CURRENT.with(|c| match c.get() {
        Some(t) => t,
        None => {
            let t = register(std::ptr::null_mut());
            // the registry keeps it alive until the thread exits
            let r: &'static Thread = unsafe { &*Arc::as_ptr(&t) };
            c.set(Some(r));
            r
        }
    })
}

fn lookup(id: u64) -> Option<Arc<Thread>> {
    registry().lock().unwrap().get(&id).cloned()
}

// Blocking calls wait on this; pthread_kill and in-memory I/O objects
// (pipes, ttys) notify it. One condvar keeps it simple; waiters recheck.
static WAKE: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();

fn wake() -> &'static (Mutex<u64>, Condvar) {
    WAKE.get_or_init(|| (Mutex::new(0), Condvar::new()))
}

/// Wakes every blocked call so it rechecks its condition.
pub fn notify_all() {
    let (m, c) = wake();
    *m.lock().unwrap() += 1;
    c.notify_all();
}

/// True (and clears it) if pthread_kill interrupted the calling thread.
pub fn take_interrupt() -> bool {
    current().interrupted.swap(false, Ordering::AcqRel)
}

/// Waits until `ready()` returns Some, the timeout passes (None means
/// forever), or the thread is interrupted (Err(EINTR)). `ready` runs
/// with no locks held; whoever changes its answer calls notify_all().
pub fn wait_for<T>(timeout: Option<Duration>, mut ready: impl FnMut() -> Option<T>) -> Result<Option<T>, c_int> {
    let deadline = timeout.map(|t| Instant::now() + t);
    let (m, c) = wake();
    loop {
        let seen = *m.lock().unwrap();
        if take_interrupt() {
            return Err(errno::EINTR);
        }
        if let Some(v) = ready() {
            return Ok(Some(v));
        }
        let left = match deadline {
            Some(d) => match d.checked_duration_since(Instant::now()) {
                Some(l) if !l.is_zero() => l,
                _ => return Ok(None),
            },
            None => Duration::from_secs(3600),
        };
        let g = m.lock().unwrap();
        if *g == seen {
            // also poll now and then: sockets and files don't notify
            let _ = c.wait_timeout(g, left.min(Duration::from_millis(10))).unwrap();
        }
    }
}

/// Sleeps, returning Err(EINTR) early if interrupted.
pub fn sleep(d: Duration) -> Result<(), c_int> {
    wait_for::<()>(Some(d), || None).map(|_| ())
}

#[repr(C)]
pub struct PthreadAttr {
    stacksize: u64,
    detached: c_int,
}

struct Start {
    func: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
    thread: Arc<Thread>,
}

unsafe extern "system" fn trampoline(p: *mut c_void) -> u32 {
    // SAFETY: pthread_create passed a leaked Box<Start>.
    let start = unsafe { Box::from_raw(p as *mut Start) };
    let t: &'static Thread = unsafe { &*Arc::as_ptr(&start.thread) };
    CURRENT.with(|c| c.set(Some(t)));
    (start.func)(start.arg);
    finish();
    0
}

fn finish() {
    let t = current();
    if let Some(t) = registry().lock().unwrap().remove(&t.id) {
        let h = *t.handle.lock().unwrap();
        if !h.is_null() {
            // SAFETY: our own thread handle; nobody joins detached threads
            unsafe { CloseHandle(h) };
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_create(
    out: *mut u64,
    attr: *const PthreadAttr,
    func: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
) -> c_int {
    let stack = if attr.is_null() { 0 } else { unsafe { (*attr).stacksize as usize } };
    let thread = register(std::ptr::null_mut());
    let id = thread.id;
    let start = Box::into_raw(Box::new(Start { func, arg, thread: thread.clone() }));
    let mut tid = 0u32;
    // STACK_SIZE_PARAM_IS_A_RESERVATION
    let h = unsafe {
        CreateThread(std::ptr::null(), stack.max(1 << 20), Some(trampoline), start as *const c_void, 0x10000, &mut tid)
    };
    if h.is_null() {
        drop(unsafe { Box::from_raw(start) });
        registry().lock().unwrap().remove(&id);
        return errno::EAGAIN;
    }
    *thread.handle.lock().unwrap() = h;
    unsafe { *out = id };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_exit(_ret: *mut c_void) -> ! {
    finish();
    // SAFETY: ends the calling thread
    unsafe { ExitThread(0) }
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_self() -> u64 {
    current().id
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_equal(a: u64, b: u64) -> c_int {
    (a == b) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_join(id: u64, ret: *mut *mut c_void) -> c_int {
    let Some(t) = lookup(id) else { return errno::ESRCH };
    let h = *t.handle.lock().unwrap();
    if !h.is_null() {
        // SAFETY: valid until the thread's finish(), which runs before exit
        unsafe { WaitForSingleObject(h, INFINITE) };
    }
    if !ret.is_null() {
        unsafe { *ret = std::ptr::null_mut() };
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_detach(_id: u64) -> c_int {
    0
}

/// blink's only use: interrupt a blocked thread (SIGSYS) or probe it (0).
#[unsafe(no_mangle)]
pub extern "C" fn pthread_kill(id: u64, sig: c_int) -> c_int {
    let Some(t) = lookup(id) else { return errno::ESRCH };
    if sig != 0 {
        t.interrupted.store(true, Ordering::Release);
        notify_all();
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_init(a: *mut PthreadAttr) -> c_int {
    unsafe { *a = PthreadAttr { stacksize: 0, detached: 0 } };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_destroy(_a: *mut PthreadAttr) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_setdetachstate(a: *mut PthreadAttr, d: c_int) -> c_int {
    unsafe { (*a).detached = d };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_attr_setstacksize(a: *mut PthreadAttr, s: usize) -> c_int {
    unsafe { (*a).stacksize = s as u64 };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_atfork(_a: *const c_void, _b: *const c_void, _c: *const c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_setcancelstate(_s: c_int, old: *mut c_int) -> c_int {
    if !old.is_null() {
        unsafe { *old = 0 };
    }
    0
}

// mutex: w[0] SRWLOCK, w[1] owner thread id, w[2] recursion depth, w[3] type
#[repr(C)]
pub struct Mutex5 {
    w: [u64; 5],
}

#[repr(C)]
pub struct MutexAttr {
    kind: u32,
    pshared: u32,
}

const RECURSIVE: u64 = 1;

fn srw(m: *mut Mutex5) -> *mut SRWLOCK {
    m as *mut SRWLOCK
}

fn me() -> u64 {
    // SAFETY: plain Win32 call
    unsafe { GetCurrentThreadId() as u64 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_init(a: *mut MutexAttr) -> c_int {
    unsafe { *a = MutexAttr { kind: 0, pshared: 0 } };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutexattr_destroy(_a: *mut MutexAttr) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_settype(a: *mut MutexAttr, kind: c_int) -> c_int {
    unsafe { (*a).kind = kind as u32 };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutexattr_setpshared(a: *mut MutexAttr, p: c_int) -> c_int {
    unsafe { (*a).pshared = p as u32 };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_init(m: *mut Mutex5, a: *const MutexAttr) -> c_int {
    unsafe {
        (*m).w = [0; 5];
        if !a.is_null() {
            (*m).w[3] = (*a).kind as u64;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_mutex_destroy(_m: *mut Mutex5) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_lock(m: *mut Mutex5) -> c_int {
    unsafe {
        if (*m).w[3] == RECURSIVE && (*m).w[1] == me() {
            (*m).w[2] += 1;
            return 0;
        }
        AcquireSRWLockExclusive(srw(m));
        (*m).w[1] = me();
        (*m).w[2] = 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_trylock(m: *mut Mutex5) -> c_int {
    unsafe {
        if (*m).w[3] == RECURSIVE && (*m).w[1] == me() {
            (*m).w[2] += 1;
            return 0;
        }
        if !TryAcquireSRWLockExclusive(srw(m)) {
            return errno::EBUSY;
        }
        (*m).w[1] = me();
        (*m).w[2] = 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_mutex_unlock(m: *mut Mutex5) -> c_int {
    unsafe {
        if (*m).w[3] == RECURSIVE && (*m).w[2] > 1 {
            (*m).w[2] -= 1;
            return 0;
        }
        (*m).w[1] = 0;
        (*m).w[2] = 0;
        ReleaseSRWLockExclusive(srw(m));
    }
    0
}

// condition variable: w[0] CONDITION_VARIABLE
#[repr(C)]
pub struct Cond2 {
    w: [u64; 2],
}

#[repr(C)]
pub struct CondAttr {
    pshared: u32,
    clock: u32,
}

fn cv(c: *mut Cond2) -> *mut CONDITION_VARIABLE {
    c as *mut CONDITION_VARIABLE
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_condattr_init(a: *mut CondAttr) -> c_int {
    unsafe { *a = CondAttr { pshared: 0, clock: 0 } };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_condattr_destroy(_a: *mut CondAttr) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_condattr_setpshared(_a: *mut CondAttr, _p: c_int) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_init(c: *mut Cond2, _a: *const CondAttr) -> c_int {
    unsafe { (*c).w = [0; 2] };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn pthread_cond_destroy(_c: *mut Cond2) -> c_int {
    0
}

unsafe fn cond_wait_ms(c: *mut Cond2, m: *mut Mutex5, ms: u32) -> c_int {
    unsafe {
        let depth = (*m).w[2];
        (*m).w[1] = 0;
        let ok = SleepConditionVariableSRW(cv(c), srw(m), ms, 0);
        (*m).w[1] = me();
        (*m).w[2] = depth;
        if ok == 0 { errno::ETIMEDOUT } else { 0 }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_wait(c: *mut Cond2, m: *mut Mutex5) -> c_int {
    unsafe { cond_wait_ms(c, m, INFINITE) };
    0
}

/// `abstime` is CLOCK_REALTIME, like POSIX's default.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_timedwait(c: *mut Cond2, m: *mut Mutex5, abstime: *const super::time::Timespec) -> c_int {
    let now = super::time::realtime_ns();
    let at = unsafe { (*abstime).tv_sec as i128 * 1_000_000_000 + (*abstime).tv_nsec as i128 };
    let ms = ((at - now as i128).max(0) as u128).div_ceil(1_000_000).min(INFINITE as u128 - 1) as u32;
    unsafe { cond_wait_ms(c, m, ms) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_signal(c: *mut Cond2) -> c_int {
    unsafe { WakeConditionVariable(cv(c)) };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_cond_broadcast(c: *mut Cond2) -> c_int {
    unsafe { WakeAllConditionVariable(cv(c)) };
    0
}

unsafe extern "system" fn once_trampoline(_o: *mut INIT_ONCE, f: *mut c_void, _ctx: *mut *mut c_void) -> i32 {
    // SAFETY: pthread_once passes the init function as the parameter
    let f: extern "C" fn() = unsafe { std::mem::transmute(f) };
    f();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pthread_once(o: *mut u64, f: extern "C" fn()) -> c_int {
    unsafe { InitOnceExecuteOnce(o as *mut INIT_ONCE, Some(once_trampoline), f as *mut c_void, std::ptr::null_mut()) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn sched_yield() -> c_int {
    // SAFETY: plain Win32 call
    unsafe { SwitchToThread() };
    0
}

pub fn ncpu() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sched_getaffinity(_pid: c_int, size: usize, set: *mut u8) -> c_int {
    unsafe {
        std::ptr::write_bytes(set, 0, size);
        for i in 0..ncpu().min(size * 8) {
            *set.add(i / 8) |= 1 << (i % 8);
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn sched_setaffinity(_pid: c_int, _size: usize, _set: *const u8) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __sched_cpucount(size: usize, set: *const u8) -> c_int {
    (0..size).map(|i| unsafe { *set.add(i) }.count_ones() as c_int).sum()
}
