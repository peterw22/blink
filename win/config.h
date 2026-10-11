// blink configuration for the Xbox (x86_64-pc-windows-msvc, UWP).
// Used instead of ../blink/config.h; the POSIX pieces blink needs come
// from include/ (declarations) and the blink-xbox crate (Rust).
#ifndef BLINK_CONFIG_H_
#define BLINK_CONFIG_H_

// guest processes run in-process (see blink/procs.h), which needs the
// VFS for per-process file tables; the host can't fork or overlay
#define DISABLE_OVERLAYS
// #define DISABLE_VFS
#define DISABLE_METAL
#define DISABLE_ANCILLARY
#define DISABLE_ROM
#define DISABLE_BCD

// what the Windows host layer provides
#define HAVE_DUP3
#define HAVE_PIPE2
#define HAVE_PREADV
#define HAVE_REALPATH
#define HAVE_SCHED_YIELD
#define HAVE_MAP_ANONYMOUS
#define HAVE_MAP_SHARED
#define HAVE_GETRANDOM
#define HAVE_STRUCT_TIMEZONE

#endif /* BLINK_CONFIG_H_ */
