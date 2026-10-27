/* SPDX-License-Identifier: Apache-2.0
 * Authorized for public redistribution; see native/LICENSE-overlay and
 * the crate's LICENSE for the applicable license terms.
 */
/* Phase 2 — FAP1 wire codec implementation.  See wire.h (incl. the measured
 * revision from canonical-[0,q) to representation-faithful signed values)
 * and memos/2026-08-26_wire_format_sketch.md.  Overlay: compiled against
 * the pinned labrador tree (3f95485); no upstream file is modified.
 *
 * Serialization order (struct order, all lengths params-derived):
 *   [header 12 B]
 *   com        kappa_outer polz          iff nexact||nquad   (dch front round)
 *   round i    T_i polz  (+ p[256] i32   iff !compressed)    i = 0..np-1,
 *              where the slot i == zkround (zk only) is the upstream
 *              memset-zero placeholder and is skipped entirely
 *   zkp        (4*kappa_linfmsis + 256/N) polz               iff zk
 *   owt        sum(n[i]) poly, int16 coefficients            (clear witness)
 *
 * T_i per round mode mirrors lab_proof_init exactly:
 *   compressed:  4*kappa[2]
 *   !tail:       2*kappa[1] + LIFTS
 *   tail:        len[INCOM]+len[QUADG]+len[LIFT]+len[LING]
 * In all modes m[0] is one contiguous allocation holding every message, so
 * the block serializes in memory order == message order.
 *
 * owt shape mirrors the upstream POST-compile_bincnst witness (every BIN
 * vector in the relevant output statement gets its sigmam1 copy appended
 * and r bumped — see compile_bincnst in proofsystem.c):
 *   np == 0:               dch output shape + sigmam1 of each BIN vector
 *   zk && zkround==np-1:   lnp output shape + sigmam1(xbin)
 *                          (reachable only at the maxnp cap; kept anyway)
 *   otherwise:             lab_witness_init dims of p[np-1]; compressed
 *                          rounds additionally append sigmam1(bin)
 */
#include <string.h>
#include "wire.h"
#include "pack.h"
#include "labrador_core.h"
#include "lnp.h"
#include "malloc.h"

#define FAP1_VMAX ((int64_t)1 << LOGQ)

/* The empty placeholder slot the prover memsets when zk (pack.c). */
static int round_empty(const pack_params pk, size_t i){
  return pk->zkp != NULL && i == pk->zkround;
}

/* Total polz in round i's contiguous m[0] block (mirrors lab_proof_init). */
static size_t round_npolz(const lab_params lpp){
  if(lpp->compressed)
    return 4*lpp->kappa[2];
  if(!lpp->tail)
    return 2*lpp->kappa[1] + LIFTS;
  return lpp->len[LAB_INCOM] + lpp->len[LAB_QUADG]
       + lpp->len[LAB_LIFT] + lpp->len[LAB_LING];
}

/* Final clear-witness shape (post-compile_bincnst).  Returns r; sets
 * *nn_out (total polys) when non-NULL; writes per-vector lengths into
 * n_dst when non-NULL (caller guarantees capacity r). */
