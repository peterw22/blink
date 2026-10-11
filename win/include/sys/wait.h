#ifndef BLINKWIN_SYS_WAIT_H_
#define BLINKWIN_SYS_WAIT_H_
#include <sys/types.h>
#include <sys/resource.h>
#define WNOHANG 1
#define WUNTRACED 2
#define WSTOPPED WUNTRACED
#define WEXITED 4
#define WCONTINUED 8
#define WNOWAIT 0x1000000
#define WEXITSTATUS(s) (((s) & 0xff00) >> 8)
#define WTERMSIG(s) ((s) & 0x7f)
#define WSTOPSIG(s) WEXITSTATUS(s)
#define WIFEXITED(s) (!WTERMSIG(s))
#define WIFSTOPPED(s) (((s) & 0xff) == 0x7f)
#define WIFSIGNALED(s) (((s) & 0xffff) - 1u < 0xffu)
#define WIFCONTINUED(s) ((s) == 0xffff)
#define WCOREDUMP(s) ((s) & 0x80)
pid_t wait(int *);
pid_t waitpid(pid_t, int *, int);
pid_t wait4(pid_t, int *, int, struct rusage *);
pid_t fork(void);
pid_t vfork(void);
#endif
