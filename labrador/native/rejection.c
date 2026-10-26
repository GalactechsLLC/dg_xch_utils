#include "rejection.h"
#include <limits.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>

// assume in the following that long double can represent 64 bit integers

static inline uint64_t _bytes2uint63(uint8_t bytes[8])
{
    uint64_t ret = 0;

    ret |= (uint64_t)bytes[0] | ((uint64_t)bytes[1] << 8)
        | ((uint64_t)bytes[2] << 16) | ((uint64_t)bytes[3] << 24)
        | ((uint64_t)bytes[4] << 32) | ((uint64_t)bytes[5] << 40)
        | ((uint64_t)bytes[6] << 48) | ((uint64_t)(bytes[7] & 0x7f) << 56);
    return ret;
}

// return random long double in [0,1)
static inline long double _ldrand(aes128ctr_ctx *state) {
    const uint64_t denom = (uint64_t)1 << 63;
    uint64_t nom;
    uint8_t buf[AES128CTR_BLOCKBYTES];
    long double ret;

    aes128ctr_squeezeblocks(buf, 1, state);
    nom = _bytes2uint63(buf);
    ret = (long double)nom / denom;
    return ret;
}

// standard rejection sampling
// zv  : <z,v>
// vv  : <v,v>
// var : variance
// m   : repetition rate M
int is_rejected_std0(aes128ctr_ctx *state, int64_t zv, int64_t vv,
                        long double var, long double m) {
    // int64_t nom;
    long double u;
    int rejected = 1;

    // nom = -2 * zv + vv;

    u = _ldrand(state);
    if (!(u * m > expl((-2 * zv + vv) / (2 * var)))) {
        rejected = 0;
    }
    //printf ("zv int %ld\n", zv);
    //printf ("zv ld %Lf\n", (long double)zv);
    //printf ("vv int %ld\n", vv);
    //printf ("vv ld %Lf\n", (long double)vv);
    //printf ("-2zv+vv %Lf\n", (-2 * (long double)zv + (long double)vv));
    //printf ("2 * var %Lf\n", 2*var);
    //printf ("%Lf\n", (long double)(-2 * zv + vv) / (2 * var));
    //printf ("%Lf <= %Lf\n", u * m, expl((-2 * zv + vv) / (2 * var)));

    return rejected;
}

// standard rejection sampling
// zv  : <z,v>
// vv  : <v,v>
// var : variance
// m   : repetition rate M
int is_rejected_sgnleak0(aes128ctr_ctx *state, int64_t zv, int64_t vv,
                          long double var, long double m) {
    int rejected = 1;

    if (zv < 0)
        goto ret;

    rejected = is_rejected_std0(state, zv, vv, var, m);
ret:
    return rejected;
}

// bimodal rejection sampling
// zv  : <z,v>
// vv  : <v,v>
// var : variance
// m   : repetition rate M
int is_rejected_bimodal0(aes128ctr_ctx *state, int64_t zv, int64_t vv,
                            long double var, long double m)
{
    long double u;
    int rejected;

    u = _ldrand(state);
    if (u * m * expl(-vv / (2 * var)) * coshl(zv / var) > 1)
        rejected = 1;
    else
        rejected = 0;

    return rejected;
}

// standard rejection sampling
// var : variance
// m   : repetition rate M
int is_rejected_std1(aes128ctr_ctx *state, poly *z, poly *v, size_t len,
                        long double var, long double m) {
    int64_t zv, vv;

    zv = polyvec_sprodz(z, v, 1, 1, len);
    vv = polyvec_sprodz(v, v, 1, 1, len);
    return is_rejected_std0(state, zv, vv, var, m);
}

// binomial rejection sampling
// var : variance
// m   : repetition rate M
int is_rejected_bimodal1(aes128ctr_ctx *state, poly *z, poly *v, size_t len,
                            long double var, long double m) {
    int64_t zv, vv;

    zv = polyvec_sprodz(z, v, 1, 1, len);
    vv = polyvec_sprodz(v, v, 1, 1, len);
    return is_rejected_bimodal0(state, zv, vv, var, m);
}