static size_t owt_shape(size_t *nn_out, size_t *n_dst,
                        const dch_pack_params pp){
  size_t r = 0, nn = 0, i;
#define PUT(x) do { size_t x_ = (x); if(n_dst) n_dst[r] = x_; \
                    nn += x_; r++; } while(0)

  if(pp->pp_pack->np == 0){
    /* pack input == dch output witness after compile_bincnst
     * (dch_pack_prove); owt is a straight witness_copy of it */
    for(i=0;i<pp->pp_dch->r;i++)
      PUT(pp->pp_dch->n[i]);
    for(i=0;i<pp->pp_dch->r;i++)
      if(pp->pp_dch->normty[i] == BIN)
        PUT(pp->pp_dch->n[i]);
  }
  else if(pp->pp_pack->zkp != NULL &&
          pp->pp_pack->zkround == pp->pp_pack->np - 1){
    /* witness_copy of the LNP output witness after compile_bincnst */
    for(i=0;i<NWIT;i++)
      PUT(i == (size_t)XBIN ? pp->pp_pack->zkp[0]->xbinlen
                            : pp->pp_pack->zkp[0]->len[i]);
    PUT(pp->pp_pack->zkp[0]->xbinlen);          /* sigmam1(xbin) */
  }
  else{
    /* lab_witness_init dims of the final round, plus the sigmam1(bin)
     * append compile_bincnst performs after compressed rounds */
    const struct _lab_params *lpp = pp->pp_pack->p[pp->pp_pack->np - 1];
    size_t mid = 0;
    PUT(lpp->nmax);
    if(lpp->fz == 2)
      PUT(lpp->nmax);
    if(!lpp->tail)
      mid = lpp->len[LAB_INCOM] + lpp->len[LAB_QUADG] + lpp->len[LAB_LING];
    if(lpp->compressed)
      mid += lpp->len[LAB_OUTCOM];
    if(!lpp->tail || lpp->compressed)
      PUT(mid);
    if(lpp->compressed){
      PUT(lpp->len[LAB_BIN]);                   /* binary randomness */
      PUT(lpp->len[LAB_BIN]);                   /* sigmam1(bin) append */
    }
  }
#undef PUT
  if(nn_out) *nn_out = nn;
  return r;
}

size_t fap1_payload_size(const dch_pack_params pp){
  size_t sz = 0, i, nn;

  if(pp->pp_dch->nexact || pp->pp_dch->nquad)
    sz += pp->pp_dch->kappa_outer * N * FAP1_CBYTES;

  for(i=0;i<pp->pp_pack->np;i++){
    if(round_empty(pp->pp_pack, i)) continue;
    sz += round_npolz(pp->pp_pack->p[i]) * N * FAP1_CBYTES;
    if(!pp->pp_pack->p[i]->compressed)
      sz += 256 * 4;                       /* p[256], int32 LE */
  }

  if(pp->pp_pack->zkp != NULL)
    sz += (4*pp->pp_pack->zkp[0]->kappa_linfmsis + 256/N) * N * FAP1_CBYTES;

  owt_shape(&nn, NULL, pp);
  sz += nn * N * 2;                        /* int16 LE coefficients */

  return sz;
}

size_t fap1_wire_size(const dch_pack_params pp){
  return FAP1_HDR_BYTES + fap1_payload_size(pp);
}

/* ---------- coefficient codecs ---------- */

/* Signed value of one polz coefficient's limb representation, with the
 * representation tripwire (only the setcoeff_fromint64 image — nonneg
 * low digits, signed top limb — is bijective with the value, and the FS
 * hash covers the limbs). */
static int polz_getval(int64_t *v, const polz a, int k){
  size_t i;
  zz c;
  int64_t x = 0;

  polz_getcoeff(c, a, k);
  for(i=0;i<L;i++){
    int32_t li = c->limbs[i];
    if(i < L-1 && (li < 0 || li >= (1<<14)))
      return FAP1_ERR_ENC_REP;
    x += (int64_t)li * ((int64_t)1 << (14*i));
  }
  if(x >= FAP1_VMAX || x <= -FAP1_VMAX)
    return FAP1_ERR_ENC_REP;
  *v = x;
  return FAP1_OK;
}

static int put_polzvec(uint8_t **w, const polz *a, size_t len){
  size_t j, i;
  int k;
  int64_t v;
  uint64_t u;
  int ret;

  for(j=0;j<len;j++){
    for(k=0;k<N;k++){
      ret = polz_getval(&v, a[j], k);
      if(ret)
        return ret;
      u = (uint64_t)v;                     /* two's complement */
      for(i=0;i<FAP1_CBYTES;i++)
        (*w)[i] = (uint8_t)(u >> (8*i));
      *w += FAP1_CBYTES;
    }
  }
  return FAP1_OK;
}

