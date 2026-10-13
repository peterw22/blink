#ifndef BLINKWIN_SETJMP_H_
#define BLINKWIN_SETJMP_H_
// ucrt's longjmp unwinds the stack with SEH, which needs unwind info for
// every frame; blink longjmps out of C called from JIT-generated code that
// has none. These save and restore callee-saved state without unwinding
// (win/setjmp.S). Signal masks aren't saved: host signals are emulated.
typedef __attribute__((__aligned__(16))) unsigned long long jmp_buf[32];
typedef jmp_buf sigjmp_buf;
int blinkwin_setjmp(jmp_buf) __attribute__((__returns_twice__));
void blinkwin_longjmp(jmp_buf, int) __attribute__((__noreturn__));
#define setjmp(b) blinkwin_setjmp(b)
#define _setjmp(b) blinkwin_setjmp(b)
#define longjmp(b, v) blinkwin_longjmp(b, v)
#define _longjmp(b, v) blinkwin_longjmp(b, v)
#define sigsetjmp(b, s) blinkwin_setjmp(b)
#define siglongjmp(b, v) blinkwin_longjmp(b, v)
#endif
