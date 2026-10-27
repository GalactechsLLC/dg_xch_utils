#ifndef REJECTION_H
#define REJECTION_H
#include "aesctr.h"
#include "poly.h"

int is_rejected_std0(aes128ctr_ctx *state, int64_t zv, int64_t vv,
                        long double var, long double m);
int is_rejected_std1(aes128ctr_ctx *state, poly *z, poly *v, size_t len,
                        long double var, long double m);

int is_rejected_sgnleak0(aes128ctr_ctx *state, int64_t zv, int64_t vv,
                            long double var, long double m);

int is_rejected_bimodal0(aes128ctr_ctx *state, int64_t zv, int64_t vv,
                            long double var, long double m);
int is_rejected_bimodal1(aes128ctr_ctx *state, poly *z, poly *v, size_t len,
                            long double var, long double m);

#endif