static int get_polzvec(polz *r, const uint8_t **in, size_t len){
  size_t j, i;
  int k;
  uint64_t u;
  int64_t v;

  for(j=0;j<len;j++){
    for(k=0;k<N;k++){
      u = 0;
      for(i=0;i<FAP1_CBYTES;i++)
        u |= (uint64_t)(*in)[i] << (8*i);
      /* sign-extend from bit 8*FAP1_CBYTES-1 */
      v = (int64_t)(u << (64 - 8*FAP1_CBYTES)) >> (64 - 8*FAP1_CBYTES);
      if(v >= FAP1_VMAX || v <= -FAP1_VMAX)
        return FAP1_ERR_NONCANON;
      polz_setcoeff_fromint64(r[j], v, k);   /* exact inverse of the digits */
      *in += FAP1_CBYTES;
    }
  }
  return FAP1_OK;
}

static void put_polyvec(uint8_t **w, const poly *a, size_t len){
  size_t j;
  int k;
  uint16_t u;

  for(j=0;j<len;j++){
    for(k=0;k<N;k++){
      u = (uint16_t)a[j]->c[k];
      (*w)[0] = (uint8_t)u;
      (*w)[1] = (uint8_t)(u >> 8);
      *w += 2;
    }
  }
}

static void get_polyvec(poly *r, const uint8_t **in, size_t len){
  size_t j;
  int k;

  for(j=0;j<len;j++){
    for(k=0;k<N;k++){
      r[j]->c[k] = (int16_t)(uint16_t)((*in)[0] | ((uint16_t)(*in)[1] << 8));
      *in += 2;
    }
  }
}

static void put_i32vec(uint8_t **w, const int32_t *a, size_t len){
  size_t j, i;
  uint32_t u;

  for(j=0;j<len;j++){
    u = (uint32_t)a[j];
    for(i=0;i<4;i++)
      (*w)[i] = (uint8_t)(u >> (8*i));
    *w += 4;
  }
}

static void get_i32vec(int32_t *r, const uint8_t **in, size_t len){
  size_t j, i;
  uint32_t u;

  for(j=0;j<len;j++){
    u = 0;
    for(i=0;i<4;i++)
      u |= (uint32_t)(*in)[i] << (8*i);
    r[j] = (int32_t)u;
    *in += 4;
  }
}

/* ---------- proof allocation (upstream shapes, zero-filled) ---------- */

static void owt_alloc(witness owt, const dch_pack_params pp){
  size_t r, nn, i;

  r = owt_shape(&nn, NULL, pp);
  witness_init(owt, r, r);
  owt_shape(NULL, owt->n, pp);
  owt->s[0] = _aligned_alloc(64, nn * sizeof(poly));
  for(i=1;i<r;i++)
    owt->s[i] = &owt->s[i-1][owt->n[i-1]];
  memset(owt->s[0], 0, nn * sizeof(poly));
}

void fap1_proof_alloc(dch_pack_proof pi, const dch_pack_params pp){
  size_t i;

  if(pp->pp_dch->nexact || pp->pp_dch->nquad){
    pi->pi_dch->com = _aligned_alloc(64, pp->pp_dch->kappa_outer * sizeof(polz));
    polzvec_setzero(pi->pi_dch->com, pp->pp_dch->kappa_outer);
  }
  else{
    pi->pi_dch->com = NULL;
  }

  pack_proof_init(pi->pi_pack, pp->pp_pack);
  for(i=0;i<pp->pp_pack->np;i++){
    if(round_empty(pp->pp_pack, i)){
      memset(pi->pi_pack->p[i], 0, sizeof(pi->pi_pack->p[i]));
      continue;
    }
    lab_proof_init(pi->pi_pack->p[i], pp->pp_pack->p[i]);
    polzvec_setzero(pi->pi_pack->p[i]->m[0], round_npolz(pp->pp_pack->p[i]));
    memset(pi->pi_pack->p[i]->p, 0, sizeof(pi->pi_pack->p[i]->p));
  }

  if(pp->pp_pack->zkp != NULL){
    lnp_proof_init(pi->pi_pack->zkp[0], pp->pp_pack->zkp[0]);
    polzvec_setzero(pi->pi_pack->zkp[0]->m[0],
                    4*pp->pp_pack->zkp[0]->kappa_linfmsis + 256/N);
  }

  owt_alloc(pi->pi_pack->owt, pp);
}

/* ---------- encode / decode ---------- */

