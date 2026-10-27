/* SPDX-License-Identifier: Apache-2.0
 * Authorized for public redistribution; see native/LICENSE-overlay and
 * the crate's LICENSE for the applicable license terms.
 */
/* Scalar emulation of the AVX-512 intrinsic subset used by labrador's
 * ntt.c / poly.c / polz.c / jlproj.c (pinned commit 3f95485) plus the types
 * pulled in through data.h / poly.h / polz.h.  Phase 1.5 portable-C spike.
 *
 * Drop this directory on the include path AHEAD of the system headers
 * (data.h does `#include <immintrin.h>`) and the upstream kernel files
 * compile unchanged on any little-endian target; outputs are bit-exact with
 * the AVX-512 build by construction (each intrinsic is implemented with the
 * exact per-lane semantics of the Intel SDM, element 0 = lowest address).
 * Validated against the golden vectors in results/phase15/vectors/.
 *
 * Portability assumptions (documented, checked where cheap):
 *  - little-endian target (x86, aarch64 linux)
 *  - GCC/Clang semantics for signed-int right shift (arithmetic) and
 *    out-of-range narrowing casts (modular) — both are guaranteed by the
 *    compilers this project builds with
 *  - _mm512_castsi128_si512 / _mm512_castsi256_si512 zero the upper lanes
 *    (upstream code never depends on the "undefined" lanes, so any fixed
 *    choice reproduces the AVX-512 results)
 */
#ifndef SCALAR512_IMMINTRIN_H
#define SCALAR512_IMMINTRIN_H

#include <stdint.h>
#include <string.h>
#include <sys/types.h> /* ssize_t — the real immintrin.h provides it transitively
                          and poly.h/polz.h rely on that */
#include <stdalign.h>  /* alignas — jlproj.c uses the keyword spelling, which is
                          C23-only on older gcc; the macro covers both */

#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error "scalar512 immintrin.h requires a little-endian target"
#endif

typedef union __attribute__((aligned(16))) {
  int8_t   i8[16];  uint8_t  u8[16];
  int16_t i16[8];   uint16_t u16[8];
  int32_t i32[4];   uint32_t u32[4];
  int64_t i64[2];   uint64_t u64[2];
} __m128i;

typedef union __attribute__((aligned(32))) {
  int8_t   i8[32];  uint8_t  u8[32];
  int16_t i16[16];  uint16_t u16[16];
  int32_t i32[8];   uint32_t u32[8];
  int64_t i64[4];   uint64_t u64[4];
} __m256i;

typedef union __attribute__((aligned(64))) {
  int8_t   i8[64];  uint8_t  u8[64];
  int16_t i16[32];  uint16_t u16[32];
  int32_t i32[16];  uint32_t u32[16];
  int64_t i64[8];   uint64_t u64[8];
} __m512i;

typedef __m512i __m512; /* only used for bit-pattern shuffles (moveldup) */

typedef uint8_t  __mmask8;
typedef uint16_t __mmask16;
typedef uint32_t __mmask32;
typedef uint64_t __mmask64;

#define SC512_INLINE static inline __attribute__((always_inline, unused))

/* ---- mask helpers ---- */
SC512_INLINE __mmask32 _cvtu32_mask32(uint32_t a) { return a; }
SC512_INLINE __mmask64 _cvtu64_mask64(uint64_t a) { return a; }
SC512_INLINE __mmask32 _knot_mask32(__mmask32 a) { return ~a; }
SC512_INLINE uint32_t _cvtmask32_u32(__mmask32 a) { return a; }
SC512_INLINE int _popcnt32(int a) { return __builtin_popcount((unsigned)a); }

/* ---- loads / stores ---- */
SC512_INLINE __m512i _mm512_load_si512(const void *p)  { __m512i r; memcpy(&r, p, 64); return r; }
SC512_INLINE __m512i _mm512_loadu_si512(const void *p) { __m512i r; memcpy(&r, p, 64); return r; }
SC512_INLINE void _mm512_store_si512(void *p, __m512i a)  { memcpy(p, &a, 64); }
SC512_INLINE void _mm512_storeu_si512(void *p, __m512i a) { memcpy(p, &a, 64); }
SC512_INLINE __m256i _mm256_load_si256(const __m256i *p)  { __m256i r; memcpy(&r, p, 32); return r; }
SC512_INLINE __m256i _mm256_loadu_si256(const __m256i *p) { __m256i r; memcpy(&r, p, 32); return r; }
SC512_INLINE __m128i _mm_load_si128(const __m128i *p) { __m128i r; memcpy(&r, p, 16); return r; }
SC512_INLINE __m128i _mm_loadl_epi64(const __m128i *p)
{ __m128i r; memcpy(&r, p, 8); r.u64[1] = 0; return r; }

