// In-process guest processes: fork() without a host fork(). See procs.h.
#include "blink/procs.h"

#include <errno.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "blink/assert.h"
#include "blink/atomic.h"
#include "blink/endian.h"
#include "blink/errno.h"
#include "blink/fds.h"
#include "blink/jit.h"
#include "blink/linux.h"
#include "blink/log.h"
#include "blink/machine.h"
#include "blink/signal.h"
#include "blink/syscall.h"
#include "blink/thread.h"
#include "blink/vfs.h"
#include "blink/xlat.h"

const struct ProcessHost *g_prochost;
int g_rootpid;

static _Atomic(u64) g_hostsigs;

#ifndef DISABLE_VFS

// Host signals can't be delivered to a guest process directly, because
// any thread of any guest process might receive them. They're recorded
// here, and the embedder forwards them with TakeHostSignals().
static void OnHostSignal(int sig) {
  int lsig;
  if ((lsig = UnXlatSignal(sig)) > 0 && lsig <= 64) {
    atomic_fetch_or(&g_hostsigs, (u64)1 << (lsig - 1));
  }
}

/**
 * Returns a pending host signal (as a linux signal number), or zero.
 */
int TakeHostSignals(void) {
  int i;
  u64 have, bit;
  have = atomic_load(&g_hostsigs);
  for (i = 0; i < 64; ++i) {
    bit = (u64)1 << i;
    if ((have & bit) && (atomic_fetch_and(&g_hostsigs, ~bit) & bit)) {
      return i + 1;
    }
  }
  return 0;
}

/**
 * Makes guest processes run as threads of this host process.
 */
