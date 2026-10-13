#ifndef BLINKWIN_SYS_EVENTFD_H_
#define BLINKWIN_SYS_EVENTFD_H_
#include <stdint.h>
#define EFD_SEMAPHORE 1
#define EFD_CLOEXEC 02000000
#define EFD_NONBLOCK 04000
int eventfd(unsigned, int);
#endif