/* ---- set / setzero ---- */
SC512_INLINE __m512i _mm512_setzero_si512(void) { __m512i r; memset(&r, 0, 64); return r; }
SC512_INLINE __m512i _mm512_set1_epi8(char a)
{ __m512i r; for (int i = 0; i < 64; i++) r.i8[i] = (int8_t)a; return r; }
SC512_INLINE __m512i _mm512_set1_epi16(short a)
{ __m512i r; for (int i = 0; i < 32; i++) r.i16[i] = a; return r; }
SC512_INLINE __m512i _mm512_set1_epi32(int a)
{ __m512i r; for (int i = 0; i < 16; i++) r.i32[i] = a; return r; }
SC512_INLINE __m512i _mm512_set1_epi64(long long a)
{ __m512i r; for (int i = 0; i < 8; i++) r.i64[i] = a; return r; }

SC512_INLINE __m512i _mm512_set_epi16(
    short e31, short e30, short e29, short e28, short e27, short e26, short e25, short e24,
    short e23, short e22, short e21, short e20, short e19, short e18, short e17, short e16,
    short e15, short e14, short e13, short e12, short e11, short e10, short e9,  short e8,
    short e7,  short e6,  short e5,  short e4,  short e3,  short e2,  short e1,  short e0)
{
  __m512i r;
  const short v[32] = { e0,e1,e2,e3,e4,e5,e6,e7,e8,e9,e10,e11,e12,e13,e14,e15,
                        e16,e17,e18,e19,e20,e21,e22,e23,e24,e25,e26,e27,e28,e29,e30,e31 };
  for (int i = 0; i < 32; i++) r.i16[i] = v[i];
  return r;
}

SC512_INLINE __m512i _mm512_set_epi8(
    char e63, char e62, char e61, char e60, char e59, char e58, char e57, char e56,
    char e55, char e54, char e53, char e52, char e51, char e50, char e49, char e48,
    char e47, char e46, char e45, char e44, char e43, char e42, char e41, char e40,
    char e39, char e38, char e37, char e36, char e35, char e34, char e33, char e32,
    char e31, char e30, char e29, char e28, char e27, char e26, char e25, char e24,
    char e23, char e22, char e21, char e20, char e19, char e18, char e17, char e16,
    char e15, char e14, char e13, char e12, char e11, char e10, char e9,  char e8,
    char e7,  char e6,  char e5,  char e4,  char e3,  char e2,  char e1,  char e0)
{
  __m512i r;
  const char v[64] = { e0,e1,e2,e3,e4,e5,e6,e7,e8,e9,e10,e11,e12,e13,e14,e15,
                       e16,e17,e18,e19,e20,e21,e22,e23,e24,e25,e26,e27,e28,e29,e30,e31,
                       e32,e33,e34,e35,e36,e37,e38,e39,e40,e41,e42,e43,e44,e45,e46,e47,
                       e48,e49,e50,e51,e52,e53,e54,e55,e56,e57,e58,e59,e60,e61,e62,e63 };
  for (int i = 0; i < 64; i++) r.i8[i] = (int8_t)v[i];
  return r;
}

SC512_INLINE __m512i _mm512_set_epi64(
    long long e7, long long e6, long long e5, long long e4,
    long long e3, long long e2, long long e1, long long e0)
{
  __m512i r;
  r.i64[0] = e0; r.i64[1] = e1; r.i64[2] = e2; r.i64[3] = e3;
  r.i64[4] = e4; r.i64[5] = e5; r.i64[6] = e6; r.i64[7] = e7;
  return r;
}

SC512_INLINE __m128i _mm_set_epi16(short e7, short e6, short e5, short e4,
                                   short e3, short e2, short e1, short e0)
{
  __m128i r;
  r.i16[0] = e0; r.i16[1] = e1; r.i16[2] = e2; r.i16[3] = e3;
  r.i16[4] = e4; r.i16[5] = e5; r.i16[6] = e6; r.i16[7] = e7;
  return r;
}

