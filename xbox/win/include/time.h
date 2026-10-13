#ifndef BLINKWIN_TIME_H_
#define BLINKWIN_TIME_H_
#include_next <time.h>
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_PROCESS_CPUTIME_ID 2
#define CLOCK_THREAD_CPUTIME_ID 3
#define CLOCK_MONOTONIC_RAW 4
#define CLOCK_REALTIME_COARSE 5
#define CLOCK_MONOTONIC_COARSE 6
#define CLOCK_BOOTTIME 7
#define TIMER_ABSTIME 1
int clock_gettime(int, struct timespec *);
int clock_getres(int, struct timespec *);
int clock_settime(int, const struct timespec *);
int clock_nanosleep(int, int, const struct timespec *, struct timespec *);
int nanosleep(const struct timespec *, struct timespec *);
struct tm *gmtime_r(const time_t *, struct tm *);
struct tm *localtime_r(const time_t *, struct tm *);
#endif
