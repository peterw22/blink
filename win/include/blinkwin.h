// Force-included into every blink C file built for Windows (see build.rs).
// Turns off ucrt's old non-standard names (its open/read/stat have Windows
// semantics) and sets up POSIX basics. The POSIX functions declared by the
// headers in this directory are implemented in Rust (blink-xbox src/win).
#ifndef BLINKWIN_H_
#define BLINKWIN_H_
#define _CRT_DECLARE_NONSTDC_NAMES 0
#define _CRT_SECURE_NO_WARNINGS 1
#define _CRT_RAND_S 1
#define _USE_MATH_DEFINES 1
// keep ucrt from defining these with Windows widths (see sys/types.h)
#define _OFF_T_DEFINED 1
#define _INO_T_DEFINED 1
#define _DEV_T_DEFINED 1
#define _PID_T_DEFINED 1
#define _MODE_T_DEFINED 1
// Rust's std links Winsock (ws2_32), which exports these names with
// Windows semantics; blink's POSIX versions are renamed blinkwin_<name>
#define socket blinkwin_socket
#define socketpair blinkwin_socketpair
#define bind blinkwin_bind
#define connect blinkwin_connect
#define listen blinkwin_listen
#define accept blinkwin_accept
#define accept4 blinkwin_accept4
#define shutdown blinkwin_shutdown
#define getsockname blinkwin_getsockname
#define getpeername blinkwin_getpeername
#define getsockopt blinkwin_getsockopt
#define setsockopt blinkwin_setsockopt
#define send blinkwin_send
#define recv blinkwin_recv
#define sendto blinkwin_sendto
#define recvfrom blinkwin_recvfrom
#define sendmsg blinkwin_sendmsg
#define recvmsg blinkwin_recvmsg
#define select blinkwin_select
#define gethostname blinkwin_gethostname
#define inet_ntop blinkwin_inet_ntop
#define inet_pton blinkwin_inet_pton
#define sockatmark blinkwin_sockatmark
// ucrt has signal() and raise() for its handful of console signals
#define signal blinkwin_signal
#define raise blinkwin_raise
#include <sys/types.h>
// blink.c sets the console code page on Windows; UWP apps have no console
#define CP_UTF8 65001
static inline int SetConsoleOutputCP(unsigned cp) { (void)cp; return 1; }
#endif
