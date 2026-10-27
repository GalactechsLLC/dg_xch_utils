/* DGX experimental FN-DSA-512 relation, raw and ZK PACKED11-v2 profiles.
 * The prover is a separate, resource-limited worker. No secret signing keys
 * enter this interface. Upstream has a global commitment key: one job/process.
 */
#include <stdlib.h>
#include <string.h>
#include "labrados_python.h"
#include "comkey.h"
#include "fips202.h"
#include "wire.h"

#pragma GCC visibility push(default)

#define DEG 512
#define BITS 11
#define PARTS 13
#define BINF 840
#define MAX_BATCH 256
#define MAX_PROOF (1024 * 1024)
typedef struct {
    statement st;
    witness wt;
    dch_pack_params pp;
    size_t count;
    size_t parts;
    int ready;
    int zk;
    int64_t *publics;
} dg_job;

/* Keep each norm family contiguous so Dachshund can merge parts across
 * signatures. This is only a layout change; each exact bound remains separate. */
static size_t part_index(const dg_job *j, size_t slot, size_t part) {
    return part < 5 ? 5*slot+part : 5*j->count+(j->parts-5)*slot+part-5;
}

static int append(dg_job *j, size_t slot, size_t nv, size_t *idx,
                  size_t *dim, size_t deg, int64_t *phi, int64_t *rhs, int full) {
    for (size_t k = 0; k < nv; k++) idx[k] = part_index(j,slot,idx[k]);
    return py_append_constraint(j->st, nv, idx, dim, deg, phi, rhs, full);
}

void dg_labrador_free(void *ptr) {
    dg_job *j = ptr;
    if (!j) return;
    if (j->ready) dch_pack_params_free(j->pp);
    statement_free(j->st);
    witness_free(j->wt);
    comkey_free();
    free(j->publics);
    free(j);
}

