//! Guest process table for running fork()ed guest processes as threads.
//!
//! The Xbox (UWP) can't create host processes, so blink runs every guest
//! process inside this host process: blink's C side gives each one its own
//! `System` (address space, file descriptors, signal handlers) on its own
//! thread, and asks this table for everything that spans processes: pids,
//! parentage, process groups and sessions, exit statuses, wait4() and
//! kill(). See blink/procs.h for the C side of the contract.

use std::collections::BTreeMap;
use std::ffi::{c_int, c_void};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

// Linux constants (guest ABI, not host).
const EPERM: c_int = 1;
const ESRCH: c_int = 3;
const EINTR: c_int = 4;
const ECHILD: c_int = 10;
const WNOHANG: c_int = 1;
const SIGCHLD: c_int = 17;

/// blink's thread ids start here (kMinThreadId), so pids must stay below.
const MAX_PID: i32 = 262_144;
const FIRST_RECYCLED_PID: i32 = 300;

/// How often blocked waits recheck for guest signals.
const SIGNAL_POLL: Duration = Duration::from_millis(10);

#[repr(C)]
struct ProcessHost {
    register: extern "C" fn(c_int, *mut c_void, bool) -> c_int,
    exec: extern "C" fn(c_int, *mut c_void),
    detach: extern "C" fn(c_int),
    exit: extern "C" fn(c_int, c_int),
    wait: extern "C" fn(c_int, c_int, c_int, *mut c_int, *mut c_void) -> c_int,
    vfork_wait: extern "C" fn(c_int),
    kill: extern "C" fn(c_int, c_int, c_int) -> c_int,
    getppid: extern "C" fn(c_int) -> c_int,
    getpgid: extern "C" fn(c_int, c_int) -> c_int,
    setpgid: extern "C" fn(c_int, c_int, c_int) -> c_int,
    getsid: extern "C" fn(c_int, c_int) -> c_int,
    setsid: extern "C" fn(c_int) -> c_int,
}

extern "C" {
    fn SetProcessHost(host: *const ProcessHost);
    fn SignalSystem(system: *mut c_void, sig: c_int) -> c_int;
    fn IsMachineInterrupted(machine: *mut c_void) -> bool;
    fn TakeHostSignals() -> c_int;
}

static HOST: ProcessHost = ProcessHost {
    register,
    exec,
    detach,
    exit,
    wait,
    vfork_wait,
    kill,
    getppid,
    getpgid,
    setpgid,
    getsid,
    setsid,
};

struct Proc {
    ppid: i32,
    pgid: i32,
    sid: i32,
    /// blink `struct System *` while the process runs; 0 once it's exiting.
    system: usize,
    /// Linux wait status once the process has exited (a zombie).
    status: Option<i32>,
    /// A vfork() parent is blocked until this process execs or exits.
    vfork: bool,
}

struct Table {
    procs: BTreeMap<i32, Proc>,
    root: i32,
    next_pid: i32,
}

static TABLE: Mutex<Table> = Mutex::new(Table { procs: BTreeMap::new(), root: 0, next_pid: 0 });
static CHANGED: Condvar = Condvar::new();

fn table() -> MutexGuard<'static, Table> {
    // a panic while holding the lock leaves the table consistent enough
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Makes blink run guest processes in this host process. Call before
/// `run()`; blink must also use non-linear memory (`-m`).
pub fn install() {
    // SAFETY: HOST is a static with the layout blink/procs.h expects.
    unsafe { SetProcessHost(&HOST) };
    // blink records host signals (terminal ^C, window resizes, ...) and
    // we deliver them to the initial process's process group.
    std::thread::Builder::new()
        .name("blink-signals".into())
        .spawn(|| loop {
            // SAFETY: plain C call, async-signal-safe bookkeeping only.
            match unsafe { TakeHostSignals() } {
                0 => std::thread::sleep(Duration::from_millis(20)),
                sig => {
                    let t = table();
                    if let Some(root) = t.procs.get(&t.root) {
                        let pgid = root.pgid;
                        drop(t);
                        kill(0, -pgid, sig);
                    }
                }
            }
        })
        .expect("failed to start signal forwarding thread");
}

impl Table {
    fn allocate_pid(&mut self) -> i32 {
        loop {
            let pid = self.next_pid;
            self.next_pid = if pid + 1 >= MAX_PID { FIRST_RECYCLED_PID } else { pid + 1 };
            if !self.procs.contains_key(&pid) && !self.procs.values().any(|p| p.pgid == pid || p.sid == pid) {
                return pid;
            }
        }
    }
}

extern "C" fn register(ppid: c_int, system: *mut c_void, isvfork: bool) -> c_int {
    let mut t = table();
    let (pid, pgid, sid) = if ppid == 0 {
        let host = std::process::id() as i32;
        let pid = if (2..MAX_PID).contains(&host) { host } else { 1000 };
        t.root = pid;
        t.next_pid = if pid + 1 < MAX_PID { pid + 1 } else { FIRST_RECYCLED_PID };
        (pid, pid, pid)
    } else {
        let (pgid, sid) = match t.procs.get(&ppid) {
            Some(p) => (p.pgid, p.sid),
            None => (ppid, ppid),
        };
        (t.allocate_pid(), pgid, sid)
    };
    t.procs.insert(pid, Proc { ppid, pgid, sid, system: system as usize, status: None, vfork: isvfork });
    pid
}

extern "C" fn exec(pid: c_int, system: *mut c_void) {
    let mut t = table();
    if let Some(p) = t.procs.get_mut(&pid) {
        p.system = system as usize;
        p.vfork = false;
    }
    CHANGED.notify_all();
}

