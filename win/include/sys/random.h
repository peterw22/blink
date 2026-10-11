#ifndef BLINKWIN_SYS_RANDOM_H_
#define BLINKWIN_SYS_RANDOM_H_
#include <sys/types.h>
#define GRND_NONBLOCK 1
#define GRND_RANDOM 2
ssize_t getrandom(void *, size_t, unsigned);
#endif