/* ---- casts / width changes ---- */
SC512_INLINE __m512i _mm512_castsi128_si512(__m128i a)
{ __m512i r; memset(&r, 0, 64); memcpy(&r, &a, 16); return r; }
SC512_INLINE __m512i _mm512_castsi256_si512(__m256i a)
{ __m512i r; memset(&r, 0, 64); memcpy(&r, &a, 32); return r; }
SC512_INLINE __m256i _mm512_castsi512_si256(__m512i a)
{ __m256i r; memcpy(&r, &a, 32); return r; }
SC512_INLINE __m128i _mm256_castsi256_si128(__m256i a)
{ __m128i r; memcpy(&r, &a, 16); return r; }
SC512_INLINE __m512 _mm512_castsi512_ps(__m512i a) { return a; }
SC512_INLINE __m512i _mm512_castps_si512(__m512 a) { return a; }

SC512_INLINE __m512i _mm512_broadcast_i64x2(__m128i a)
{ __m512i r; for (int l = 0; l < 4; l++) memcpy(&r.u8[16 * l], &a, 16); return r; }
SC512_INLINE __m256i _mm512_extracti64x4_epi64(__m512i a, int imm)
{ __m256i r; memcpy(&r, &a.u8[32 * (imm & 1)], 32); return r; }
SC512_INLINE __m128i _mm256_extracti64x2_epi64(__m256i a, int imm)
{ __m128i r; memcpy(&r, &a.u8[16 * (imm & 1)], 16); return r; }
SC512_INLINE long long _mm_cvtsi128_si64(__m128i a) { return a.i64[0]; }
SC512_INLINE long long _mm_extract_epi64(__m128i a, int imm) { return a.i64[imm & 1]; }

SC512_INLINE __m512i _mm512_cvtepi16_epi32(__m256i a)
{ __m512i r; for (int i = 0; i < 16; i++) r.i32[i] = a.i16[i]; return r; }
SC512_INLINE __m512i _mm512_cvtepu8_epi32(__m128i a)
{ __m512i r; for (int i = 0; i < 16; i++) { r.i32[i] = a.u8[i]; } return r; }
SC512_INLINE __m512i _mm512_cvtepu8_epi64(__m128i a)
{ __m512i r; for (int i = 0; i < 8; i++) { r.i64[i] = a.u8[i]; } return r; }
SC512_INLINE __m512i _mm512_cvtepu32_epi64(__m256i a)
{ __m512i r; for (int i = 0; i < 8; i++) { r.i64[i] = a.u32[i]; } return r; }
SC512_INLINE __m512i _mm512_cvtepi32_epi64(__m256i a)
{ __m512i r; for (int i = 0; i < 8; i++) { r.i64[i] = a.i32[i]; } return r; }

/* ---- logic ---- */
SC512_INLINE __m512i _mm512_and_si512(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 8; i++) r.u64[i] = a.u64[i] & b.u64[i]; return r; }
SC512_INLINE __m512i _mm512_or_si512(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 8; i++) r.u64[i] = a.u64[i] | b.u64[i]; return r; }

/* ---- 8-bit arithmetic ---- */
SC512_INLINE __m512i _mm512_add_epi8(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 64; i++) r.u8[i] = (uint8_t)(a.u8[i] + b.u8[i]); return r; }

/* ---- 16-bit arithmetic ---- */
SC512_INLINE __m512i _mm512_add_epi16(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = (uint16_t)(a.u16[i] + b.u16[i]); return r; }
SC512_INLINE __m512i _mm512_sub_epi16(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = (uint16_t)(a.u16[i] - b.u16[i]); return r; }
SC512_INLINE __m512i _mm512_mullo_epi16(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = (uint16_t)((uint32_t)a.u16[i] * b.u16[i]); return r; }
SC512_INLINE __m512i _mm512_mulhi_epi16(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++) r.i16[i] = (int16_t)(((int32_t)a.i16[i] * b.i16[i]) >> 16); return r; }
SC512_INLINE __m512i _mm512_mulhi_epu16(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = (uint16_t)(((uint32_t)a.u16[i] * b.u16[i]) >> 16); return r; }
SC512_INLINE __m512i _mm512_mulhrs_epi16(__m512i a, __m512i b)
{
  __m512i r;
  for (int i = 0; i < 32; i++) {
    int32_t t = ((int32_t)a.i16[i] * b.i16[i]) >> 14;
    r.i16[i] = (int16_t)((t + 1) >> 1);
  }
  return r;
}