extern "C" fn detach(pid: c_int) {
    if let Some(p) = table().procs.get_mut(&pid) {
        p.system = 0;
    }
}

extern "C" fn exit(pid: c_int, wstatus: c_int) {
    let mut t = table();
    // orphans get reaped as they exit, since nobody can wait for them
    t.procs.retain(|_, c| !(c.ppid == pid && c.status.is_some()));
    for c in t.procs.values_mut().filter(|c| c.ppid == pid) {
        c.ppid = 0;
    }
    let ppid = match t.procs.get_mut(&pid) {
        Some(p) => {
            p.system = 0;
            p.vfork = false;
            p.status = Some(wstatus);
            p.ppid
        }
        None => 0,
    };
    match t.procs.get(&ppid) {
        Some(parent) if parent.status.is_none() => {
            if parent.system != 0 {
                // SAFETY: the System stays alive until its process detaches,
                // which needs the table lock we're holding.
                unsafe { SignalSystem(parent.system as *mut c_void, SIGCHLD) };
            }
        }
        _ => {
            if pid != t.root {
                t.procs.remove(&pid);
            }
        }
    }
    CHANGED.notify_all();
}

extern "C" fn wait(me: c_int, pid: c_int, options: c_int, wstatus: *mut c_int, machine: *mut c_void) -> c_int {
    let mut t = table();
    loop {
        let my_pgid = t.procs.get(&me).map_or(me, |p| p.pgid);
        let wanted = |cpid: i32, c: &Proc| {
            c.ppid == me
                && match pid {
                    -1 => true,
                    0 => c.pgid == my_pgid,
                    p if p > 0 => cpid == p,
                    p => c.pgid == -p,
                }
        };
        let mut any = false;
        let mut zombie = None;
        for (&cpid, c) in t.procs.iter().filter(|(&cpid, c)| wanted(cpid, c)) {
            any = true;
            if let Some(status) = c.status {
                zombie = Some((cpid, status));
                break;
            }
        }
        if let Some((cpid, status)) = zombie {
            t.procs.remove(&cpid);
            // SAFETY: blink passes a pointer to a local int.
            unsafe { *wstatus = status };
            return cpid;
        }
        if !any {
            return -ECHILD;
        }
        if options & WNOHANG != 0 {
            return 0;
        }
        // SAFETY: machine is the calling thread's own live Machine.
        if unsafe { IsMachineInterrupted(machine) } {
            return -EINTR;
        }
        t = CHANGED.wait_timeout(t, SIGNAL_POLL).unwrap_or_else(|e| e.into_inner()).0;
    }
}

extern "C" fn vfork_wait(child: c_int) {
    let mut t = table();
    while t.procs.get(&child).is_some_and(|c| c.vfork) {
        t = CHANGED.wait_timeout(t, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

extern "C" fn kill(me: c_int, pid: c_int, sig: c_int) -> c_int {
    let t = table();
    let my_pgid = t.procs.get(&me).map_or(me, |p| p.pgid);
    let targets: Vec<&Proc> = t
        .procs
        .iter()
        .filter(|(&cpid, c)| match pid {
            -1 => cpid != me && cpid != t.root,
            0 => c.pgid == my_pgid,
            p if p > 0 => cpid == p,
            p => c.pgid == -p,
        })
        .map(|(_, c)| c)
        .collect();
    if targets.is_empty() {
        return -ESRCH;
    }
    if sig != 0 {
        for c in targets.into_iter().filter(|c| c.system != 0) {
            // SAFETY: see exit(); the table lock keeps the System alive.
            unsafe { SignalSystem(c.system as *mut c_void, sig) };
        }
    }
    0
}

fn host_parent_pid() -> c_int {
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id() as c_int
    }
    #[cfg(not(unix))]
    {
        1
    }
}

extern "C" fn getppid(me: c_int) -> c_int {
    match table().procs.get(&me) {
        Some(p) if p.ppid != 0 => p.ppid,
        _ => host_parent_pid(),
    }
}

extern "C" fn getpgid(me: c_int, pid: c_int) -> c_int {
    let pid = if pid == 0 { me } else { pid };
    table().procs.get(&pid).map_or(-ESRCH, |p| p.pgid)
}

extern "C" fn getsid(me: c_int, pid: c_int) -> c_int {
    let pid = if pid == 0 { me } else { pid };
    table().procs.get(&pid).map_or(-ESRCH, |p| p.sid)
}

extern "C" fn setpgid(me: c_int, pid: c_int, pgid: c_int) -> c_int {
    let mut t = table();
    let pid = if pid == 0 { me } else { pid };
    let pgid = if pgid == 0 { pid } else { pgid };
    if pgid < 0 {
        return -22; // EINVAL
    }
    let (sid, ppid) = match t.procs.get(&pid) {
        Some(p) => (p.sid, p.ppid),
        None => return -ESRCH,
    };
    if pid != me && ppid != me {
        return -ESRCH;
    }
    if sid == pid {
        return -EPERM; // session leaders can't move
    }
    if pgid != pid && !t.procs.values().any(|p| p.pgid == pgid && p.sid == sid) {
        return -EPERM;
    }
    t.procs.get_mut(&pid).unwrap().pgid = pgid;
    0
}

extern "C" fn setsid(me: c_int) -> c_int {
    let mut t = table();
    if t.procs.values().any(|p| p.pgid == me) {
        return -EPERM; // already a process group leader
    }
    match t.procs.get_mut(&me) {
        Some(p) => {
            p.pgid = me;
            p.sid = me;
            me
        }
        None => -ESRCH,
    }
}
