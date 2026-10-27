/* SPDX-License-Identifier: Apache-2.0
 * Authorized for public redistribution; see native/LICENSE-overlay and
 * the crate's LICENSE for the applicable license terms.
 */
/* Overlay for `#include <x86intrin.h>` on non-x86 hosts.
 *
 * labrador's kernels only need the immintrin.h emulation.  lazer core
 * (lazer-in2.h) additionally calls the ADX carry intrinsics on every
 * non-iOS target, TARGET_GENERIC included — its iOS branch uses the
 * Clang-only __builtin_{add,sub}cll, so on GCC/aarch64 we supply the
 * Intel semantics directly:
 *   _addcarry_u64:  {carry_out, *out} = x + y + (c_in != 0)
 *   _subborrow_u64: {borrow_out, *out} = x - y - (b_in != 0)
 */
#ifndef SCALAR512_X86INTRIN_H
#define SCALAR512_X86INTRIN_H
#include "immintrin.h"

static inline unsigned char
_addcarry_u64 (unsigned char c_in, unsigned long long x, unsigned long long y,
               unsigned long long *out)
{
  unsigned long long t = x + (unsigned long long)(c_in != 0);
  unsigned char c = t < x;
  unsigned long long s = t + y;
  c |= s < y;
  *out = s;
  return c;
}

static inline unsigned char
_subborrow_u64 (unsigned char b_in, unsigned long long x, unsigned long long y,
                unsigned long long *out)
{
  unsigned long long d = x - y;
  unsigned char b = x < y;
  unsigned long long r = d - (unsigned long long)(b_in != 0);
  b |= d < (unsigned long long)(b_in != 0);
  *out = r;
  return b;
}

#endif