void SetProcessHost(const struct ProcessHost *host) {
  static const int kForwarded[] = {SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
  struct sigaction sa;
  size_t i;
  g_prochost = host;
  memset(&sa, 0, sizeof(sa));
  sigfillset(&sa.sa_mask);
  sa.sa_handler = OnHostSignal;
  for (i = 0; i < sizeof(kForwarded) / sizeof(*kForwarded); ++i) {
    unassert(!sigaction(kForwarded[i], &sa, 0));
  }
}

/**
 * Delivers a signal to some thread of a guest process.
 */
int SignalSystem(struct System *s, int sig) {
  struct Dll *e;
  struct Machine *m, *pick;
  if (!(1 <= sig && sig <= 64)) return -EINVAL;
  switch (sig) {
    case SIGSTOP_LINUX:
    case SIGTSTP_LINUX:
    case SIGTTIN_LINUX:
    case SIGTTOU_LINUX:
    case SIGCONT_LINUX:
      // job control isn't supported; only deliver these to handlers
      if (sig == SIGSTOP_LINUX ||
          Read64(s->hands[sig - 1].handler) == SIG_DFL_LINUX) {
        return 0;
      }
      break;
    default:
      break;
  }
  pick = 0;
  LOCK(&s->machines_lock);
  for (e = dll_first(s->machines); e; e = dll_next(s->machines, e)) {
    m = MACHINE_CONTAINER(e);
    if (!pick) pick = m;
    if (~m->sigmask & ((u64)1 << (sig - 1))) {
      pick = m;
      break;
    }
  }
  if (pick) {
    EnqueueSignal(pick, sig);
    pthread_kill(pick->thread, SIGSYS);
  }
  UNLOCK(&s->machines_lock);
  return 0;
}

/**
 * Returns true if a blocking call on behalf of `m` should return EINTR.
 */
bool IsMachineInterrupted(struct Machine *m) {
  return atomic_load_explicit(&m->killed, memory_order_acquire) ||
         (m->signals & ~m->sigmask);
}

// Sets up the main thread of a forked child as a copy of the forking one.
static void CopyMachineForFork(struct Machine *m2, struct Machine *m) {
  struct Dll elem;
  struct System *s;
  LOCK(&m2->system->machines_lock);
  s = m2->system;
  elem = m2->elem;
  memcpy(m2, m, sizeof(*m2));
  m2->elem = elem;
  UNLOCK(&s->machines_lock);
  m2->system = s;
  m2->tid = s->pid;
  m2->ctid = 0;
  m2->oplen = 0;
  m2->signals = 0;
  m2->sysdepth = 0;
  m2->sigdepth = 0;
  m2->nofault = false;
  m2->insyscall = false;
  m2->threaded = false;
  m2->interrupted = false;
  m2->killed = false;
  memset(&m2->path, 0, sizeof(m2->path));
  memset(&m2->freelist, 0, sizeof(m2->freelist));
  memset(&m2->pagelocks, 0, sizeof(m2->pagelocks));
  m2->xedd = (struct XedDecodedInst *)m2->opcache->icache[0];
  ResetInstructionCache(m2);
  ResetTlb(m2);
}

static void CopyFds(struct Fds *to, struct Fds *from) {
  struct Fd *fd;
  struct Dll *e;
  LOCK(&from->lock);
  for (e = dll_first(from->list); e; e = dll_next(from->list, e)) {
    fd = FD_CONTAINER(e);
    ForkFd(to, fd, fd->fildes, fd->oflags);
  }
  UNLOCK(&from->lock);
}

static char *StrdupOrNull(const char *s) {
  return s ? strdup(s) : 0;
}

/**
 * Implements fork() and vfork() by running the child on a new thread.
 *
 * A fork() child gets a copy of its parent's memory. A vfork() child
 * runs on its parent's page tables, and the parent waits until the
 * child has called execve() or exited, just like on Linux.
 */
int ForkProcess(struct Machine *m, u64 flags, u64 stack, u64 ctid) {
  int pid, err;
  bool isvfork;
  pthread_t thread;
  sigset_t ss, oldss;
  pthread_attr_t attr;
  struct Machine *m2;
  struct System *p, *s;
  _Atomic(int) *ctid_ptr;
  p = m->system;
  isvfork = !!(flags & CLONE_VM_LINUX);
  if (HasLinearMapping()) {
    LOGF("in-process fork() requires non-linear memory (blink -m)");
    return enosys();
  }
  if (!(s = NewSystem(p->mode))) return -1;
  // NewSystem() shares the caller's files like execve() wants, but the
  // child of a fork() gets its own copy of the file descriptor table.
  VfsFreeProc((struct VfsProc *)s->vfsproc);
  unassert((s->vfsproc = VfsForkProc((struct VfsProc *)p->vfsproc)));
  CopyFds(&s->fds, &p->fds);
  LOCK(&p->sig_lock);
  memcpy(s->hands, p->hands, sizeof(s->hands));
  UNLOCK(&p->sig_lock);
  s->blinksigs = p->blinksigs;
  memcpy(s->rlim, p->rlim, sizeof(s->rlim));
  s->dlab = p->dlab;
  s->loaded = p->loaded;
  s->iscosmo = p->iscosmo;
  s->gdt_limit = p->gdt_limit;
  s->idt_limit = p->idt_limit;
  s->efer = p->efer;
  s->gdt_base = p->gdt_base;
  s->idt_base = p->idt_base;
  s->cr0 = p->cr0;
  s->cr2 = p->cr2;
  s->cr4 = p->cr4;
  s->brk = p->brk;
  s->automap = p->automap;
  s->codestart = p->codestart;
  s->codesize = p->codesize;
  s->elf = p->elf;
  s->elf.prog = StrdupOrNull(p->elf.prog);
  s->elf.execfn = StrdupOrNull(p->elf.execfn);
  s->elf.interpreter = StrdupOrNull(p->elf.interpreter);
  s->onfilemap = p->onfilemap;
  s->onsymbols = p->onsymbols;
  s->onbinbase = p->onbinbase;
  s->onlongbranch = p->onlongbranch;
  s->onromwriteattempt = p->onromwriteattempt;
  s->exec = p->exec;
  s->redraw = p->redraw;
  if (isvfork) {
    s->cr3 = p->cr3;
    s->borrowedmm = true;
    s->mmowner = p;
#ifdef HAVE_JIT
    DisableJit(&s->jit);  // it only runs until execve()
#endif
  } else {
    LOCK(&p->mmap_lock);
    err = ForkVirtual(s, p);
    UNLOCK(&p->mmap_lock);
    if (err == -1) {
      LOGF("ran out of memory copying address space for fork()");
      FreeSystem(s);
      return enomem();
    }
  }
  if ((pid = g_prochost->Register(p->pid, s, isvfork)) < 0) {
    FreeSystem(s);
    errno = -pid;
    return -1;
  }
  s->pid = pid;
  if (!(m2 = NewMachine(s, 0))) {
    g_prochost->Detach(pid);
    FreeSystem(s);
    g_prochost->Exit(pid, SIGKILL_LINUX);
    return eagain();
  }
  CopyMachineForFork(m2, m);
  if ((flags & (CLONE_CHILD_SETTID_LINUX | CLONE_CHILD_CLEARTID_LINUX)) &&
      !(ctid & (sizeof(i32) - 1)) &&
      (ctid_ptr = (_Atomic(i32) *)LookupAddress(m2, ctid))) {
    if (flags & CLONE_CHILD_SETTID_LINUX) {
      atomic_store_explicit(ctid_ptr, Little32(pid), memory_order_release);
    }
    if (flags & CLONE_CHILD_CLEARTID_LINUX) {
      m2->ctid = ctid;
    }
  }
  Put64(m2->ax, 0);
  if (stack) Put64(m2->sp, stack);
  THR_LOGF("pid=%d tid=%d ForkProcess -> pid=%d%s", p->pid, m->tid, pid,
           isvfork ? " (vfork)" : "");
  sigfillset(&ss);
  unassert(!pthread_sigmask(SIG_SETMASK, &ss, &oldss));
  m2->spawn_sigmask = oldss;
  unassert(!pthread_attr_init(&attr));
  unassert(!pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED));
  unassert(!pthread_attr_setstacksize(&attr, 8 * 1024 * 1024));
  err = pthread_create(&thread, &attr, OnSpawn, m2);
  unassert(!pthread_attr_destroy(&attr));
  unassert(!pthread_sigmask(SIG_SETMASK, &oldss, 0));
  if (err) {
    LOGF("failed to create thread for child process: %s", strerror(err));
    // let the child die as though it was killed; it never ran
    g_prochost->Detach(pid);
    FreeMachine(m2);
    g_prochost->Exit(pid, SIGKILL_LINUX);
    return pid;
  }
  if (isvfork) {
    g_prochost->VforkWait(pid);
  }
  return pid;
}