SC512_INLINE __m512i _mm512_slli_epi16(__m512i a, unsigned imm)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = imm > 15 ? 0 : (uint16_t)(a.u16[i] << imm); return r; }
SC512_INLINE __m512i _mm512_srli_epi16(__m512i a, unsigned imm)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = imm > 15 ? 0 : (uint16_t)(a.u16[i] >> imm); return r; }
SC512_INLINE __m512i _mm512_srai_epi16(__m512i a, unsigned imm)
{ __m512i r; unsigned s = imm > 15 ? 15 : imm;
  for (int i = 0; i < 32; i++) r.i16[i] = (int16_t)(a.i16[i] >> s); return r; }
SC512_INLINE __m512i _mm512_sllv_epi16(__m512i a, __m512i cnt)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = cnt.u16[i] > 15 ? 0 : (uint16_t)(a.u16[i] << cnt.u16[i]); return r; }
SC512_INLINE __m512i _mm512_srlv_epi16(__m512i a, __m512i cnt)
{ __m512i r; for (int i = 0; i < 32; i++) r.u16[i] = cnt.u16[i] > 15 ? 0 : (uint16_t)(a.u16[i] >> cnt.u16[i]); return r; }

SC512_INLINE __m512i _mm512_mask_mov_epi16(__m512i src, __mmask32 k, __m512i a)
{ __m512i r; for (int i = 0; i < 32; i++) r.i16[i] = (k >> i) & 1 ? a.i16[i] : src.i16[i]; return r; }
SC512_INLINE __m512i _mm512_maskz_mov_epi16(__mmask32 k, __m512i a)
{ __m512i r; for (int i = 0; i < 32; i++) r.i16[i] = (k >> i) & 1 ? a.i16[i] : 0; return r; }
SC512_INLINE __m512i _mm512_mask_add_epi16(__m512i src, __mmask32 k, __m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++)
    r.u16[i] = (k >> i) & 1 ? (uint16_t)(a.u16[i] + b.u16[i]) : src.u16[i]; return r; }
SC512_INLINE __m512i _mm512_mask_sub_epi16(__m512i src, __mmask32 k, __m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++)
    r.u16[i] = (k >> i) & 1 ? (uint16_t)(a.u16[i] - b.u16[i]) : src.u16[i]; return r; }
SC512_INLINE __m512i _mm512_maskz_sub_epi16(__mmask32 k, __m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++)
    r.u16[i] = (k >> i) & 1 ? (uint16_t)(a.u16[i] - b.u16[i]) : 0; return r; }
SC512_INLINE __m512i _mm512_mask_blend_epi16(__mmask32 k, __m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 32; i++) r.i16[i] = (k >> i) & 1 ? b.i16[i] : a.i16[i]; return r; }
SC512_INLINE __m512i _mm512_mask_blend_epi32(__mmask16 k, __m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 16; i++) r.i32[i] = (k >> i) & 1 ? b.i32[i] : a.i32[i]; return r; }

SC512_INLINE __mmask32 _mm512_movepi16_mask(__m512i a)
{ __mmask32 k = 0; for (int i = 0; i < 32; i++) k |= (uint32_t)((a.u16[i] >> 15) & 1) << i; return k; }

/* predicate: 0 EQ, 1 LT, 2 LE, 3 FALSE, 4 NE, 5 GE, 6 GT, 7 TRUE */
SC512_INLINE __mmask32 _mm512_cmp_epi16_mask(__m512i a, __m512i b, int imm)
{
  __mmask32 k = 0;
  for (int i = 0; i < 32; i++) {
    int c;
    switch (imm & 7) {
    case 0: c = a.i16[i] == b.i16[i]; break;
    case 1: c = a.i16[i] <  b.i16[i]; break;
    case 2: c = a.i16[i] <= b.i16[i]; break;
    case 3: c = 0; break;
    case 4: c = a.i16[i] != b.i16[i]; break;
    case 5: c = a.i16[i] >= b.i16[i]; break;
    case 6: c = a.i16[i] >  b.i16[i]; break;
    default: c = 1; break;
    }
    k |= (uint32_t)c << i;
  }
  return k;
}

