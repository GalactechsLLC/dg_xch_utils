/* SPDX-License-Identifier: Apache-2.0
 * Authorized for public redistribution; see native/LICENSE-overlay and
 * the crate's LICENSE for the applicable license terms.
 */
/* Phase 2 — FAP1 wire codec for dch_pack_proof (overlay artifact; upstream
 * untouched).  Spec sketch: memos/2026-08-26_wire_format_sketch.md, REVISED
 * by measurement (see below).
 *
 * Wire = 12-byte header + coefficient payloads.  Every length is derived
 * from dch_pack_params (which the verifier derives from the statement); the
 * header's np/paylen are redundant cross-checks against params-generator
 * version skew.
 *
 * MEASURED REVISION of the sketch's canonical-[0,q) rule (wire_diag on real
 * dch_pack_prove outputs, 2026-08-26): upstream's Fiat-Shamir hash chain
 * (update_hash_polz) covers the raw 14-bit limb representation, and real
 * prover messages are NOT canonical residues —
 *   - lift coefficients equal to exactly q (0 mod q stored as q),
 *   - tail-round and LNP-z blocks centered (negative values, |v| small),
 *   - polz_reduce output lives in [0, 2^LOGQ), not [0, q).
 * All observed coefficients lie in the image of polz_setcoeff_fromint64
 * (low limbs = nonnegative base-2^14 digits, top limb signed), which is
 * bijective with the signed value v.  FAP1 therefore serializes v itself:
 * fixed FAP1_CBYTES little-endian two's complement, domain |v| < 2^LOGQ.
 * Decode rejects anything outside the domain; encode rejects any in-memory
 * representation outside the setcoeff_fromint64 image (digit overflow) or
 * outside the domain — a tripwire that keeps representation drift measurable
 * instead of silent.
 *
 * Canonicity: byte string <-> limb representation is a bijection, so there
 * is exactly one byte string per proof OBJECT.  Two representations of the
 * same residue (v and v-q) are distinct byte strings, but at most one
 * verifies: the FS hash chain binds the prover's actual representation, so
 * flipping representation breaks verification like any other bit flip.
 * For the Rust port: normalize to canonical [0,q) BEFORE hashing, which
 * makes the wire residue-canonical and shrinks the domain check (recorded
 * as a port requirement).
 */
#ifndef FAP1_WIRE_H
#define FAP1_WIRE_H

#include <stdint.h>
#include <stddef.h>
#include "dachshund.h"

/* exported (visibility default) so the lazer-python liblabradorXX.so build
 * (-fvisibility=hidden) exposes the codec to cffi, like labrados_python.h */
#define FAP1_EXPORT __attribute__((visibility("default")))

#define FAP1_HDR_BYTES 12
/* signed coefficient, |v| < 2^LOGQ -> LOGQ+1 bits, byte-rounded (5 for all
 * three suites) */
#define FAP1_CBYTES ((LOGQ + 1 + 7) / 8)

#define FAP1_OK             0
#define FAP1_ERR_MAGIC     (-1)  /* magic != "FAP1" (fap2: != "FAP2") */
#define FAP1_ERR_SUITE     (-2)  /* suite byte != LOGQ of this build */
#define FAP1_ERR_FLAGS     (-3)  /* zk bit != params, or reserved flag bits set */
#define FAP1_ERR_NP        (-4)  /* header np != params-derived round count */
#define FAP1_ERR_RESERVED  (-5)  /* reserved header byte nonzero */
#define FAP1_ERR_PAYLEN    (-6)  /* header paylen != params-derived payload size */
#define FAP1_ERR_TRUNC     (-7)  /* input length != 12 + paylen */
#define FAP1_ERR_NONCANON  (-8)  /* decode: coefficient outside |v| < 2^LOGQ */
#define FAP1_ERR_ENC_REP   (-9)  /* encode: representation outside the
                                    setcoeff_fromint64 image or the domain */
#define FAP1_ERR_BUF      (-10)  /* encode: output buffer too small */
/* fap2-only decode errors */
#define FAP2_ERR_WIDTH    (-11)  /* segment descriptor invalid: width over the
                                    per-kind cap, or signed flag with w == 0 */
#define FAP2_ERR_NONMIN   (-12)  /* segment not canonically packed: width not
                                    minimal, or signed flag with no negative
                                    coefficient present */

/* Params-derived payload size in bytes (excluding the 12-byte header). */
#define fap1_payload_size NAMESPACE(fap1_payload_size)
FAP1_EXPORT
size_t fap1_payload_size(const dch_pack_params pp);

/* 12 + fap1_payload_size(). */
#define fap1_wire_size NAMESPACE(fap1_wire_size)
FAP1_EXPORT
size_t fap1_wire_size(const dch_pack_params pp);

/* Allocate a dch_pack_proof with the exact shapes the upstream prover
 * produces (so dch_pack_proof_free works and pack_verify sees the shape it
 * expects), zero-filled.  Used by decode and by the synthetic-fill KATs. */
#define fap1_proof_alloc NAMESPACE(fap1_proof_alloc)
FAP1_EXPORT
void fap1_proof_alloc(dch_pack_proof pi, const dch_pack_params pp);

/* Serialize pi into out (capacity outcap).  On success returns FAP1_OK and
 * sets *outlen = fap1_wire_size(pp).  Never allocates. */
#define fap1_encode NAMESPACE(fap1_encode)
FAP1_EXPORT
int fap1_encode(uint8_t *out, size_t outcap, size_t *outlen,
                const dch_pack_proof pi, const dch_pack_params pp);

