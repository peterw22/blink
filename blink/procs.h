#ifndef BLINK_PROCS_H_
#define BLINK_PROCS_H_
#include <stdbool.h>

#include "blink/machine.h"
#include "blink/types.h"

// Lets an embedder run fork()ed guest processes as threads inside this
// host process, rather than calling the host's fork() (which platforms
// like UWP don't have). Each guest process is a separate System with
// its own address space, so this requires non-linear memory (-m).
//
// The embedder owns the process table (pids, parentage, process groups,
// sessions, exit statuses); blink owns each process's System. All the
// hooks return linux errno values negated on failure.
struct ProcessHost {
  // Adds a process running `system`, returning its new pid. The initial
  // process has ppid 0. A vfork child's parent later calls VforkWait().
  int (*Register)(int ppid, struct System *system, bool isvfork);
  // pid's System was replaced by execve(); releases a vfork() parent.
  void (*Exec)(int pid, struct System *system);
  // pid's System is about to be freed; stop routing signals to it.
  void (*Detach)(int pid);
  // pid has exited with linux wait status; makes it a zombie, notifies
  // its parent with SIGCHLD and releases a vfork() parent. When pid is
  // the initial process, the embedder decides what happens next; if the
  // hook returns, the host process exits.
  void (*Exit)(int pid, int wstatus);
  // wait4() minus rusage. Should return -EINTR promptly once
  // IsMachineInterrupted(m) says so.
  int (*Wait)(int self, int pid, int options, int *wstatus, struct Machine *m);
  // Blocks until a vfork() child has exec'd or exited.
  void (*VforkWait)(int child);
  // kill(); delivers signals using SignalSystem().
  int (*Kill)(int self, int pid, int sig);
  int (*Getppid)(int self);
  int (*Getpgid)(int self, int pid);
  int (*Setpgid)(int self, int pid, int pgid);
  int (*Getsid)(int self, int pid);
  int (*Setsid)(int self);
};

extern const struct ProcessHost *g_prochost;
extern int g_rootpid;  // pid of the initial process
extern bool g_embedded;

void SetProcessHost(const struct ProcessHost *);
void SetEmbedded(bool);
int SignalSystem(struct System *, int);
bool IsMachineInterrupted(struct Machine *);
int TakeHostSignals(void);
int ForkProcess(struct Machine *, u64, u64, u64);
_Noreturn void ExitGuestProcess(struct Machine *, int);

#endif /* BLINK_PROCS_H_ */
