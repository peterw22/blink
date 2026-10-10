/*-*- mode:c;indent-tabs-mode:nil;c-basic-offset:2;tab-width:8;coding:utf-8 -*-│
│ vi: set et ft=c ts=2 sts=2 sw=2 fenc=utf-8                               :vi │
╞══════════════════════════════════════════════════════════════════════════════╡
│ Permission to use, copy, modify, and/or distribute this software for         │
│ any purpose with or without fee is hereby granted, provided that the         │
│ above copyright notice and this permission notice appear in all copies.      │
│                                                                              │
│ THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL                │
│ WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED                │
│ WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE             │
│ AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL         │
│ DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR        │
│ PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER               │
│ TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR             │
│ PERFORMANCE OF THIS SOFTWARE.                                                │
╚─────────────────────────────────────────────────────────────────────────────*/
#include <math.h>
#include <string.h>

#include "blink/endian.h"
#include "blink/flags.h"
#include "blink/fpu.h"
#include "blink/machine.h"
#include "blink/macros.h"
#include "blink/modrm.h"
#include "blink/pun.h"
#include "blink/rde.h"
#include "blink/sse.h"

// SSE4.1 and SSE4.2 (66 0F 38 xx and 66 0F 3A xx) instructions.
// Every op here requires the 66 prefix; there are no MMX forms.

static u8 *GetXmmOrMem(P, size_t n) {
  if (IsModrmRegister(rde)) {
    return XmmRexbRm(m, rde);
  } else {
    return ComputeReserveAddressRead(A, n);
  }
}

static void ReadSrc(P, u8 b[16], size_t n) {
  memset(b, 0, 16);
  memcpy(b, GetXmmOrMem(A, n), n);
}

static void SetPtestFlags(struct Machine *m, bool zf, bool cf) {
  m->flags = SetFlag(m->flags, FLAGS_ZF, zf);
  m->flags = SetFlag(m->flags, FLAGS_CF, cf);
  m->flags = SetFlag(m->flags, FLAGS_AF, false);
  m->flags = SetFlag(m->flags, FLAGS_OF, false);
  m->flags = SetFlag(m->flags, FLAGS_PF, false);
  m->flags = SetFlag(m->flags, FLAGS_SF, false);
}

////////////////////////////////////////////////////////////////////////////////
// rounding

static double RoundEven(double x) {
  double r, d;
  if (!(fabs(x) < 4503599627370496.)) return x;  // NaN, inf, or integral
  r = floor(x);
  d = x - r;
  if (d > .5 || (d == .5 && fmod(r, 2) != 0)) r += 1;
  return copysign(r, x);
}

static double RoundMode(struct Machine *m, double x, int imm) {
  int mode = imm & 4 ? (m->mxcsr & kMxcsrRc) >> 13 : imm & 3;
  switch (mode) {
    case 0:
      return RoundEven(x);
    case 1:
      return floor(x);
    case 2:
      return ceil(x);
    default:
      return trunc(x);
  }
}

static float RoundFloat(struct Machine *m, float x, int imm) {
  if (isnan(x)) {  // signaling NaNs come back quieted
    union FloatPun u = {.f = x};
    u.i |= 0x00400000;
    return u.f;
  }
  return (float)RoundMode(m, x, imm);
}

static double RoundDouble(struct Machine *m, double x, int imm) {
  if (isnan(x)) {
    union DoublePun u = {.f = x};
    u.i |= 0x0008000000000000;
    return u.f;
  }
  return RoundMode(m, x, imm);
}

static void OpRound(P, int op) {
  int i;
  u8 x[16], *d;
  union FloatPun f;
  union DoublePun g;
  d = XmmRexrReg(m, rde);
  switch (op) {
    case 0x08:  // roundps
      ReadSrc(A, x, 16);
      for (i = 0; i < 4; ++i) {
        f.i = Get32(x + i * 4);
        f.f = RoundFloat(m, f.f, uimm0);
        Put32(d + i * 4, f.i);
      }
      break;
    case 0x09:  // roundpd
      ReadSrc(A, x, 16);
      for (i = 0; i < 2; ++i) {
        g.i = Get64(x + i * 8);
        g.f = RoundDouble(m, g.f, uimm0);
        Put64(d + i * 8, g.i);
      }
      break;
    case 0x0A:  // roundss
      ReadSrc(A, x, 4);
      f.i = Get32(x);
      f.f = RoundFloat(m, f.f, uimm0);
      Put32(d, f.i);
      break;
    case 0x0B:  // roundsd
      ReadSrc(A, x, 8);
      g.i = Get64(x);
      g.f = RoundDouble(m, g.f, uimm0);
      Put64(d, g.i);
      break;
    default:
      __builtin_unreachable();
  }
}