/* Parse in[0..inlen) into a freshly allocated pi.  On success returns
 * FAP1_OK and pi must later be freed with dch_pack_proof_free.  On any
 * error returns the (negative) error code and pi owns no memory. */
#define fap1_decode NAMESPACE(fap1_decode)
FAP1_EXPORT
int fap1_decode(dch_pack_proof pi, const uint8_t *in, size_t inlen,
                const dch_pack_params pp);

/* ================= FAP2: per-segment adaptive bitpacking =================
 *
 * v2 of the wire format (Ken 2026-08-27: full bitpack).  Same 12-byte
 * header as FAP1 except magic "FAP2" and paylen = the ACTUAL payload byte
 * length (v2 payloads are value-dependent; np stays the params cross-check,
 * and the decoder bounds paylen by the params-derived maximum).
 *
 * The payload is a sequence of SEGMENTS in the same struct order as FAP1,
 * but split at the params-derived message boundaries inside each round's
 * contiguous block (lab_proof_init / lnp_proof_init layout):
 *   com | per round { compressed: u(4*kappa2) ;
 *                     !tail: U1(kappa1) lift(LIFTS) U2(kappa1) ;
 *                     tail:  t(INCOM) g(QUADG) lift(LIFT) h(LING) }
 *         (+ p[256] as one i32 segment iff !compressed)
 *       | zk: U1 U2 z U4 U5 | owt: one segment per witness vector.
 *
 * Each segment = 1 descriptor byte (bit7 = signed flag, bits 0..6 = width
 * w) followed by its coefficients bitpacked LSB-first at w bits each.
 * w == 0 means every coefficient is zero (no payload bytes).  Every
 * segment's coefficient count is a multiple of 8 (all counts are multiples
 * of N=256), so segments always end on a byte boundary — no padding
 * exists.  Values are the same representation-faithful signed values as
 * FAP1 (domain |v| < 2^LOGQ for polz; int16 for the clear witness; int32
 * for JL projections), unsigned w-bit when the segment has no negative
 * coefficient, two's-complement w-bit otherwise.
 *
 * Canonicity (decoder REJECTS violations, so one byte string per proof
 * object is preserved):
 *   - signed flag set iff the segment contains >= 1 negative coefficient
 *     (FAP2_ERR_NONMIN otherwise);
 *   - w minimal for the values under that signedness (FAP2_ERR_NONMIN);
 *   - w <= per-kind cap: polz LOGQ(+1 if signed), owt 15(+1), p 31(+1)
 *     (FAP2_ERR_WIDTH; signed flag with w == 0 is also FAP2_ERR_WIDTH);
 *   - polz domain |v| < 2^LOGQ still checked (FAP1_ERR_NONCANON — only
 *     reachable at w == LOGQ+1 signed, value -2^LOGQ);
 *   - total parse must consume exactly paylen and inlen == 12 + paylen
 *     (FAP1_ERR_TRUNC).
 * Note: the wire LENGTH now depends on coefficient magnitudes.  For zk
 * proofs every serialized message is simulatable by the protocol's own zk
 * property (commitments hiding, z rejection-sampled), so the length leaks
 * nothing beyond what the messages themselves already reveal.
 */

typedef struct {
  uint8_t kind;      /* FAP2_SEG_* */
  void *p;           /* polz* / poly* / int32_t* base (NULL if pi == NULL) */
  size_t nelem;      /* count of polz / poly / int32 elements */
} fap2_seg;

#define FAP2_SEG_POLZ 0
#define FAP2_SEG_POLY 1
#define FAP2_SEG_I32  2

/* Number of segments (params-derived). */
#define fap2_nsegs NAMESPACE(fap2_nsegs)
FAP1_EXPORT
size_t fap2_nsegs(const dch_pack_params pp);

/* Fill segs[0..fap2_nsegs(pp)) in wire order.  pi may be NULL, in which
 * case the p fields are NULL (kind/nelem are params-only). */
#define fap2_seglist NAMESPACE(fap2_seglist)
FAP1_EXPORT
void fap2_seglist(fap2_seg *segs, const dch_pack_proof pi,
                  const dch_pack_params pp);

/* Params-derived upper bound on the wire size (header included): every
 * segment at its per-kind maximum signed width.  Use for buffer sizing;
 * the actual size is fap2_encode's *outlen. */
#define fap2_wire_maxsize NAMESPACE(fap2_wire_maxsize)
FAP1_EXPORT
size_t fap2_wire_maxsize(const dch_pack_params pp);

/* Serialize pi into out (capacity outcap).  On success returns FAP1_OK and
 * sets *outlen to the actual wire size (<= fap2_wire_maxsize(pp)).  Never
 * allocates.  Same encoder-side representation tripwires as fap1_encode. */
#define fap2_encode NAMESPACE(fap2_encode)
FAP1_EXPORT
int fap2_encode(uint8_t *out, size_t outcap, size_t *outlen,
                const dch_pack_proof pi, const dch_pack_params pp);

/* Parse in[0..inlen) into a freshly allocated pi.  On success returns
 * FAP1_OK and pi must later be freed with dch_pack_proof_free.  On any
 * error returns the (negative) error code and pi owns no memory. */
#define fap2_decode NAMESPACE(fap2_decode)
FAP1_EXPORT
int fap2_decode(dch_pack_proof pi, const uint8_t *in, size_t inlen,
                const dch_pack_params pp);

#endif