int fap1_encode(uint8_t *out, size_t outcap, size_t *outlen,
                const dch_pack_proof pi, const dch_pack_params pp){
  size_t pay = fap1_payload_size(pp);
  size_t total = FAP1_HDR_BYTES + pay;
  size_t i, r;
  uint8_t *w;
  int ret;

  if(outcap < total)
    return FAP1_ERR_BUF;

  r = owt_shape(NULL, NULL, pp);
  if(pi->pi_pack->owt->r != r)         /* params/proof skew, never expected */
    return FAP1_ERR_ENC_REP;

  out[0] = 'F'; out[1] = 'A'; out[2] = 'P'; out[3] = '1';
  out[4] = (uint8_t)LOGQ;
  out[5] = pp->pp_pack->zkp != NULL ? 1 : 0;
  out[6] = (uint8_t)pp->pp_pack->np;
  out[7] = 0;
  for(i=0;i<4;i++)
    out[8+i] = (uint8_t)((uint32_t)pay >> (8*i));

  w = out + FAP1_HDR_BYTES;

  if(pi->pi_dch->com != NULL){
    ret = put_polzvec(&w, pi->pi_dch->com, pp->pp_dch->kappa_outer);
    if(ret) return ret;
  }

  for(i=0;i<pp->pp_pack->np;i++){
    if(round_empty(pp->pp_pack, i)) continue;
    ret = put_polzvec(&w, pi->pi_pack->p[i]->m[0],
                      round_npolz(pp->pp_pack->p[i]));
    if(ret) return ret;
    if(!pp->pp_pack->p[i]->compressed)
      put_i32vec(&w, pi->pi_pack->p[i]->p, 256);
  }

  if(pp->pp_pack->zkp != NULL){
    ret = put_polzvec(&w, pi->pi_pack->zkp[0]->m[0],
                      4*pp->pp_pack->zkp[0]->kappa_linfmsis + 256/N);
    if(ret) return ret;
  }

  for(i=0;i<r;i++)
    put_polyvec(&w, pi->pi_pack->owt->s[i], pi->pi_pack->owt->n[i]);

  *outlen = total;
  return FAP1_OK;
}

int fap1_decode(dch_pack_proof pi, const uint8_t *in, size_t inlen,
                const dch_pack_params pp){
  size_t pay, i;
  uint32_t hdrpay;
  const uint8_t *rd;
  int ret;

  if(inlen < FAP1_HDR_BYTES)
    return FAP1_ERR_TRUNC;
  if(in[0] != 'F' || in[1] != 'A' || in[2] != 'P' || in[3] != '1')
    return FAP1_ERR_MAGIC;
  if(in[4] != (uint8_t)LOGQ)
    return FAP1_ERR_SUITE;
  if(in[5] != (pp->pp_pack->zkp != NULL ? 1 : 0))
    return FAP1_ERR_FLAGS;
  if(in[6] != (uint8_t)pp->pp_pack->np)
    return FAP1_ERR_NP;
  if(in[7] != 0)
    return FAP1_ERR_RESERVED;

  pay = fap1_payload_size(pp);
  hdrpay = 0;
  for(i=0;i<4;i++)
    hdrpay |= (uint32_t)in[8+i] << (8*i);
  if(hdrpay != (uint32_t)pay)
    return FAP1_ERR_PAYLEN;
  if(inlen != FAP1_HDR_BYTES + pay)
    return FAP1_ERR_TRUNC;

  fap1_proof_alloc(pi, pp);
  rd = in + FAP1_HDR_BYTES;

  if(pi->pi_dch->com != NULL){
    ret = get_polzvec(pi->pi_dch->com, &rd, pp->pp_dch->kappa_outer);
    if(ret) goto fail;
  }

  for(i=0;i<pp->pp_pack->np;i++){
    if(round_empty(pp->pp_pack, i)) continue;
    ret = get_polzvec(pi->pi_pack->p[i]->m[0], &rd,
                      round_npolz(pp->pp_pack->p[i]));
    if(ret) goto fail;
    if(!pp->pp_pack->p[i]->compressed)
      get_i32vec(pi->pi_pack->p[i]->p, &rd, 256);
  }

  if(pp->pp_pack->zkp != NULL){
    ret = get_polzvec(pi->pi_pack->zkp[0]->m[0], &rd,
                      4*pp->pp_pack->zkp[0]->kappa_linfmsis + 256/N);
    if(ret) goto fail;
  }

  for(i=0;i<pi->pi_pack->owt->r;i++)
    get_polyvec(pi->pi_pack->owt->s[i], &rd, pi->pi_pack->owt->n[i]);

  return FAP1_OK;

fail:
  dch_pack_proof_free(pi);
  return ret;
}

