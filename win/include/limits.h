#ifndef BLINKWIN_LIMITS_H_
#define BLINKWIN_LIMITS_H_
#include_next <limits.h>
#define PATH_MAX 4096
#define NAME_MAX 255
#define SSIZE_MAX LLONG_MAX
#define IOV_MAX 1024
#define HOST_NAME_MAX 64
#define NGROUPS_MAX 65536
#define PIPE_BUF 4096
#endif