/**
 * Ends the guest process that `m` belongs to.
 *
 * @param wstatus is a linux wait status, e.g. `code << 8` or `signal`
 */
_Noreturn void ExitProcess(struct Machine *m, int wstatus) {
  int pid;
  struct System *s;
  s = m->system;
  pid = s->pid;
  THR_LOGF("pid=%d tid=%d ExitProcess(%#x)", pid, m->tid, wstatus);
  if (pid == g_rootpid) {
    // the whole host process is going away; don't wait for threads
    g_prochost->Exit(pid, wstatus);
    _Exit((wstatus & 127) ? 128 + (wstatus & 127) : (wstatus >> 8) & 255);
  }
  KillOtherThreads(s);
#ifdef HAVE_JIT
  DisableJit(&s->jit);  // unmapping exec pages is slow
#endif
  g_prochost->Detach(pid);
  FreeMachine(m);  // frees the System too, which closes its files
  g_prochost->Exit(pid, wstatus);
  pthread_exit(0);
}

#else /* DISABLE_VFS */

// guest processes need their own file descriptor tables, which only
// exist when the VFS is enabled

void SetProcessHost(const struct ProcessHost *host) {
  WriteErrorString("in-process guest processes need the VFS\n");
  exit(1);
}

int ForkProcess(struct Machine *m, u64 flags, u64 stack, u64 ctid) {
  return enosys();
}

_Noreturn void ExitProcess(struct Machine *m, int wstatus) {
  _Exit(1);
}

int SignalSystem(struct System *s, int sig) {
  return -ENOSYS;
}

bool IsMachineInterrupted(struct Machine *m) {
  return false;
}

int TakeHostSignals(void) {
  return 0;
}

#endif /* DISABLE_VFS */