void *dg_labrador_new(size_t count, const int64_t *h, const int64_t *target,
                      const unsigned char context[32], int zk) {
    if (!count || count > MAX_BATCH || (zk != 0 && zk != 1)) return NULL;
    /* For a target with any centered coefficient outside [-840,840],
     * s2=0 forces s1=target modulo 12289 and contradicts the proven s1
     * range. Only all-small targets need the explicit nonzero gadget.
     * Both prover and verifier derive this from the same public targets;
     * if any batch slot needs it, use the full layout for the whole job. */
    int needs_nonzero=0;
    for(size_t slot=0;slot<count;slot++) {
        int all_small=1;
        for(size_t k=0;k<DEG;k++) {
            int64_t x=target[slot*DEG+k];
            if(x < -6144 || x > 6144) return NULL;
            if(x < -BINF || x > BINF) all_small=0;
        }
        needs_nonzero |= all_small;
    }
    size_t parts=needs_nonzero ? PARTS : PARTS-2;
    dg_job *j = calloc(1, sizeof(*j));
    size_t *dims = calloc(parts * count, sizeof(*dims));
    uint64_t *bounds = calloc(parts * count, sizeof(*bounds));
    uint64_t *required = calloc(parts * count, sizeof(*required));
    normtype *types = calloc(parts * count, sizeof(*types));
    if (!j || !dims || !bounds || !required || !types) abort();
    const size_t shape[PARTS] = {4, 4, 2, 2, 4, 2*BITS, 2*BITS, 1,
        2*BITS, 2*BITS, 2*BITS, 2*BITS, 1};
    const uint64_t norms[PARTS] = {265896, 65536, 223027200, 1280000,
        34034726, DEG*BITS, DEG*BITS, 256,
        DEG*BITS, DEG*BITS, DEG*BITS, DEG*BITS, 256};
    j->count = count;
    j->parts = parts;
    j->zk = zk;
    for (size_t s = 0; s < count; s++) for (size_t p = 0; p < parts; p++) {
        size_t i=part_index(j,s,p);
        dims[i] = shape[p];
        bounds[i] = norms[p];
        types[i] = p >= 5 ? BIN : L2EXACT;
    }
    py_init_statement(j->st, parts*count, dims, bounds, required, types,
        (7+(needs_nonzero ? BITS : 0))*count, (1+needs_nonzero)*count, 0);
    py_init_witness(j->wt, parts*count, dims);
    free(dims); free(bounds); free(required); free(types);
    j->count = count;
    j->publics = malloc(2*count*DEG*sizeof(int64_t));
    if (!j->publics) abort();
    memcpy(j->publics, h, count*DEG*sizeof(int64_t));
    memcpy(j->publics+count*DEG, target, count*DEG*sizeof(int64_t));
    int64_t *phi = calloc(3*BITS*DEG, sizeof(int64_t));
    if (!phi) abort();
    int64_t zero[DEG] = {0};
    for (size_t slot = 0; slot < count; slot++) {
        const int64_t *pk = h + slot*DEG;
        /* Exact integer lift of h*s2+s1 = target (mod 12289). The
         * decomposition/norm bounds imply |residual| < Q=274877906837,
         * so equality modulo Q implies integer equality coefficientwise. */
        memset(phi, 0, (2+2*BITS)*DEG*sizeof(int64_t));
        for (size_t k=0; k<DEG; k++) { phi[k]=16*pk[k]; phi[2*DEG+k]=pk[k]; }
        phi[DEG]=16; phi[3*DEG]=1; phi[4*DEG]=1228900; phi[5*DEG]=12289;
        size_t ei[]={0,1,2,3}, ed[]={2,2,1,1};
        if (append(j,slot,4,ei,ed,2,phi,(int64_t *)target+slot*DEG,1)) abort();
        for (size_t part=0; part<2; part++) {
            memset(phi,0,6*DEG*sizeof(int64_t));
            phi[part*DEG]=16; phi[(2+part)*DEG]=1; phi[(4+part)*DEG]=-1;
            size_t li[]={0,1,4}, ld[]={2,2,2};
            if (append(j,slot,3,li,ld,2,phi,zero,1)) abort();
        }
        /* s2 = u-v, where u and v each have eleven binary coefficient
         * planes. Thus |s2[j]| <= 2047, bounding the representation used by comp_decode.
         * Disjointness is unnecessary: floor(|u-v|/128) is at most
         * floor(u/128)+floor(v/128). The budget below proves that a
         * canonical padded signature exists, even for noncanonical u,v. */
        memset(phi,0,(2+2*BITS)*DEG*sizeof(int64_t));
        phi[0]=1;
        for (size_t bit=0; bit<BITS; bit++) {
            phi[(2+bit)*DEG]=-(1LL<<bit);
            phi[(2+BITS+bit)*DEG]=(1LL<<bit);
        }
        size_t mi[]={4,5,6}, md[]={2,BITS,BITS};
        if (append(j,slot,3,mi,md,2,phi,zero,1)) abort();
        /* Constant term of a(X)*b(X) equals the coefficient dot product
         * when a = (w0,-w255,...,-w1). Degree-512 inputs are interleaved
         * into two degree-256 polynomials by the upstream adapter. */
        memset(phi,0,(2+2*BITS)*DEG*sizeof(int64_t));
        for (size_t side=0; side<2; side++) for (size_t p=0; p<2*BITS; p++) {
            int64_t w = p/2 >= 7 ? 1LL<<(p/2-7) : 0;
            size_t off=(side*2*BITS+p)*256;
            phi[off]=w;
            for (size_t k=1;k<256;k++) phi[off+k]=-w;
        }
        size_t off=4*BITS*256;
        phi[off]=1;
        for (size_t k=1;k<256;k++) phi[off+256-k]=-(k<9 ? 1LL<<k : 512);
        int64_t budget[256]={392};
        size_t bi[]={5,6,7}, bd[]={2*BITS,2*BITS,1};
        if (append(j,slot,3,bi,bd,1,phi,budget,0)) abort();
        /* With nonnegative binary u,v and s2=u-v already proved above,
         * weighted(u)+weighted(v)+z=840 implies |s2|<=840. A canonical
         * sign/magnitude representation admits z=840-|s2|, so this saves
         * one witness part without excluding any valid FN-DSA signature.
         * Every term is <=2047; no reduction modulo the proof modulus can
         * turn an out-of-range coefficient into a valid equality. */
        int64_t range[DEG];
        for (size_t k=0;k<DEG;k++) range[k]=BINF;
        memset(phi,0,3*BITS*DEG*sizeof(int64_t));
        // Reuse s2=u-v to write s2+2*v+z=840 with fewer dense coefficients.
        phi[0]=1;
        for(size_t bit=0;bit<BITS;bit++) {
            phi[(2+bit)*DEG]=2LL<<bit;
            phi[(2+BITS+bit)*DEG]=1LL<<bit;
        }
        size_t si[]={4,6,8}, sd[]={2,BITS,BITS};
        if(append(j,slot,3,si,sd,2,phi,range,1)) abort();
        /* For s1, binary x=s1+840 and y=840-s1 enforce the exact range. */
        for (size_t side=0;side<2;side++) {
            memset(phi,0,3*BITS*DEG*sizeof(int64_t));
            phi[DEG]=side ? 1 : -1;
            for(size_t bit=0;bit<BITS;bit++) phi[(2+bit)*DEG]=1LL<<bit;
            size_t ri[]={4,9+side}, rd[]={2,BITS};
            if(append(j,slot,2,ri,rd,2,phi,range,1)) abort();
        }
        if(needs_nonzero) {
        /* The upstream verifier rejects the all-zero s2 vector. Enforce
         * disjoint sign bitplanes with a binary w=u+v for EACH plane, then
         * sum(w)=1+binary slack. If u=v, uniqueness of binary encoding and
         * disjointness force w=0, contradicting the positive total. This
         * adds no public information about the signature coefficients. */
        for(size_t bit=0;bit<BITS;bit++) {
            memset(phi,0,3*BITS*DEG*sizeof(int64_t));
            phi[bit*DEG]=1;
            phi[(BITS+bit)*DEG]=1;
            phi[(2*BITS+bit)*DEG]=-1;
            size_t ni[]={5,6,11}, nd[]={BITS,BITS,BITS};
            if(append(j,slot,3,ni,nd,2,phi,zero,1)) abort();
        }
        memset(phi,0,3*BITS*DEG*sizeof(int64_t));
        for(size_t poly=0;poly<2*BITS;poly++) {
            phi[poly*256]=1;
            for(size_t k=1;k<256;k++) phi[poly*256+k]=-1;
        }
        off=2*BITS*256;
        phi[off]=-1;
        for(size_t bit=1;bit<13;bit++) phi[off+256-bit]=1LL<<bit;
        int64_t one[256]={1};
        size_t ni[]={11,12}, nd[]={2*BITS,1};
        if(append(j,slot,2,ni,nd,1,phi,one,0)) abort();
        }
    }
    free(phi);
    /* Bind the complete execution context as well as the public relation.
     * Byte order is fixed; neither native struct padding nor producer
     * supplied parameter metadata enters the transcript. */
    shake256incctx hash;
    shake256_inc_init(&hash);
    const unsigned char domain[]="DGX/FN-DSA/LaBRADOR/PACKED11-v2/Q38/raw/FAP2";
    const unsigned char zkdomain[]="DGX/FN-DSA/LaBRADOR/PACKED11-v2/Q38/zkp/FAP2";
    shake256_inc_absorb(&hash,zk ? zkdomain : domain,sizeof(domain));
    shake256_inc_absorb(&hash,context,32);
    unsigned char buf[8];
    for(size_t k=0;k<8;k++) buf[k]=(count>>(8*k))&255;
    shake256_inc_absorb(&hash,buf,8);
    for(size_t i=0;i<2*count*DEG;i++) {
        uint64_t x=(uint64_t)j->publics[i];
        for(size_t k=0;k<8;k++) buf[k]=(x>>(8*k))&255;
        shake256_inc_absorb(&hash,buf,8);
    }
    shake256_inc_finalize(&hash);
    shake256_inc_squeeze(j->st->h,16,&hash);
    return j;
}

