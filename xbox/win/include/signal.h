#ifndef BLINKWIN_SIGNAL_H_
#define BLINKWIN_SIGNAL_H_
#include <sys/types.h>
// ucrt's signal.h has signal()/raise() for a handful of signals; blink
// needs POSIX signals. Numbers follow Linux. Host signals are emulated by
// the Rust layer (mainly to interrupt blocking calls, see pthread_kill).
#define SIGHUP 1
#define SIGINT 2
#define SIGQUIT 3
#define SIGILL 4
#define SIGTRAP 5
#define SIGABRT 6
#define SIGBUS 7
#define SIGFPE 8
#define SIGKILL 9
#define SIGUSR1 10
#define SIGSEGV 11
#define SIGUSR2 12
#define SIGPIPE 13
#define SIGALRM 14
#define SIGTERM 15
#define SIGSTKFLT 16
#define SIGCHLD 17
#define SIGCONT 18
#define SIGSTOP 19
#define SIGTSTP 20
#define SIGTTIN 21
#define SIGTTOU 22
#define SIGURG 23
#define SIGXCPU 24
#define SIGXFSZ 25
#define SIGVTALRM 26
#define SIGPROF 27
#define SIGWINCH 28
#define SIGIO 29
#define SIGPOLL SIGIO
#define SIGPWR 30
#define SIGSYS 31
#define SIGRTMIN 34
#define SIGRTMAX 64
#define NSIG 65
#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2
#define SA_NOCLDSTOP 0x00000001
#define SA_NOCLDWAIT 0x00000002
#define SA_SIGINFO 0x00000004
#define SA_ONSTACK 0x08000000
#define SA_RESTART 0x10000000
#define SA_NODEFER 0x40000000
#define SA_RESETHAND 0x80000000
#define SI_USER 0
#define SI_KERNEL 0x80
#define SI_TKILL (-6)
#define SEGV_MAPERR 1
#define SEGV_ACCERR 2
#define BUS_ADRALN 1
#define FPE_INTDIV 1
#define ILL_ILLOPC 1
#define TRAP_BRKPT 1
#define SS_ONSTACK 1
#define SS_DISABLE 2
#define MINSIGSTKSZ 2048
#define SIGSTKSZ 8192
typedef void (*sighandler_t)(int);
#define SIG_DFL ((sighandler_t)0)
#define SIG_IGN ((sighandler_t)1)
#define SIG_ERR ((sighandler_t)-1)
typedef struct { unsigned long long sig[1]; } sigset_t;
typedef int sig_atomic_t;
union sigval { int sival_int; void *sival_ptr; };
typedef struct {
  int si_signo, si_errno, si_code;
  pid_t si_pid;
  uid_t si_uid;
  void *si_addr;
  int si_status;
  union sigval si_value;
} siginfo_t;
struct sigaction {
  union {
    sighandler_t sa_handler;
    void (*sa_sigaction)(int, siginfo_t *, void *);
  };
  sigset_t sa_mask;
  int sa_flags;
};
typedef struct { void *ss_sp; int ss_flags; size_t ss_size; } stack_t;
sighandler_t signal(int, sighandler_t);
int raise(int);
int kill(pid_t, int);
int sigaction(int, const struct sigaction *, struct sigaction *);
int sigprocmask(int, const sigset_t *, sigset_t *);
int pthread_sigmask(int, const sigset_t *, sigset_t *);
int sigsuspend(const sigset_t *);
int sigpending(sigset_t *);
int sigaltstack(const stack_t *, stack_t *);
int sigemptyset(sigset_t *);
int sigfillset(sigset_t *);
int sigaddset(sigset_t *, int);
int sigdelset(sigset_t *, int);
int sigismember(const sigset_t *, int);
int __libc_current_sigrtmin(void);
int __libc_current_sigrtmax(void);
#endif
