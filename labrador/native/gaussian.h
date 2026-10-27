#ifndef GAUSSIAN_H
#define GAUSSIAN_H
#include "aesctr.h"

void
gaussian_i32 (int32_t *ret, unsigned int nelems, aes128ctr_ctx *state,
                        unsigned int log2sd);

#endif
