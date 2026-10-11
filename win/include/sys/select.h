#ifndef BLINKWIN_SYS_SELECT_H_
#define BLINKWIN_SYS_SELECT_H_
#include <sys/time.h>
#include <signal.h>
#define FD_SETSIZE 1024
typedef struct { unsigned long long fds_bits[FD_SETSIZE / 64]; } fd_set;
#define FD_ZERO(s) __builtin_memset((s), 0, sizeof(fd_set))
#define FD_SET(d, s) ((s)->fds_bits[(d) / 64] |= 1ull << ((d) % 64))
#define FD_CLR(d, s) ((s)->fds_bits[(d) / 64] &= ~(1ull << ((d) % 64)))
#define FD_ISSET(d, s) (!!((s)->fds_bits[(d) / 64] & (1ull << ((d) % 64))))
int select(int, fd_set *, fd_set *, fd_set *, struct timeval *);
#endif