////////////////////////////////////////////////////////////////////////////////
// pcmpestri, pcmpestrm, pcmpistri, pcmpistrm

static int GetStrElem(const u8 *p, int i, int fmt) {
  switch (fmt) {
    case 0:
      return p[i];
    case 1:
      return Get16(p + i * 2);
    case 2:
      return (i8)p[i];
    default:
      return (i16)Get16(p + i * 2);
  }
}

static int GetImplicitLength(const u8 *p, int n, int fmt) {
  int i;
  for (i = 0; i < n; ++i) {
    if (!GetStrElem(p, i, fmt)) break;
  }
  return i;
}

static int GetExplicitLength(u64 reg, bool rexw, int n) {
  i64 x = rexw ? (i64)reg : (i32)reg;
  if (x < 0) x = x == INT64_MIN ? INT64_MAX : -x;
  return x > n ? n : x;
}

static void OpPcmpstr(P, int op) {
  bool explicit, wantmask;
  u8 a[16], b[16], *d;
  int i, j, k, n, la, lb, fmt, agg, pol, res1, res2;
  fmt = uimm0 & 3;
  agg = (uimm0 >> 2) & 3;
  pol = (uimm0 >> 4) & 3;
  n = fmt & 1 ? 8 : 16;
  explicit = !(op & 2);
  wantmask = !(op & 1);
  memcpy(a, XmmRexrReg(m, rde), 16);
  ReadSrc(A, b, 16);
  if (explicit) {
    la = GetExplicitLength(Get64(m->ax), Rexw(rde), n);
    lb = GetExplicitLength(Get64(m->dx), Rexw(rde), n);
  } else {
    la = GetImplicitLength(a, n, fmt);
    lb = GetImplicitLength(b, n, fmt);
  }
  res1 = 0;
  for (j = 0; j < n; ++j) {
    bool r = false;
    switch (agg) {
      case 0:  // equal any
        if (j < lb) {
          for (i = 0; i < la && !r; ++i) {
            r = GetStrElem(a, i, fmt) == GetStrElem(b, j, fmt);
          }
        }
        break;
      case 1:  // ranges
        if (j < lb) {
          for (i = 0; i + 1 < la && !r; i += 2) {
            r = GetStrElem(a, i, fmt) <= GetStrElem(b, j, fmt) &&
                GetStrElem(b, j, fmt) <= GetStrElem(a, i + 1, fmt);
          }
        }
        break;
      case 2:  // equal each
        if (j < la && j < lb) {
          r = GetStrElem(a, j, fmt) == GetStrElem(b, j, fmt);
        } else {
          r = j >= la && j >= lb;
        }
        break;
      case 3:  // equal ordered
        r = true;
        for (i = 0; i < n - j && r; ++i) {
          k = j + i;
          if (i >= la) break;  // needle exhausted: matches
          if (k >= lb) {
            r = false;  // haystack exhausted before needle
          } else {
            r = GetStrElem(a, i, fmt) == GetStrElem(b, k, fmt);
          }
        }
        break;
      default:
        __builtin_unreachable();
    }
    res1 |= r << j;
  }
  switch (pol) {
    case 1:
      res2 = ~res1;
      break;
    case 3:
      res2 = res1 ^ ((1 << lb) - 1);
      break;
    default:
      res2 = res1;
      break;
  }
  res2 &= (1 << n) - 1;
  if (wantmask) {
    d = m->xmm[0];
    memset(d, 0, 16);
    if (uimm0 & 0x40) {
      for (j = 0; j < n; ++j) {
        if (res2 & (1 << j)) {
          memset(d + j * (16 / n), 0xff, 16 / n);
        }
      }
    } else {
      Put16(d, res2);
    }
  } else {
    if (!res2) {
      i = n;
    } else if (uimm0 & 0x40) {
      i = 31 - __builtin_clz(res2);
    } else {
      i = __builtin_ctz(res2);
    }
    Put64(m->cx, i);
  }
  m->flags = SetFlag(m->flags, FLAGS_CF, res2 != 0);
  m->flags = SetFlag(m->flags, FLAGS_ZF, lb < n);
  m->flags = SetFlag(m->flags, FLAGS_SF, la < n);
  m->flags = SetFlag(m->flags, FLAGS_OF, res2 & 1);
  m->flags = SetFlag(m->flags, FLAGS_AF, false);
  m->flags = SetFlag(m->flags, FLAGS_PF, false);
}