int dg_labrador_set(void *ptr, size_t slot, const int64_t s2[512]) {
    dg_job *j=ptr;
    if (!j || slot>=j->count) return 0;
    const int64_t *h=j->publics+slot*DEG;
    const int64_t *t=j->publics+(j->count+slot)*DEG;
    int64_t prod[DEG]={0}, s[2*DEG], hi[2*DEG], lo[2*DEG], c[DEG], d[DEG];
    int64_t u[BITS*DEG]={0}, v[BITS*DEG]={0}, slack[256]={0};
    int64_t ranges[3][BITS*DEG]={0}, nonzero[BITS*DEG]={0}, nzslack[256]={0};
    size_t nonzero_bits=0;
    for(size_t a=0;a<DEG;a++) for(size_t b=0;b<DEG;b++)
        prod[(a+b)%DEG]+=(a+b<DEG?1:-1)*h[a]*s2[b];
    uint64_t norm=0;
    int budget=392;
    for(size_t k=0;k<DEG;k++) {
        /* Setting a witness is not validation: the proof relation below
         * enforces BINF even for a prover bypassing the canonical decoder. */
        if (s2[k]<-2047 || s2[k]>2047) return 0;
        int64_t s1=(t[k]-prod[k])%12289;
        if(s1>6144) s1-=12289;
        if(s1<-6144) s1+=12289;
        s[k]=s2[k]; s[DEG+k]=s1;
        norm+=s1*s1+s2[k]*s2[k];
        int64_t lift=(t[k]-prod[k]-s1)/12289;
        d[k]=((lift+50)%100+100)%100-50;
        c[k]=(lift-d[k])/100;
        int64_t mag=llabs(s2[k]);
        budget-=mag/128;
        for(size_t b=0;b<BITS;b++) {
            int64_t bit=(mag>>b)&1;
            (s2[k]<0?v:u)[b*DEG+k]=bit;
            nonzero[b*DEG+k]=bit;
            nonzero_bits+=bit;
            ranges[0][b*DEG+k]=((BINF-mag)>>b)&1;
            ranges[1][b*DEG+k]=((s1+BINF)>>b)&1;
            ranges[2][b*DEG+k]=((BINF-s1)>>b)&1;
        }
    }
    if(norm>34034726 || budget<0) return 0;
    for(size_t bit=0;bit<13;bit++) nzslack[bit]=(((nonzero_bits ? nonzero_bits : 1)-1)>>bit)&1;
    for(size_t k=0;k<2*DEG;k++) { lo[k]=((s[k]+8)%16+16)%16-8; hi[k]=(s[k]-lo[k])/16; }
    for(size_t k=0;k<9;k++) slack[k]=(budget>>k)&1;
    const int64_t *parts[]={hi,lo,c,d,s,u,v,slack,
        ranges[0],ranges[1],ranges[2],nonzero,nzslack};
    const size_t dims[]={2,2,1,1,2,BITS,BITS,1,BITS,BITS,BITS,BITS,1};
    for(size_t i=0;i<j->parts;i++) if(py_set_witness_vector(j->wt,part_index(j,slot,i),dims[i],(i==7 || i==12)?1:2,parts[i])) return 0;
    return 1;
}

