#ifndef BLINKWIN_SYS_TIME_H_
#define BLINKWIN_SYS_TIME_H_
#include <sys/types.h>
#include <time.h>
struct timeval { long long tv_sec; long long tv_usec; };
struct timezone { int tz_minuteswest, tz_dsttime; };
struct itimerval { struct timeval it_interval, it_value; };
#define ITIMER_REAL 0
#define ITIMER_VIRTUAL 1
#define ITIMER_PROF 2
int gettimeofday(struct timeval *, void *);
int getitimer(int, struct itimerval *);
int setitimer(int, const struct itimerval *, struct itimerval *);
int utimes(const char *, const struct timeval[2]);
#endif