/* ================= FAP2: per-segment adaptive bitpacking ================= */

/* Coefficient count of a segment (i32 segments count elements directly;
 * polz/poly count N ring coefficients per element).  Every count is a
 * multiple of 8 (all are multiples of N=256, or exactly 256), so bitpacked
 * segments end on a byte boundary — asserted structurally, no pad bits. */
static size_t seg_ncoeff(const fap2_seg *s){
  return s->nelem * (s->kind == FAP2_SEG_I32 ? 1 : N);
}

size_t fap2_nsegs(const dch_pack_params pp){
  size_t n = 0, i;

  if(pp->pp_dch->nexact || pp->pp_dch->nquad)
    n++;
  for(i=0;i<pp->pp_pack->np;i++){
    if(round_empty(pp->pp_pack, i)) continue;
    if(pp->pp_pack->p[i]->compressed)
      n += 1;
    else if(!pp->pp_pack->p[i]->tail)
      n += 3 + 1;                      /* U1, lift, U2 + p[256] */
    else
      n += 4 + 1;                      /* t, g, lift, h + p[256] */
  }
  if(pp->pp_pack->zkp != NULL)
    n += 5;                            /* U1, U2, z, U4, U5 */
  n += owt_shape(NULL, NULL, pp);
  return n;
}

void fap2_seglist(fap2_seg *segs, const dch_pack_proof pi,
                  const dch_pack_params pp){
  size_t ns = 0, i, r, *nvec;
  polz *base;

#define SEG(k, ptr, ne) do { segs[ns].kind = (k); segs[ns].p = (void *)(ptr); \
                             segs[ns].nelem = (ne); ns++; } while(0)

  if(pp->pp_dch->nexact || pp->pp_dch->nquad)
    SEG(FAP2_SEG_POLZ, pi ? pi->pi_dch->com : NULL, pp->pp_dch->kappa_outer);

  for(i=0;i<pp->pp_pack->np;i++){
    const struct _lab_params *lpp;
    if(round_empty(pp->pp_pack, i)) continue;
    lpp = pp->pp_pack->p[i];
    base = pi ? pi->pi_pack->p[i]->m[0] : NULL;
    if(lpp->compressed)
      SEG(FAP2_SEG_POLZ, base, 4*lpp->kappa[2]);
    else if(!lpp->tail){
      /* memory order per lab_proof_init: U1 | lift | U2 */
      SEG(FAP2_SEG_POLZ, base, lpp->kappa[1]);
      SEG(FAP2_SEG_POLZ, base ? base + lpp->kappa[1] : NULL, LIFTS);
      SEG(FAP2_SEG_POLZ, base ? base + lpp->kappa[1] + LIFTS : NULL,
          lpp->kappa[1]);
    }
    else{
      /* memory order per lab_proof_init: t | g | lift | h */
      size_t off = 0;
      SEG(FAP2_SEG_POLZ, base, lpp->len[LAB_INCOM]);
      off += lpp->len[LAB_INCOM];
      SEG(FAP2_SEG_POLZ, base ? base + off : NULL, lpp->len[LAB_QUADG]);
      off += lpp->len[LAB_QUADG];
      SEG(FAP2_SEG_POLZ, base ? base + off : NULL, lpp->len[LAB_LIFT]);
      off += lpp->len[LAB_LIFT];
      SEG(FAP2_SEG_POLZ, base ? base + off : NULL, lpp->len[LAB_LING]);
    }
    if(!lpp->compressed)
      SEG(FAP2_SEG_I32, pi ? pi->pi_pack->p[i]->p : NULL, 256);
  }

  if(pp->pp_pack->zkp != NULL){
    size_t kap = pp->pp_pack->zkp[0]->kappa_linfmsis, zl = 256/N;
    base = pi ? pi->pi_pack->zkp[0]->m[0] : NULL;
    /* memory order per lnp_proof_init: U1 | U2 | z | U4 | U5 */
    SEG(FAP2_SEG_POLZ, base, kap);
    SEG(FAP2_SEG_POLZ, base ? base + kap : NULL, kap);
    SEG(FAP2_SEG_POLZ, base ? base + 2*kap : NULL, zl);
    SEG(FAP2_SEG_POLZ, base ? base + 2*kap + zl : NULL, kap);
    SEG(FAP2_SEG_POLZ, base ? base + 3*kap + zl : NULL, kap);
  }

  r = owt_shape(NULL, NULL, pp);
  nvec = _malloc(r * sizeof(size_t));
  owt_shape(NULL, nvec, pp);
  for(i=0;i<r;i++)
    SEG(FAP2_SEG_POLY, pi ? (void *)pi->pi_pack->owt->s[i] : NULL, nvec[i]);
  free(nvec);
