#include "aesctr.h"
#include "data.h"
#include "gaussian.h"
#include "malloc.h"

#include <math.h>
/* Modified by Galactechs LLC: restore the fdlibm attribution and permission
 * notice for _exp_small. This change does not alter the implementation.
 */
#include <stdint.h>
#include <string.h>

static const double INV_LN2 = 1.4426950408889634;
static const double LN2     = 0.6931471805599453;
static const double DSS_155 = 1.0 / (2.0 * 1.55 * 1.55);

typedef struct
{
  uint64_t hi, lo;
} z128;

static const z128 CDF155[]
    = { { 10894764499197476522ULL, 10804844707381617341ULL },
        { 4761708367981796450ULL, 6209732027000382074ULL },
        { 1476784279527800432ULL, 14108379346150813303ULL },
        { 316388870594767345ULL, 17827298407763885637ULL },
        { 46043503515468600ULL, 18385899657892021654ULL },
        { 4503729779335039ULL, 3860889375818664979ULL },
        { 294122444862326ULL, 13947176349836216550ULL },
        { 12769598070895ULL, 14321894682751135119ULL },
        { 367552986472ULL, 10286761328368440884ULL },
        { 7001273393ULL, 2287787188970898528ULL },
        { 88153536ULL, 17843977990435837663ULL },
        { 733119ULL, 12174894787802692461ULL },
        { 4024ULL, 18067426722645776197ULL },
        { 14ULL, 10764017821655913055ULL },
        { 0ULL, 643125733022530080ULL },
        { 0ULL, 1014291014134832ULL },
        { 0ULL, 1055215183460ULL },
        { 0ULL, 724109373ULL },
        { 0ULL, 327744ULL },
        { 0ULL, 98ULL },
        { 0ULL, 0ULL } };

// Buffered randomness: squeeze 512 bytes at a time
// (cdfsampler needs 16 bytes, ber_exp needs 16 bytes, sign bit needs 1 bit)
typedef struct {
  uint8_t buf[AES128CTR_BLOCKBYTES];
  size_t pos;    // byte position in buf
  uint64_t bits; // cached bits for single-bit extraction
  size_t bpos;   // bit position (0..63), 64 = empty
} randbuf;

// empty, refill on first use
static inline void randbuf_init(randbuf *rb) {
  rb->pos = AES128CTR_BLOCKBYTES;
  rb->bpos = 64;
}

static inline void randbuf_fill(randbuf *rb, aes128ctr_ctx *state) {
  aes128ctr_squeezeblocks(rb->buf, 1, state);
  rb->pos = 0;
}

static inline const uint8_t *randbuf_bytes(randbuf *rb, aes128ctr_ctx *state, size_t n) {
  if (rb->pos + n > AES128CTR_BLOCKBYTES)
    randbuf_fill(rb, state);
  const uint8_t *r = &rb->buf[rb->pos];
  rb->pos += n;
  return r;
}

static inline int randbuf_bit(randbuf *rb, aes128ctr_ctx *state) {
  if (rb->bpos >= 64) {
    const uint64_t *w = (const uint64_t *)randbuf_bytes(rb, state, 8);
    rb->bits = *w;
    rb->bpos = 0;
  }
  int b = rb->bits & 1;
  rb->bits >>= 1;
  rb->bpos++;
  return b;
}

/*
 * Compute exp(x) for x such that |x| <= 0.5*ln 2
 * FIXME: Recompute Remez coefficients for interval [-ln 2,0]
 *
 * The algorithm used below is derived from fdlibm's e_exp.c:
 * https://netlib.org/fdlibm/e_exp.c
 *
 * Copyright (C) 2004 by Sun Microsystems, Inc. All rights reserved.
 *
 * Permission to use, copy, modify, and distribute this
 * software is freely granted, provided that this notice
 * is preserved.
 *
 */
static inline double
_exp_small (double x)
{
#define C1 (1.66666666666666019037e-01)
#define C2 (-2.77777777770155933842e-03)
#define C3 (6.61375632143793436117e-05)
#define C4 (-1.65339022054652515390e-06)
#define C5 (4.13813679705723846039e-08)

  double t;

  t = x * x;
  t = x - t * (C1 + t * (C2 + t * (C3 + t * (C4 + t * C5)))); // R1
  t = 1.0 - ((x * t) / (t - 2.0) - x);
  return t;

#undef C1
#undef C2
#undef C3
#undef C4
#undef C5
}

static inline unsigned int
_cdfsampler (aes128ctr_ctx *state, randbuf *rb, const z128 CDF[])
{
  const uint64_t *w = (const uint64_t *)randbuf_bytes(rb, state, 16);
  const uint64_t hi = w[0], lo = w[1];
  unsigned int z;

  z = 0;
  while (hi <= CDF[z].hi && (hi < CDF[z].hi || lo < CDF[z].lo))
    ++z;

  return z;
}

static inline unsigned int
_ber_exp (aes128ctr_ctx *state, randbuf *rb, double x)
{
  unsigned int b;
  const uint64_t *w = (const uint64_t *)randbuf_bytes(rb, state, 16);
  uint64_t t, u;

  t = w[0];
  u = x * INV_LN2;
  x -= LN2 * u;
  u ^= (u ^ 63) & ((int64_t)(63 - u) >> 63); /* if(u > 63) u = 63; */
  t ^= (t >> u) << u;                        /* u random bits */
  b = 1 - ((t | -t) >> 63);                  /* 1 with probability 2^-u */

  t = w[1];
  t &= (1ULL << 53) - 1;
  u = _exp_small (-x) * (double)(1ULL << 53);
  b &= (t - u) >> 63; /* t < u with probability u/2^56 = e^-x */

  return b;
}

static inline int
gaussian155 (aes128ctr_ctx *state, randbuf *rb, double c)
{
  int k, b;
  double x;

  do
    {
      b = randbuf_bit(rb, state);
      k = _cdfsampler (state, rb, CDF155);
      k = (-b & (2 * k)) - k + b; // bimodal Gaussian
      x = ((k - c) * (k - c) - (k - b) * (k - b)) * DSS_155;
    }
  while (!_ber_exp (state, rb, x));

  return k;
}

void
gaussian_i32 (int32_t *ret, unsigned int nelems, aes128ctr_ctx *state,
                        unsigned int log2sd)
{
  unsigned int i;
  const unsigned int nbits = log2sd * nelems;
  const unsigned int nblocks = (nbits + AES128CTR_BLOCKBYTES * 8 - 1) / (AES128CTR_BLOCKBYTES * 8) + 1;
  uint8_t *out;
  uint32_t urand;
  int32_t k;
  double c;
  randbuf rb;
  const uint32_t mask = (log2sd >= 32) ? ~(uint32_t)0 : (((uint32_t)1 << log2sd) - 1);
  const double inv2pow = 1.0 / (double)((uint64_t)1 << log2sd);

  out = _malloc (nblocks * AES128CTR_BLOCKBYTES);

  aes128ctr_squeezeblocks(out, nblocks, state);

  randbuf_init(&rb);

  for (i = 0; i < nelems; i++)
    {
      const unsigned int boff = i * log2sd;
      uint64_t word;
      memcpy(&word, &out[boff >> 3], sizeof(word));
      urand = (uint32_t)((word >> (boff & 7)) & mask);

      c = (double)urand * inv2pow;
      k = gaussian155 (state, &rb, c);

      ret[i] = ((int32_t)k << log2sd) - urand;
    }

    free (out);
}