////////////////////////////////////////////////////////////////////////////////
// 66 0F 38 xx

static void PmovExtend(P, int from, int to, bool sign) {
  int i, n = 16 / to;
  u8 x[16], y[16];
  u64 v;
  ReadSrc(A, x, n * from);
  for (i = 0; i < n; ++i) {
    switch (from) {
      case 1:
        v = sign ? (u64)(i64)(i8)x[i] : x[i];
        break;
      case 2:
        v = sign ? (u64)(i64)(i16)Get16(x + i * 2) : Get16(x + i * 2);
        break;
      default:
        v = sign ? (u64)(i64)(i32)Get32(x + i * 4) : Get32(x + i * 4);
        break;
    }
    switch (to) {
      case 2:
        Put16(y + i * 2, v);
        break;
      case 4:
        Put32(y + i * 4, v);
        break;
      default:
        Put64(y + i * 8, v);
        break;
    }
  }
  memcpy(XmmRexrReg(m, rde), y, 16);
}

static void Op0f38(P, int op) {
  int i;
  u8 x[16], *d;
  d = XmmRexrReg(m, rde);
  switch (op) {
    case 0x20:
      PmovExtend(A, 1, 2, true);
      return;
    case 0x21:
      PmovExtend(A, 1, 4, true);
      return;
    case 0x22:
      PmovExtend(A, 1, 8, true);
      return;
    case 0x23:
      PmovExtend(A, 2, 4, true);
      return;
    case 0x24:
      PmovExtend(A, 2, 8, true);
      return;
    case 0x25:
      PmovExtend(A, 4, 8, true);
      return;
    case 0x30:
      PmovExtend(A, 1, 2, false);
      return;
    case 0x31:
      PmovExtend(A, 1, 4, false);
      return;
    case 0x32:
      PmovExtend(A, 1, 8, false);
      return;
    case 0x33:
      PmovExtend(A, 2, 4, false);
      return;
    case 0x34:
      PmovExtend(A, 2, 8, false);
      return;
    case 0x35:
      PmovExtend(A, 4, 8, false);
      return;
    default:
      break;
  }
  ReadSrc(A, x, 16);
  switch (op) {
    case 0x10:  // pblendvb
      for (i = 0; i < 16; ++i) {
        if (m->xmm[0][i] & 0x80) d[i] = x[i];
      }
      break;
    case 0x14:  // blendvps
      for (i = 0; i < 4; ++i) {
        if (m->xmm[0][i * 4 + 3] & 0x80) memcpy(d + i * 4, x + i * 4, 4);
      }
      break;
    case 0x15:  // blendvpd
      for (i = 0; i < 2; ++i) {
        if (m->xmm[0][i * 8 + 7] & 0x80) memcpy(d + i * 8, x + i * 8, 8);
      }
      break;
    case 0x17: {  // ptest
      bool zf = true, cf = true;
      for (i = 0; i < 16; ++i) {
        if (x[i] & d[i]) zf = false;
        if (x[i] & ~d[i]) cf = false;
      }
      SetPtestFlags(m, zf, cf);
      break;
    }
    case 0x28:  // pmuldq
      for (i = 0; i < 2; ++i) {
        Put64(d + i * 8, (u64)((i64)(i32)Get32(d + i * 8) *
                               (i64)(i32)Get32(x + i * 8)));
      }
      break;
    case 0x29:  // pcmpeqq
      for (i = 0; i < 2; ++i) {
        Put64(d + i * 8, Get64(d + i * 8) == Get64(x + i * 8) ? -1 : 0);
      }
      break;
    case 0x2B: {  // packusdw
      u8 y[16];
      for (i = 0; i < 8; ++i) {
        i32 v = (i32)Get32(i < 4 ? d + i * 4 : x + (i - 4) * 4);
        Put16(y + i * 2, MAX(0, MIN(65535, v)));
      }
      memcpy(d, y, 16);
      break;
    }
    case 0x37:  // pcmpgtq
      for (i = 0; i < 2; ++i) {
        Put64(d + i * 8,
              (i64)Get64(d + i * 8) > (i64)Get64(x + i * 8) ? -1 : 0);
      }
      break;
    case 0x38:  // pminsb
      for (i = 0; i < 16; ++i) d[i] = MIN((i8)d[i], (i8)x[i]);
      break;
    case 0x39:  // pminsd
      for (i = 0; i < 4; ++i) {
        Put32(d + i * 4, MIN((i32)Get32(d + i * 4), (i32)Get32(x + i * 4)));
      }
      break;
    case 0x3A:  // pminuw
      for (i = 0; i < 8; ++i) {
        Put16(d + i * 2, MIN(Get16(d + i * 2), Get16(x + i * 2)));
      }
      break;
    case 0x3B:  // pminud
      for (i = 0; i < 4; ++i) {
        Put32(d + i * 4, MIN(Get32(d + i * 4), Get32(x + i * 4)));
      }
      break;
    case 0x3C:  // pmaxsb
      for (i = 0; i < 16; ++i) d[i] = MAX((i8)d[i], (i8)x[i]);
      break;
    case 0x3D:  // pmaxsd
      for (i = 0; i < 4; ++i) {
        Put32(d + i * 4, MAX((i32)Get32(d + i * 4), (i32)Get32(x + i * 4)));
      }
      break;
    case 0x3E:  // pmaxuw
      for (i = 0; i < 8; ++i) {
        Put16(d + i * 2, MAX(Get16(d + i * 2), Get16(x + i * 2)));
      }
      break;
    case 0x3F:  // pmaxud
      for (i = 0; i < 4; ++i) {
        Put32(d + i * 4, MAX(Get32(d + i * 4), Get32(x + i * 4)));
      }
      break;
    case 0x41: {  // phminposuw
      int j = 0;
      for (i = 1; i < 8; ++i) {
        if (Get16(x + i * 2) < Get16(x + j * 2)) j = i;
      }
      memset(d, 0, 16);
      Put16(d, Get16(x + j * 2));
      Put16(d + 2, j);
      break;
    }
    default:
      OpUdImpl(m);
  }
}

