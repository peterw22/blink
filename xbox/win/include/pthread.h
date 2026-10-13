#ifndef BLINKWIN_PTHREAD_H_
#define BLINKWIN_PTHREAD_H_
#include <sys/types.h>
#include <sched.h>
#include <time.h>
// All objects are zero-initializable words; the Rust side keeps Windows
// SRW locks / condition variables / INIT_ONCE in them.
typedef unsigned long long pthread_t;
typedef struct { unsigned long long w[5]; } pthread_mutex_t;
typedef struct { unsigned long long w[2]; } pthread_cond_t;
typedef struct { unsigned long long w[1]; } pthread_once_t;
typedef struct { unsigned int type, pshared; } pthread_mutexattr_t;
typedef struct { unsigned int pshared, clock; } pthread_condattr_t;
typedef struct { unsigned long long stacksize; int detached; } pthread_attr_t;
typedef unsigned int pthread_key_t;
#define PTHREAD_MUTEX_INITIALIZER {{0}}
#define PTHREAD_COND_INITIALIZER {{0}}
#define PTHREAD_ONCE_INIT {{0}}
#define PTHREAD_MUTEX_NORMAL 0
#define PTHREAD_MUTEX_RECURSIVE 1
#define PTHREAD_MUTEX_ERRORCHECK 2
#define PTHREAD_MUTEX_DEFAULT PTHREAD_MUTEX_NORMAL
#define PTHREAD_PROCESS_PRIVATE 0
#define PTHREAD_PROCESS_SHARED 1
#define PTHREAD_CREATE_JOINABLE 0
#define PTHREAD_CREATE_DETACHED 1
#define PTHREAD_CANCEL_ENABLE 0
#define PTHREAD_CANCEL_DISABLE 1
int pthread_create(pthread_t *, const pthread_attr_t *, void *(*)(void *), void *);
int pthread_join(pthread_t, void **);
int pthread_detach(pthread_t);
void pthread_exit(void *) __attribute__((__noreturn__));
pthread_t pthread_self(void);
int pthread_equal(pthread_t, pthread_t);
int pthread_kill(pthread_t, int);
int pthread_once(pthread_once_t *, void (*)(void));
int pthread_atfork(void (*)(void), void (*)(void), void (*)(void));
int pthread_setcancelstate(int, int *);
int pthread_attr_init(pthread_attr_t *);
int pthread_attr_destroy(pthread_attr_t *);
int pthread_attr_setdetachstate(pthread_attr_t *, int);
int pthread_attr_setstacksize(pthread_attr_t *, size_t);
int pthread_mutexattr_init(pthread_mutexattr_t *);
int pthread_mutexattr_destroy(pthread_mutexattr_t *);
int pthread_mutexattr_settype(pthread_mutexattr_t *, int);
int pthread_mutexattr_setpshared(pthread_mutexattr_t *, int);
int pthread_mutex_init(pthread_mutex_t *, const pthread_mutexattr_t *);
int pthread_mutex_destroy(pthread_mutex_t *);
int pthread_mutex_lock(pthread_mutex_t *);
int pthread_mutex_trylock(pthread_mutex_t *);
int pthread_mutex_unlock(pthread_mutex_t *);
int pthread_condattr_init(pthread_condattr_t *);
int pthread_condattr_destroy(pthread_condattr_t *);
int pthread_condattr_setpshared(pthread_condattr_t *, int);
int pthread_cond_init(pthread_cond_t *, const pthread_condattr_t *);
int pthread_cond_destroy(pthread_cond_t *);
int pthread_cond_wait(pthread_cond_t *, pthread_mutex_t *);
int pthread_cond_timedwait(pthread_cond_t *, pthread_mutex_t *, const struct timespec *);
int pthread_cond_signal(pthread_cond_t *);
int pthread_cond_broadcast(pthread_cond_t *);
int pthread_key_create(pthread_key_t *, void (*)(void *));
void *pthread_getspecific(pthread_key_t);
int pthread_setspecific(pthread_key_t, const void *);
#endif
