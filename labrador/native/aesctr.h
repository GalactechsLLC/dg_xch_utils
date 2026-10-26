/* SPDX-License-Identifier: Apache-2.0
 * Authorized for public redistribution; see native/LICENSE-overlay and
 * the crate's LICENSE for the applicable license terms.
 */
/* Portable drop-in for labrador's aesctr.h (AVX-512/VAES CTR PRG).
 * Same API names and macro namespace; ctx holds byte-array round keys and an
 * explicit (nonce, counter) pair instead of __m128i.
 *
 * Keystream contract (matches the original exactly; see aesctr_portable.c):
 * 128-bit counter block = LE64(nonce) || LE64(counter); counter starts at 0,
 * increments by 1 per 16-byte block; one squeezeblocks "block" is 512 bytes
 * = 32 AES blocks; select() sets a new nonce AND resets the counter to 0.
 */
#ifndef AESCTR_H
#define AESCTR_H

#include <stddef.h>
#include <stdint.h>

#define AES128CTR_NAMESPACE(s) aes128ctr_portable_##s
#define AES256CTR_NAMESPACE(s) aes256ctr_portable_##s

#define AES128CTR_BLOCKBYTES 512
#define AES256CTR_BLOCKBYTES 512

typedef struct {
  uint8_t rkeys[11][16];
  uint64_t nonce;
  uint64_t ctr;
} aes128ctr_ctx;

typedef struct {
  uint8_t rkeys[15][16];
  uint64_t nonce;
  uint64_t ctr;
} aes256ctr_ctx;

#define aes128ctr_init AES128CTR_NAMESPACE(init)
void aes128ctr_init(aes128ctr_ctx *state, const uint8_t key[16], uint64_t nonce);

#define aes128ctr_select AES128CTR_NAMESPACE(select)
void aes128ctr_select(aes128ctr_ctx *state, uint64_t nonce);

#define aes128ctr_squeezeblocks AES128CTR_NAMESPACE(squeezeblocks)
void aes128ctr_squeezeblocks(uint8_t *out, size_t nblocks, aes128ctr_ctx *state);

#define aes256ctr_init AES256CTR_NAMESPACE(init)
void aes256ctr_init(aes256ctr_ctx *state, const uint8_t key[32], uint64_t nonce);

#define aes256ctr_select AES256CTR_NAMESPACE(select)
void aes256ctr_select(aes256ctr_ctx *state, uint64_t nonce);

#define aes256ctr_squeezeblocks AES256CTR_NAMESPACE(squeezeblocks)
void aes256ctr_squeezeblocks(uint8_t *out, size_t nblocks, aes256ctr_ctx *state);

#endif