////////////////////////////////////////////////////////////////////////////////
// 66 0F 3A xx

static void Op0f3a(P, int op) {
  int i;
  u8 x[16], *d, *p;
  d = XmmRexrReg(m, rde);
  switch (op) {
    case 0x08:
    case 0x09:
    case 0x0A:
    case 0x0B:
      OpRound(A, op);
      break;
    case 0x0C:  // blendps
      ReadSrc(A, x, 16);
      for (i = 0; i < 4; ++i) {
        if (uimm0 & (1 << i)) memcpy(d + i * 4, x + i * 4, 4);
      }
      break;
    case 0x0D:  // blendpd
      ReadSrc(A, x, 16);
      for (i = 0; i < 2; ++i) {
        if (uimm0 & (1 << i)) memcpy(d + i * 8, x + i * 8, 8);
      }
      break;
    case 0x0E:  // pblendw
      ReadSrc(A, x, 16);
      for (i = 0; i < 8; ++i) {
        if (uimm0 & (1 << i)) memcpy(d + i * 2, x + i * 2, 2);
      }
      break;
    case 0x14:  // pextrb
      if (IsModrmRegister(rde)) {
        Put64(RegRexbRm(m, rde), d[uimm0 & 15]);
      } else {
        p = ComputeReserveAddressWrite1(A);
        p[0] = d[uimm0 & 15];
      }
      break;
    case 0x15:  // pextrw
      if (IsModrmRegister(rde)) {
        Put64(RegRexbRm(m, rde), Get16(d + (uimm0 & 7) * 2));
      } else {
        p = ComputeReserveAddressWrite(A, 2);
        Put16(p, Get16(d + (uimm0 & 7) * 2));
      }
      break;
    case 0x16:  // pextrd, pextrq
      if (Rexw(rde)) {
        p = GetModrmRegisterWordPointerWrite8(A);
        Put64(p, Get64(d + (uimm0 & 1) * 8));
      } else if (IsModrmRegister(rde)) {
        Put64(RegRexbRm(m, rde), Get32(d + (uimm0 & 3) * 4));
      } else {
        p = ComputeReserveAddressWrite4(A);
        Put32(p, Get32(d + (uimm0 & 3) * 4));
      }
      break;
    case 0x17:  // extractps
      if (IsModrmRegister(rde)) {
        Put64(RegRexbRm(m, rde), Get32(d + (uimm0 & 3) * 4));
      } else {
        p = ComputeReserveAddressWrite4(A);
        Put32(p, Get32(d + (uimm0 & 3) * 4));
      }
      break;
    case 0x20:  // pinsrb
      if (IsModrmRegister(rde)) {
        d[uimm0 & 15] = RegRexbRm(m, rde)[0];
      } else {
        d[uimm0 & 15] = ComputeReserveAddressRead1(A)[0];
      }
      break;
    case 0x21: {  // insertps
      u32 v;
      if (IsModrmRegister(rde)) {
        v = Get32(XmmRexbRm(m, rde) + ((uimm0 >> 6) & 3) * 4);
      } else {
        v = Get32(ComputeReserveAddressRead4(A));
      }
      Put32(d + ((uimm0 >> 4) & 3) * 4, v);
      for (i = 0; i < 4; ++i) {
        if (uimm0 & (1 << i)) Put32(d + i * 4, 0);
      }
      break;
    }
    case 0x22:  // pinsrd, pinsrq
      if (Rexw(rde)) {
        Put64(d + (uimm0 & 1) * 8,
              Get64(GetModrmRegisterWordPointerRead8(A)));
      } else {
        Put32(d + (uimm0 & 3) * 4,
              Get32(GetModrmRegisterWordPointerRead4(A)));
      }
      break;
    case 0x40: {  // dpps
      float s, t, prod[4];
      union FloatPun a, b;
      ReadSrc(A, x, 16);
      for (i = 0; i < 4; ++i) {
        a.i = Get32(d + i * 4);
        b.i = Get32(x + i * 4);
        prod[i] = uimm0 & (0x10 << i) ? a.f * b.f : 0.f;
      }
      s = prod[0] + prod[1];
      t = prod[2] + prod[3];
      a.f = s + t;
      for (i = 0; i < 4; ++i) {
        Put32(d + i * 4, uimm0 & (1 << i) ? a.i : 0);
      }
      break;
    }
    case 0x41: {  // dppd
      double prod[2];
      union DoublePun a, b;
      ReadSrc(A, x, 16);
      for (i = 0; i < 2; ++i) {
        a.i = Get64(d + i * 8);
        b.i = Get64(x + i * 8);
        prod[i] = uimm0 & (0x10 << i) ? a.f * b.f : 0.;
      }
      a.f = prod[0] + prod[1];
      for (i = 0; i < 2; ++i) {
        Put64(d + i * 8, uimm0 & (1 << i) ? a.i : 0);
      }
      break;
    }
    case 0x42: {  // mpsadbw
      u8 y[16], s[16];
      int k, o1, o2, sum;
      ReadSrc(A, x, 16);
      memcpy(s, d, 16);
      o1 = uimm0 & 4 ? 4 : 0;
      o2 = (uimm0 & 3) * 4;
      for (i = 0; i < 8; ++i) {
        for (sum = k = 0; k < 4; ++k) {
          sum += ABS(s[o1 + i + k] - x[o2 + k]);
        }
        Put16(y + i * 2, sum);
      }
      memcpy(d, y, 16);
      break;
    }
    case 0x60:
    case 0x61:
    case 0x62:
    case 0x63:
      OpPcmpstr(A, op);
      break;
    default:
      OpUdImpl(m);
  }
}

void OpSse4(P) {
  int op;
  if (!Osz(rde)) {
    OpUdImpl(m);
    return;
  }
  op = Mopcode(rde);
  IGNORE_RACES_START();
  if (op < 0x300) {
    Op0f38(A, op & 0xff);
  } else {
    Op0f3a(A, op & 0xff);
  }
  IGNORE_RACES_END();
}