#undef SEG
}

/* per-kind maximum width: polz LOGQ(+sign), owt int16 15(+sign),
 * p[256] int32 31(+sign) */
static unsigned seg_wcap(uint8_t kind, int sgn){
  unsigned w = kind == FAP2_SEG_POLZ ? LOGQ
             : kind == FAP2_SEG_POLY ? 15 : 31;
  return w + (sgn ? 1 : 0);
}

size_t fap2_wire_maxsize(const dch_pack_params pp){
  size_t ns = fap2_nsegs(pp), i, sz = FAP1_HDR_BYTES;
  fap2_seg *segs = _malloc(ns * sizeof *segs);

  fap2_seglist(segs, NULL, pp);
  for(i=0;i<ns;i++)
    sz += 1 + (seg_ncoeff(&segs[i]) * seg_wcap(segs[i].kind, 1) + 7)/8;
  free(segs);
  return sz;
}

/* ---------- LSB-first bit I/O (w <= 40 < 57, so a u64 accumulator with
 * < 8 pending bits never overflows) ---------- */

typedef struct { uint8_t *p; uint64_t acc; unsigned n; } bitwr;
typedef struct { const uint8_t *p; uint64_t acc; unsigned n; } bitrd;

static void bw_put(bitwr *b, uint64_t u, unsigned w){
  b->acc |= u << b->n;
  b->n += w;
  while(b->n >= 8){
    *b->p++ = (uint8_t)b->acc;
    b->acc >>= 8;
    b->n -= 8;
  }
}

static uint64_t br_get(bitrd *b, unsigned w){
  uint64_t u;
  while(b->n < w){
    b->acc |= (uint64_t)(*b->p++) << b->n;
    b->n += 8;
  }
  u = b->acc & (((uint64_t)1 << w) - 1);
  b->acc >>= w;
  b->n -= w;
  return u;
}

/* value of coefficient idx of a segment; representation already validated
 * (encode pass 1) so no error path */
static int64_t seg_get(const fap2_seg *s, size_t idx){
  int64_t v = 0;

  switch(s->kind){
  case FAP2_SEG_POLZ:
    polz_getval(&v, ((const polz *)s->p)[idx/N], (int)(idx%N));
    break;
  case FAP2_SEG_POLY:
    v = ((const poly *)s->p)[idx/N]->c[idx%N];
    break;
  default:
    v = ((const int32_t *)s->p)[idx];
    break;
  }
  return v;
}

/* min/max of a segment, with the polz representation tripwire */
static int seg_minmax(const fap2_seg *s, int64_t *mn, int64_t *mx){
  size_t j, nc = seg_ncoeff(s);
  int64_t v, lo = INT64_MAX, hi = INT64_MIN;
  int ret;

  for(j=0;j<nc;j++){
    if(s->kind == FAP2_SEG_POLZ){
      ret = polz_getval(&v, ((const polz *)s->p)[j/N], (int)(j%N));
      if(ret)
        return ret;
    }
    else{
      v = seg_get(s, j);
    }
    if(v < lo) lo = v;
    if(v > hi) hi = v;
  }
  *mn = lo;
  *mx = hi;
  return FAP1_OK;
}