int dg_labrador_check(void *ptr) { dg_job *j=ptr; return verify(j->st,j->wt); }

static int parameters(dg_job *j) {
    if (j->ready) return 1;
    size_t bits=0;
    if (dch_pack_params_gen(j->pp,&bits,j->st,j->zk)) return 0;
    j->ready=1;
    if ((j->pp->pp_pack->zkp != NULL) != j->zk) return 0;
    return j->pp->pp_pack->np<=255 && fap2_wire_maxsize(j->pp)<=MAX_PROOF;
}

int dg_labrador_prove(void *ptr, unsigned char *out, size_t cap, size_t *len) {
    dg_job *j=ptr;
    if (!dg_labrador_check(ptr)) return -1;
    if (!parameters(j)) return -2;
    dch_pack_proof pi;
    dch_pack_prove(pi,j->st,j->wt,j->pp);
    int ok=dch_pack_verify(j->st,j->pp,pi) ? 1 : -3;
    if (ok == 1) {
        int encoded=fap2_encode(out,cap,len,pi,j->pp);
        if (encoded) ok=-100+encoded;
    }
    dch_pack_proof_free(pi);
    return ok;
}

int dg_labrador_verify(void *ptr, const unsigned char *in, size_t len) {
    dg_job *j=ptr;
    if(len<12 || len>MAX_PROOF || memcmp(in,"FAP2",4) || !parameters(j) || len>fap2_wire_maxsize(j->pp)) return 0;
    dch_pack_proof pi;
    if(fap2_decode(pi,in,len,j->pp)) return 0;
    int ok=dch_pack_verify(j->st,j->pp,pi);
    dch_pack_proof_free(pi);
    return ok;
}