SC512_INLINE void _mm512_mask_compressstoreu_epi16(void *p, __mmask32 k, __m512i a)
{
  uint8_t *dst = (uint8_t *)p;
  for (int i = 0; i < 32; i++)
    if ((k >> i) & 1) { memcpy(dst, &a.i16[i], 2); dst += 2; }
}
SC512_INLINE void _mm512_mask_storeu_epi16(void *p, __mmask32 k, __m512i a)
{
  uint8_t *dst = (uint8_t *)p;
  for (int i = 0; i < 32; i++)
    if ((k >> i) & 1) memcpy(dst + 2 * i, &a.i16[i], 2);
}

/* ---- 32-bit arithmetic ---- */
SC512_INLINE __m512i _mm512_add_epi32(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 16; i++) r.u32[i] = a.u32[i] + b.u32[i]; return r; }
SC512_INLINE __m512i _mm512_sub_epi32(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 16; i++) r.u32[i] = a.u32[i] - b.u32[i]; return r; }
SC512_INLINE __m512i _mm512_slli_epi32(__m512i a, unsigned imm)
{ __m512i r; for (int i = 0; i < 16; i++) r.u32[i] = imm > 31 ? 0 : a.u32[i] << imm; return r; }
SC512_INLINE __m512i _mm512_srli_epi32(__m512i a, unsigned imm)
{ __m512i r; for (int i = 0; i < 16; i++) r.u32[i] = imm > 31 ? 0 : a.u32[i] >> imm; return r; }
SC512_INLINE __m512i _mm512_mul_epi32(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 8; i++) r.i64[i] = (int64_t)a.i32[2 * i] * b.i32[2 * i]; return r; }
SC512_INLINE __m512i _mm512_dpwssd_epi32(__m512i src, __m512i a, __m512i b)
{
  __m512i r;
  for (int i = 0; i < 16; i++) {
    int32_t p0 = (int32_t)a.i16[2 * i] * b.i16[2 * i];
    int32_t p1 = (int32_t)a.i16[2 * i + 1] * b.i16[2 * i + 1];
    r.u32[i] = src.u32[i] + (uint32_t)p0 + (uint32_t)p1;
  }
  return r;
}

/* ---- 64-bit arithmetic ---- */
SC512_INLINE __m512i _mm512_add_epi64(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 8; i++) r.u64[i] = a.u64[i] + b.u64[i]; return r; }
SC512_INLINE __m512i _mm512_sub_epi64(__m512i a, __m512i b)
{ __m512i r; for (int i = 0; i < 8; i++) r.u64[i] = a.u64[i] - b.u64[i]; return r; }
SC512_INLINE __m512i _mm512_slli_epi64(__m512i a, unsigned imm)
{ __m512i r; for (int i = 0; i < 8; i++) r.u64[i] = imm > 63 ? 0 : a.u64[i] << imm; return r; }
SC512_INLINE __m512i _mm512_srli_epi64(__m512i a, unsigned imm)
{ __m512i r; for (int i = 0; i < 8; i++) r.u64[i] = imm > 63 ? 0 : a.u64[i] >> imm; return r; }
SC512_INLINE __m512i _mm512_srai_epi64(__m512i a, unsigned imm)
{ __m512i r; unsigned s = imm > 63 ? 63 : imm;
  for (int i = 0; i < 8; i++) r.i64[i] = a.i64[i] >> s; return r; }
SC512_INLINE __m128i _mm_add_epi64(__m128i a, __m128i b)
{ __m128i r; for (int i = 0; i < 2; i++) r.u64[i] = a.u64[i] + b.u64[i]; return r; }
SC512_INLINE __m256i _mm256_add_epi64(__m256i a, __m256i b)
{ __m256i r; for (int i = 0; i < 4; i++) r.u64[i] = a.u64[i] + b.u64[i]; return r; }
SC512_INLINE __m128i _mm_unpackhi_epi64(__m128i a, __m128i b)
{ __m128i r; r.i64[0] = a.i64[1]; r.i64[1] = b.i64[1]; return r; }

