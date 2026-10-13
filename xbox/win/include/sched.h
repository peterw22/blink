#ifndef BLINKWIN_SCHED_H_
#define BLINKWIN_SCHED_H_
#include <sys/types.h>
#define CPU_SETSIZE 1024
typedef struct { unsigned long long bits[CPU_SETSIZE / 64]; } cpu_set_t;
#define CPU_ZERO(s) __builtin_memset((s), 0, sizeof(cpu_set_t))
#define CPU_SET(i, s) ((s)->bits[(i) / 64] |= 1ull << ((i) % 64))
#define CPU_ISSET(i, s) (!!((s)->bits[(i) / 64] & (1ull << ((i) % 64))))
#define CPU_COUNT(s) __sched_cpucount(sizeof(cpu_set_t), (s))
int __sched_cpucount(size_t, const cpu_set_t *);
int sched_yield(void);
int sched_getaffinity(pid_t, size_t, cpu_set_t *);
int sched_setaffinity(pid_t, size_t, const cpu_set_t *);
#endif
