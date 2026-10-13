#ifndef BLINKWIN_IO_H_
#define BLINKWIN_IO_H_
// ucrt's io.h declares open/read/chmod/... with Windows types; zlib's
// gzguts.h includes it on Windows. The POSIX versions come from unistd.h.
#include <unistd.h>
#include <fcntl.h>
#endif
