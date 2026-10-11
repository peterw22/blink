// Things blink's C expects from the toolchain or from files left out of
// the Windows build (blinkenlights.c, the text-mode TUI debugger).
#include <stdbool.h>
#include <sys/types.h>

// blinkenlights hooks referenced by bios.c, cga.c and mda.c (metal mode,
// which the Xbox build never runs); on ELF these are weak references
struct Machine;
struct Pty;
int ttyin;
int vidya;
bool tuimode;
struct Pty *pty;
struct Machine *m;
bool ptyisenabled;
void SetCarry(bool cf) {}
void ReactiveDraw(void) {}
void Redraw(bool force) {}
void DrawDisplayOnly(void) {}
bool HasPendingKeyboard(void) { return false; }
void HandleAppReadInterrupt(bool errflag) {}
ssize_t ReadAnsi(int fd, char *p, size_t n) { return 0; }

// 128-bit division for divmul.c (x86 div with 64-bit operands); there's no
// compiler-rt for this target, so shift-and-subtract (no recursion: only
// shifts, compares and subtractions on unsigned __int128)
typedef unsigned __int128 u128;
typedef __int128 i128;

u128 __udivmodti4(u128 n, u128 d, u128 *rem) {
  u128 q = 0, r = 0;
  int i;
  if (!(d >> 64) && !(n >> 64)) {
    unsigned long long a = (unsigned long long)n, b = (unsigned long long)d;
    if (rem) *rem = a % b;
    return a / b;
  }
  for (i = 127; i >= 0; --i) {
    r = (r << 1) | ((n >> i) & 1);
    if (r >= d) {
      r -= d;
      q |= (u128)1 << i;
    }
  }
  if (rem) *rem = r;
  return q;
}

u128 __udivti3(u128 n, u128 d) {
  return __udivmodti4(n, d, 0);
}

u128 __umodti3(u128 n, u128 d) {
  u128 r;
  __udivmodti4(n, d, &r);
  return r;
}

i128 __divti3(i128 n, i128 d) {
  int neg = (n < 0) ^ (d < 0);
  u128 q = __udivmodti4(n < 0 ? -(u128)n : (u128)n, d < 0 ? -(u128)d : (u128)d, 0);
  return neg ? -(i128)q : (i128)q;
}

i128 __modti3(i128 n, i128 d) {
  u128 r;
  __udivmodti4(n < 0 ? -(u128)n : (u128)n, d < 0 ? -(u128)d : (u128)d, &r);
  return n < 0 ? -(i128)r : (i128)r;
}
