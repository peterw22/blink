#ifndef BLINKWIN_SYS_TIMES_H_
#define BLINKWIN_SYS_TIMES_H_
#include <sys/types.h>
typedef long long clock_t_posix;
struct tms { long long tms_utime, tms_stime, tms_cutime, tms_cstime; };
long long times(struct tms *);
#endif
