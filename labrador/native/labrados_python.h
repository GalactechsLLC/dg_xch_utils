#ifndef LABRADOS_H
#define LABRADOS_H

#include "proofsystem.h"
#include "dachshund.h"

#define py_init_witness NAMESPACE(py_init_witness)
__attribute__((visibility("default")))
void py_init_witness(witness wt, size_t r, size_t n[]);

#define py_print_witness_vector NAMESPACE(py_print_witness_vector)
__attribute__((visibility("default")))
int py_print_witness_vector(witness wt, size_t idx);

#define py_set_witness_vector NAMESPACE(py_set_witness_vector)
__attribute__((visibility("default")))
int py_set_witness_vector(witness wt, size_t idx, size_t n, size_t deg, 
                          const int64_t s[]);

#define py_init_statement NAMESPACE(py_init_statement)
__attribute__((visibility("default")))
void py_init_statement(statement st, size_t r, size_t n[], uint64_t normsq[], 
                       uint64_t normsq_req[], normtype normty[], 
                       size_t num_rq_cnst, size_t num_zq_cnst, 
                       size_t num_int_cnst);

#define py_append_constraint NAMESPACE(py_append_constraint)
__attribute__((visibility("default")))
int py_append_constraint(statement st, size_t nvec, const size_t idx[],
                         const size_t n[], size_t deg, int64_t *phi, int64_t *b,
                         int full);

#define py_append_quadratic NAMESPACE(py_append_quadratic)
__attribute__((visibility("default")))
int py_append_quadratic(statement st, size_t nlin, size_t nprod,
                        const size_t idx_lin[], const size_t idx_prod1[],
                        const size_t idx_prod2[], const size_t len_phi[],
                        size_t deg, int64_t *a, int64_t *phi, int64_t *b);

#define py_append_deg0_constraint NAMESPACE(py_append_deg0_constraint)
__attribute__((visibility("default")))
int py_append_deg0_constraint(statement st, size_t idx, size_t deg);

#define py_gen_params NAMESPACE(py_gen_params)
__attribute__((visibility("default")))
int py_gen_params(dch_pack_params pp, const statement st, int zk, int debug);

#define py_simple_verify NAMESPACE(py_simple_verify)
__attribute__((visibility("default")))
int py_simple_verify(const statement st, const witness wt);

#define py_prove NAMESPACE(py_prove)
__attribute__((visibility("default")))
void py_prove(dch_pack_proof pi, const statement ist, 
              const witness iwt, const dch_pack_params pp);

#define py_verify NAMESPACE(py_verify)
__attribute__((visibility("default")))
int py_verify(const statement ist, const dch_pack_params pp, 
              const dch_pack_proof pi);

#define py_free_witness NAMESPACE(py_free_witness)
__attribute__((visibility("default")))
void py_free_witness(witness wt);

#define py_free_statement NAMESPACE(py_free_statement)
__attribute__((visibility("default")))
void py_free_statement(statement st);

#define py_free_params NAMESPACE(py_free_params)
__attribute__((visibility("default")))
void py_free_params(dch_pack_params pp);

#define py_free_proof NAMESPACE(py_free_proof)
__attribute__((visibility("default")))
void py_free_proof(dch_pack_proof pi);

#endif