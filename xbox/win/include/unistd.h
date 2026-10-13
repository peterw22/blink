#ifndef BLINKWIN_UNISTD_H_
#define BLINKWIN_UNISTD_H_
#include <sys/types.h>
#include <stdio.h>
#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4
#define SEEK_DATA 3
#define SEEK_HOLE 4
#define _SC_PAGESIZE 30
#define _SC_PAGE_SIZE _SC_PAGESIZE
#define _SC_NPROCESSORS_ONLN 84
#define _SC_NPROCESSORS_CONF 83
#define _SC_CLK_TCK 2
#define _SC_OPEN_MAX 4
#define _SC_ARG_MAX 0
#define _SC_PHYS_PAGES 85
#define _SC_GETPW_R_SIZE_MAX 70
#define _SC_HOST_NAME_MAX 180
#define _SC_NGROUPS_MAX 3
extern char **environ;
extern char *optarg;
extern int optind, opterr, optopt;
int access(const char *, int);
int faccessat(int, const char *, int, int);
unsigned alarm(unsigned);
int chdir(const char *);
int fchdir(int);
int chown(const char *, uid_t, gid_t);
int fchown(int, uid_t, gid_t);
int fchownat(int, const char *, uid_t, gid_t, int);
int close(int);
int dup(int);
int dup2(int, int);
int dup3(int, int, int);
int execve(const char *, char *const[], char *const[]);
int execv(const char *, char *const[]);
int fexecve(int, char *const[], char *const[]);
void _exit(int) __attribute__((__noreturn__));
int fdatasync(int);
int fsync(int);
int ftruncate(int, off_t);
int truncate(const char *, off_t);
char *getcwd(char *, size_t);
gid_t getegid(void);
uid_t geteuid(void);
gid_t getgid(void);
uid_t getuid(void);
int getgroups(int, gid_t[]);
int setgroups(size_t, const gid_t *);
int gethostname(char *, size_t);
int getdomainname(char *, size_t);
pid_t getpgid(pid_t);
pid_t getpgrp(void);
pid_t getpid(void);
pid_t getppid(void);
pid_t getsid(pid_t);
pid_t setsid(void);
int setpgid(pid_t, pid_t);
int getresuid(uid_t *, uid_t *, uid_t *);
int getresgid(gid_t *, gid_t *, gid_t *);
int setresuid(uid_t, uid_t, uid_t);
int setresgid(gid_t, gid_t, gid_t);
int setreuid(uid_t, uid_t);
int setregid(gid_t, gid_t);
int setuid(uid_t);
int setgid(gid_t);
int seteuid(uid_t);
int setegid(gid_t);
int isatty(int);
char *ttyname(int);
int link(const char *, const char *);
int linkat(int, const char *, int, const char *, int);
off_t lseek(int, off_t, int);
int pause(void);
int pipe(int[2]);
int pipe2(int[2], int);
ssize_t pread(int, void *, size_t, off_t);
ssize_t pwrite(int, const void *, size_t, off_t);
ssize_t read(int, void *, size_t);
ssize_t write(int, const void *, size_t);
ssize_t readlink(const char *, char *, size_t);
ssize_t readlinkat(int, const char *, char *, size_t);
int rmdir(const char *);
int symlink(const char *, const char *);
int symlinkat(const char *, int, const char *);
void sync(void);
long sysconf(int);
pid_t tcgetpgrp(int);
int tcsetpgrp(int, pid_t);
int unlink(const char *);
int unlinkat(int, const char *, int);
unsigned sleep(unsigned);
int usleep(useconds_t);
int getopt(int, char *const[], const char *);
int getpagesize(void);
int getentropy(void *, size_t);
long syscall(long, ...);
#endif