/* minimal width for [mn, mx]: unsigned iff mn >= 0; w == 0 iff all zero */
static void seg_width(int64_t mn, int64_t mx, int *sgn, unsigned *w){
  unsigned b;

  if(mn >= 0){
    *sgn = 0;
    b = 0;
    while(mx >> b)
      b++;
    *w = b;
  }
  else{
    *sgn = 1;
    b = 1;
    while(mn < -((int64_t)1 << (b-1)) || mx >= ((int64_t)1 << (b-1)))
      b++;
    *w = b;
  }
}

int fap2_encode(uint8_t *out, size_t outcap, size_t *outlen,
                const dch_pack_proof pi, const dch_pack_params pp){
  size_t ns, i, j, r, nc, segbytes, pay;
  fap2_seg *segs;
  uint8_t *w;
  size_t remaining;
  int64_t mn, mx;
  uint64_t mask;
  unsigned wd;
  int sgn, ret;
  bitwr bw;

  if(outcap < FAP1_HDR_BYTES)
    return FAP1_ERR_BUF;

  /* params/proof skew tripwire, as in fap1 (checked BEFORE the seglist
   * dereferences owt->s[i] at the params-derived count) */
  r = owt_shape(NULL, NULL, pp);
  if(pi->pi_pack->owt->r != r)
    return FAP1_ERR_ENC_REP;

  ns = fap2_nsegs(pp);
  segs = _malloc(ns * sizeof *segs);
  fap2_seglist(segs, pi, pp);

  /* per-vector owt lengths, since the seglist's nelem are params-derived */
  for(i=0;i<r;i++){
    if(pi->pi_pack->owt->n[i] != segs[ns-r+i].nelem){
      free(segs);
      return FAP1_ERR_ENC_REP;
    }
  }

  out[0] = 'F'; out[1] = 'A'; out[2] = 'P'; out[3] = '2';
  out[4] = (uint8_t)LOGQ;
  out[5] = pp->pp_pack->zkp != NULL ? 1 : 0;
  out[6] = (uint8_t)pp->pp_pack->np;
  out[7] = 0;

  w = out + FAP1_HDR_BYTES;
  remaining = outcap - FAP1_HDR_BYTES;

  for(i=0;i<ns;i++){
    ret = seg_minmax(&segs[i], &mn, &mx);
    if(ret){
      free(segs);
      return ret;
    }
    seg_width(mn, mx, &sgn, &wd);
    nc = seg_ncoeff(&segs[i]);
    segbytes = (nc/8) * wd;              /* nc is a multiple of 8 */
    if(remaining < 1 + segbytes){
      free(segs);
      return FAP1_ERR_BUF;
    }
    *w++ = (uint8_t)((sgn << 7) | wd);
    if(wd){
      mask = ((uint64_t)1 << wd) - 1;
      bw.p = w; bw.acc = 0; bw.n = 0;
      for(j=0;j<nc;j++)
        bw_put(&bw, (uint64_t)seg_get(&segs[i], j) & mask, wd);
      w += segbytes;
    }
    remaining -= 1 + segbytes;
  }
  free(segs);

  pay = (size_t)(w - (out + FAP1_HDR_BYTES));
  for(i=0;i<4;i++)
    out[8+i] = (uint8_t)((uint32_t)pay >> (8*i));

  *outlen = FAP1_HDR_BYTES + pay;
  return FAP1_OK;
}