/* ---- shuffles / permutes ---- */
SC512_INLINE __m512i _mm512_shuffle_i64x2(__m512i a, __m512i b, int imm)
{
  __m512i r;
  for (int l = 0; l < 4; l++) {
    const __m512i *src = l < 2 ? &a : &b;
    int sel = (imm >> (2 * l)) & 3;
    r.i64[2 * l]     = src->i64[2 * sel];
    r.i64[2 * l + 1] = src->i64[2 * sel + 1];
  }
  return r;
}
SC512_INLINE __m512i _mm512_unpacklo_epi64(__m512i a, __m512i b)
{ __m512i r; for (int l = 0; l < 4; l++) { r.i64[2 * l] = a.i64[2 * l]; r.i64[2 * l + 1] = b.i64[2 * l]; } return r; }
SC512_INLINE __m512i _mm512_unpackhi_epi64(__m512i a, __m512i b)
{ __m512i r; for (int l = 0; l < 4; l++) { r.i64[2 * l] = a.i64[2 * l + 1]; r.i64[2 * l + 1] = b.i64[2 * l + 1]; } return r; }
SC512_INLINE __m512i _mm512_unpacklo_epi32(__m512i a, __m512i b)
{
  __m512i r;
  for (int l = 0; l < 4; l++) {
    r.i32[4 * l]     = a.i32[4 * l];     r.i32[4 * l + 1] = b.i32[4 * l];
    r.i32[4 * l + 2] = a.i32[4 * l + 1]; r.i32[4 * l + 3] = b.i32[4 * l + 1];
  }
  return r;
}
SC512_INLINE __m512i _mm512_unpackhi_epi32(__m512i a, __m512i b)
{
  __m512i r;
  for (int l = 0; l < 4; l++) {
    r.i32[4 * l]     = a.i32[4 * l + 2]; r.i32[4 * l + 1] = b.i32[4 * l + 2];
    r.i32[4 * l + 2] = a.i32[4 * l + 3]; r.i32[4 * l + 3] = b.i32[4 * l + 3];
  }
  return r;
}

SC512_INLINE __m512i _mm512_permutexvar_epi8(__m512i idx, __m512i a)
{ __m512i r; for (int i = 0; i < 64; i++) r.i8[i] = a.i8[idx.u8[i] & 63]; return r; }
SC512_INLINE __m512i _mm512_maskz_permutexvar_epi8(__mmask64 k, __m512i idx, __m512i a)
{ __m512i r; for (int i = 0; i < 64; i++) r.i8[i] = (k >> i) & 1 ? a.i8[idx.u8[i] & 63] : 0; return r; }
SC512_INLINE __m512i _mm512_permutex2var_epi8(__m512i a, __m512i idx, __m512i b)
{
  __m512i r;
  for (int i = 0; i < 64; i++) {
    int off = idx.u8[i] & 63;
    r.i8[i] = (idx.u8[i] & 64) ? b.i8[off] : a.i8[off];
  }
  return r;
}
SC512_INLINE __m512i _mm512_permutexvar_epi16(__m512i idx, __m512i a)
{ __m512i r; for (int i = 0; i < 32; i++) r.i16[i] = a.i16[idx.u16[i] & 31]; return r; }
SC512_INLINE __m512i _mm512_permutexvar_epi32(__m512i idx, __m512i a)
{ __m512i r; for (int i = 0; i < 16; i++) r.i32[i] = a.i32[idx.u32[i] & 15]; return r; }
SC512_INLINE __m512i _mm512_permutexvar_epi64(__m512i idx, __m512i a)
{ __m512i r; for (int i = 0; i < 8; i++) r.i64[i] = a.i64[idx.u64[i] & 7]; return r; }
SC512_INLINE __m512i _mm512_permutex2var_epi64(__m512i a, __m512i idx, __m512i b)
{
  __m512i r;
  for (int i = 0; i < 8; i++) {
    int off = idx.u64[i] & 7;
    r.i64[i] = (idx.u64[i] & 8) ? b.i64[off] : a.i64[off];
  }
  return r;
}

SC512_INLINE __m512i _mm512_alignr_epi8(__m512i a, __m512i b, int imm)
{
  __m512i r;
  for (int l = 0; l < 4; l++)
    for (int i = 0; i < 16; i++) {
      int s = i + (imm & 31);
      if (s < 16)       r.i8[16 * l + i] = b.i8[16 * l + s];
      else if (s < 32)  r.i8[16 * l + i] = a.i8[16 * l + s - 16];
      else              r.i8[16 * l + i] = 0;
    }
  return r;
}

/* 32-bit even-element duplicate, via the ps cast idiom used in ntt.c/polz.c */
SC512_INLINE __m512 _mm512_moveldup_ps(__m512 a)
{
  __m512 r;
  for (int i = 0; i < 8; i++) { r.i32[2 * i] = a.i32[2 * i]; r.i32[2 * i + 1] = a.i32[2 * i]; }
  return r;
}

#endif /* SCALAR512_IMMINTRIN_H */
