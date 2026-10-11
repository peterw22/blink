#ifndef BLINKWIN_ERRNO_H_
#define BLINKWIN_ERRNO_H_
// ucrt's errno (values 1-42 classic, 100-140 POSIX supplement), plus the
// names it lacks. The Rust layer sets errno with these same numbers.
#include_next <errno.h>
#undef EWOULDBLOCK
#define EWOULDBLOCK EAGAIN
#define ESTALE 200
#define EDQUOT 201
#define ESHUTDOWN 202
#define EHOSTDOWN 203
#define ETOOMANYREFS 204
#define EPFNOSUPPORT 205
#define ESOCKTNOSUPPORT 206
#define EMULTIHOP 207
#define ENOTBLK 208
#define EUSERS 209
#define EREMOTE 210
#define ENONET 211
#define ENOMEDIUM 212
#define EMEDIUMTYPE 213
#endif
