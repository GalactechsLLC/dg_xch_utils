/* SPDX-License-Identifier: Apache-2.0
 * Authorized for public redistribution; see native/LICENSE-overlay and
 * the crate's LICENSE for the applicable license terms.
 */
/* Portable scalar replacement for labrador's aesctr.c (AVX-512/VAES).
 *
 * Semantics replicated from the original (Dolbeau-derived) code:
 *  - counter block: low qword = nonce (as stored, little-endian bytes),
 *    high qword = block counter, native little-endian, starting at 0;
 *    no byte-swapping anywhere (this is NOT NIST big-endian CTR).
 *  - squeezeblocks emits 32 consecutive AES blocks (512 B) per unit and
 *    advances the counter by 32.
 *  - select(nonce) installs a new nonce and resets the counter to 0
 *    (the original loads the nonce with a zeroed high qword).
 *  - init key expansion is the standard AES-128/256 schedule (the AES-NI
 *    aeskeygenassist macros in the original compute exactly this).
 *
 * Plain lookup-table AES: adequate for the spike; PRG seeds here may be
 * secret-derived in some call paths, so the phone port should use the ARMv8
 * crypto extension (AESE/AESMC) or a bitsliced core rather than S-box tables
 * if constant-time matters on the target. Flagged in the Phase 1.5 memo.
 */

#include "aesctr_portable.h"
#include <string.h>

static const uint8_t sbox[256] = {
  0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b,
  0xfe, 0xd7, 0xab, 0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0,
  0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26,
  0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
  0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2,
  0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0,
  0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed,
  0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
  0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f,
  0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5,
  0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec,
  0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
  0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14,
  0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c,
  0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d,
  0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
  0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f,
  0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e,
  0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1, 0xf8, 0x98, 0x11,
  0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
  0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f,
  0xb0, 0x54, 0xbb, 0x16
};

static inline uint8_t
xtime (uint8_t x)
{
  return (uint8_t)((x << 1) ^ ((x >> 7) * 0x1b));
}

/* standard Rijndael key expansion; nk = 4 (AES-128) or 8 (AES-256) */
static void
expand_key (uint8_t *rk, const uint8_t *key, unsigned int nk)
{
  const unsigned int nr = nk + 6, nw = 4 * (nr + 1);
  uint8_t rcon = 1;
  unsigned int i;

  memcpy (rk, key, 4 * nk);
  for (i = nk; i < nw; i++)
    {
      uint8_t t[4];
      memcpy (t, rk + 4 * (i - 1), 4);
      if (i % nk == 0)
        {
          uint8_t tmp = t[0];
          t[0] = (uint8_t)(sbox[t[1]] ^ rcon);
          t[1] = sbox[t[2]];
          t[2] = sbox[t[3]];
          t[3] = sbox[tmp];
          rcon = xtime (rcon);
        }
      else if (nk == 8 && i % nk == 4)
        {
          t[0] = sbox[t[0]];
          t[1] = sbox[t[1]];
          t[2] = sbox[t[2]];
          t[3] = sbox[t[3]];
        }
      rk[4 * i + 0] = (uint8_t)(rk[4 * (i - nk) + 0] ^ t[0]);
      rk[4 * i + 1] = (uint8_t)(rk[4 * (i - nk) + 1] ^ t[1]);
      rk[4 * i + 2] = (uint8_t)(rk[4 * (i - nk) + 2] ^ t[2]);
      rk[4 * i + 3] = (uint8_t)(rk[4 * (i - nk) + 3] ^ t[3]);
    }
}

/* one 16-byte block, in/out in memory order (byte i = row i%4, col i/4) */
static void
aes_encrypt_block (uint8_t out[16], const uint8_t in[16],
                   const uint8_t (*rk)[16], unsigned int rounds)
{
  uint8_t s[16], t[16];
  unsigned int r, c, i;

  for (i = 0; i < 16; i++)
    s[i] = (uint8_t)(in[i] ^ rk[0][i]);

  for (r = 1; r <= rounds; r++)
    {
      /* SubBytes + ShiftRows: row j rotates left by j; byte (row j, col c)
       * lives at index 4c + j, so t[4c+j] = sbox[s[4((c+j)%4) + j]] */
      for (c = 0; c < 4; c++)
        {
          t[4 * c + 0] = sbox[s[4 * c + 0]];
          t[4 * c + 1] = sbox[s[(4 * (c + 1) + 1) % 16]];
          t[4 * c + 2] = sbox[s[(4 * (c + 2) + 2) % 16]];
          t[4 * c + 3] = sbox[s[(4 * (c + 3) + 3) % 16]];
        }
      if (r < rounds)
        {
          /* MixColumns */
          for (c = 0; c < 4; c++)
            {
              uint8_t a0 = t[4 * c], a1 = t[4 * c + 1], a2 = t[4 * c + 2],
                      a3 = t[4 * c + 3];
              uint8_t x = (uint8_t)(a0 ^ a1 ^ a2 ^ a3);
              s[4 * c + 0] = (uint8_t)(a0 ^ x ^ xtime ((uint8_t)(a0 ^ a1)));
              s[4 * c + 1] = (uint8_t)(a1 ^ x ^ xtime ((uint8_t)(a1 ^ a2)));
              s[4 * c + 2] = (uint8_t)(a2 ^ x ^ xtime ((uint8_t)(a2 ^ a3)));
              s[4 * c + 3] = (uint8_t)(a3 ^ x ^ xtime ((uint8_t)(a3 ^ a0)));
            }
        }
      else
        memcpy (s, t, 16);
      for (i = 0; i < 16; i++)
        s[i] ^= rk[r][i];
    }
  memcpy (out, s, 16);
}

static inline void
store_le64 (uint8_t *p, uint64_t x)
{
  unsigned int i;
  for (i = 0; i < 8; i++)
    p[i] = (uint8_t)(x >> (8 * i));
}

static void
ctr_squeeze (uint8_t *out, size_t nblocks, const uint8_t (*rk)[16],
             unsigned int rounds, uint64_t nonce, uint64_t *ctr)
{
  uint8_t blk[16];
  size_t i, b;

  store_le64 (blk, nonce);
  for (i = 0; i < nblocks; i++)
    {
      for (b = 0; b < 32; b++)
        {
          store_le64 (blk + 8, *ctr + b);
          aes_encrypt_block (out, blk, rk, rounds);
          out += 16;
        }
      *ctr += 32;
    }
}

void
aes128ctr_init (aes128ctr_ctx *state, const uint8_t key[16], uint64_t nonce)
{
  expand_key (&state->rkeys[0][0], key, 4);
  state->nonce = nonce;
  state->ctr = 0;
}

void
aes256ctr_init (aes256ctr_ctx *state, const uint8_t key[32], uint64_t nonce)
{
  expand_key (&state->rkeys[0][0], key, 8);
  state->nonce = nonce;
  state->ctr = 0;
}

void
aes128ctr_select (aes128ctr_ctx *state, uint64_t nonce)
{
  state->nonce = nonce;
  state->ctr = 0;
}

void
aes256ctr_select (aes256ctr_ctx *state, uint64_t nonce)
{
  state->nonce = nonce;
  state->ctr = 0;
}

void
aes128ctr_squeezeblocks (uint8_t *out, size_t nblocks, aes128ctr_ctx *state)
{
  ctr_squeeze (out, nblocks, state->rkeys, 10, state->nonce, &state->ctr);
}

void
aes256ctr_squeezeblocks (uint8_t *out, size_t nblocks, aes256ctr_ctx *state)
{
  ctr_squeeze (out, nblocks, state->rkeys, 14, state->nonce, &state->ctr);
}