int fap2_decode(dch_pack_proof pi, const uint8_t *in, size_t inlen,
                const dch_pack_params pp){
  size_t ns, i, j, nc, segbytes, remaining;
  uint32_t hdrpay;
  fap2_seg *segs = NULL;
  const uint8_t *rd;
  int64_t v, mn, mx;
  uint64_t u;
  unsigned wd;
  uint8_t d;
  int sgn, ret;
  bitrd br;

  if(inlen < FAP1_HDR_BYTES)
    return FAP1_ERR_TRUNC;
  if(in[0] != 'F' || in[1] != 'A' || in[2] != 'P' || in[3] != '2')
    return FAP1_ERR_MAGIC;
  if(in[4] != (uint8_t)LOGQ)
    return FAP1_ERR_SUITE;
  if(in[5] != (pp->pp_pack->zkp != NULL ? 1 : 0))
    return FAP1_ERR_FLAGS;
  if(in[6] != (uint8_t)pp->pp_pack->np)
    return FAP1_ERR_NP;
  if(in[7] != 0)
    return FAP1_ERR_RESERVED;

  hdrpay = 0;
  for(i=0;i<4;i++)
    hdrpay |= (uint32_t)in[8+i] << (8*i);
  if((size_t)hdrpay > fap2_wire_maxsize(pp) - FAP1_HDR_BYTES)
    return FAP1_ERR_PAYLEN;
  if(inlen != FAP1_HDR_BYTES + (size_t)hdrpay)
    return FAP1_ERR_TRUNC;

  fap1_proof_alloc(pi, pp);
  ns = fap2_nsegs(pp);
  segs = _malloc(ns * sizeof *segs);
  fap2_seglist(segs, pi, pp);

  rd = in + FAP1_HDR_BYTES;
  remaining = hdrpay;

  for(i=0;i<ns;i++){
    if(remaining < 1){
      ret = FAP1_ERR_TRUNC;
      goto fail;
    }
    d = *rd++;
    remaining--;
    sgn = d >> 7;
    wd = d & 0x7f;

    if(wd == 0){
      /* all-zero segment: signed flag invalid; alloc already zero-filled
       * (the all-limbs-zero representation is the unique in-image
       * representation of value 0, so this is bit-exact) */
      if(sgn){
        ret = FAP2_ERR_WIDTH;
        goto fail;
      }
      continue;
    }
    if(wd > seg_wcap(segs[i].kind, sgn)){
      ret = FAP2_ERR_WIDTH;
      goto fail;
    }

    nc = seg_ncoeff(&segs[i]);
    segbytes = (nc/8) * wd;
    if(remaining < segbytes){
      ret = FAP1_ERR_TRUNC;
      goto fail;
    }

    br.p = rd; br.acc = 0; br.n = 0;
    mn = INT64_MAX; mx = INT64_MIN;
    for(j=0;j<nc;j++){
      u = br_get(&br, wd);
      if(sgn)
        v = (int64_t)(u << (64 - wd)) >> (64 - wd);
      else
        v = (int64_t)u;
      if(segs[i].kind == FAP2_SEG_POLZ){
        if(v >= FAP1_VMAX || v <= -FAP1_VMAX){
          ret = FAP1_ERR_NONCANON;   /* only reachable: -2^LOGQ at wd=LOGQ+1 */
          goto fail;
        }
        polz_setcoeff_fromint64(((polz *)segs[i].p)[j/N], v, (int)(j%N));
      }
      else if(segs[i].kind == FAP2_SEG_POLY){
        ((poly *)segs[i].p)[j/N]->c[j%N] = (int16_t)v;
      }
      else{
        ((int32_t *)segs[i].p)[j] = (int32_t)v;
      }
      if(v < mn) mn = v;
      if(v > mx) mx = v;
    }

    /* canonicity: signed flag requires a negative; width must be minimal */
    if(sgn){
      if(mn >= 0 ||
         (wd >= 2 && mn >= -((int64_t)1 << (wd-2))
                  && mx <  ((int64_t)1 << (wd-2)))){
        ret = FAP2_ERR_NONMIN;
        goto fail;
      }
    }
    else if(mx < ((int64_t)1 << (wd-1))){
      ret = FAP2_ERR_NONMIN;
      goto fail;
    }

    rd += segbytes;
    remaining -= segbytes;
  }

  if(remaining != 0){
    ret = FAP1_ERR_TRUNC;
    goto fail;
  }

  free(segs);
  return FAP1_OK;

fail:
  free(segs);
  dch_pack_proof_free(pi);
  return ret;
}
