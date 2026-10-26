use crate::clvm::arena::{Arena, NodePtr};
use crate::clvm::pure_ops::OpOut;

pub trait Dialect {
    fn quote_kw(&self) -> &[u8];
    fn apply_kw(&self) -> &[u8];
    fn print_kw(&self) -> &[u8];
    // The active CLVM flag set — operators with flag-dependent behavior (size limits, cost-model
    // selection) read it.
    fn flags(&self) -> u32;
    /// Run an operator and return its DESCRIBED result. Materialization is the caller's, so the
    /// runtime can decide to reclaim the operand evaluation first.
    fn op(
        &self,
        arena: &Arena,
        op: NodePtr,
        args: NodePtr,
        max_cost: u64,
    ) -> Result<(u64, OpOut), ClvmError>;
}
use crate::clvm::bls_ops::{
    op_bls_g1_multiply, op_bls_g1_negate, op_bls_g1_subtract, op_bls_g2_add, op_bls_g2_multiply,
    op_bls_g2_negate, op_bls_g2_subtract, op_bls_map_to_g1, op_bls_map_to_g2,
    op_bls_pairing_identity, op_bls_verify,
};
use crate::clvm::core_ops::{op_cons, op_eq, op_first, op_if, op_listp, op_raise, op_rest};
use crate::clvm::keccak_ops::op_keccak256;
use crate::clvm::more_ops::{
    op_add, op_all, op_any, op_ash, op_coinid, op_concat, op_div, op_div_deprecated, op_divmod,
    op_gr, op_gr_bytes, op_logand, op_logior, op_lognot, op_logxor, op_lsh, op_mod, op_modpow,
    op_multiply, op_not, op_point_add, op_pubkey_for_exp, op_sha256, op_strlen, op_substr,
    op_subtract, op_unknown,
};
use crate::clvm::secp_ops::op_secp_verify;
use crate::clvm::utils::{DISABLE_OP, ENABLE_KECCAK_OPS_OUTSIDE_FORK, NEW_COST_MODEL};
use crate::errors::ClvmError;

// division with negative numbers are disallowed
pub const NO_NEG_DIV: u32 = 0x0001;

// unknown operators are disallowed
// (otherwise they are no-ops with well defined cost)
pub const NO_UNKNOWN_OPS: u32 = 0x0002;

pub struct ChiaDialect {
    flags: u32,
}

impl ChiaDialect {
    #[must_use]
    pub const fn new(flags: u32) -> ChiaDialect {
        ChiaDialect { flags }
    }
}
type OpFn = fn(&Arena, NodePtr, u64, &ChiaDialect) -> Result<(u64, OpOut), ClvmError>;
impl Dialect for ChiaDialect {
    fn op(
        &self,
        arena: &Arena,
        o: NodePtr,
        argument_list: NodePtr,
        max_cost: u64,
    ) -> Result<(u64, OpOut), ClvmError> {
        // Single-byte opcode, or None for multi-byte / empty (the op_unknown path).
        let opcode: Option<u8> = match arena.atom(o) {
            Some(atom) => {
                let b = atom.as_ref();
                match b {
                    [0x13, 0xd6, 0x1f, 0x00] => {
                        return op_secp_verify(arena, argument_list, max_cost, false);
                    }
                    [0x1c, 0x3a, 0x8f, 0x00] => {
                        return op_secp_verify(arena, argument_list, max_cost, true);
                    }
                    [v] => Some(*v),
                    _ => None,
                }
            }
            None => return Err(ClvmError::ExpectedAtomGotPair(arena.display(o))),
        };
        let Some(v) = opcode else {
            return if (self.flags & NO_UNKNOWN_OPS) != 0 {
                let b0 = arena
                    .atom(o)
                    .and_then(|a| a.as_ref().first().copied())
                    .unwrap_or(0);
                Err(ClvmError::Unimplemented(b0))
            } else {
                op_unknown(arena, o, argument_list, max_cost, self)
            };
        };
        let f: OpFn = match v {
            3 => op_if,
            4 => op_cons,
            5 => op_first,
            6 => op_rest,
            7 => op_listp,
            8 => op_raise,
            9 => op_eq,
            10 => op_gr_bytes,
            11 => op_sha256,
            12 => op_substr,
            13 => op_strlen,
            14 => op_concat,
            // 15 - Not Used
            16 => op_add,
            17 => op_subtract,
            18 => op_multiply,
            19 => {
                if (self.flags & NO_NEG_DIV) != 0 {
                    op_div_deprecated
                } else {
                    op_div
                }
            }
            20 => op_divmod,
            21 => op_gr,
            22 => op_ash,
            23 => op_lsh,
            24 => op_logand,
            25 => op_logior,
            26 => op_logxor,
            27 => op_lognot,
            // 28 - Not Used
            29 => op_point_add,
            30 => op_pubkey_for_exp,
            // 31 - Not Used
            32 => op_not,
            33 => op_any,
            34 => op_all,
            // 35 - Not Used
            // 36 (softfork) is handled by the evaluator, like quote and apply.
            48 => op_coinid,
            // 49..=59 — the CLVM BLS operators, dispatched unconditionally in the base
            // dialect: the 2.0 hard fork moved the BLS extension outside the softfork guard.
            49 => op_bls_g1_subtract,
            50 => op_bls_g1_multiply,
            51 => op_bls_g1_negate,
            52 => op_bls_g2_add,
            53 => op_bls_g2_subtract,
            54 => op_bls_g2_multiply,
            55 => op_bls_g2_negate,
            56 => op_bls_map_to_g1,
            57 => op_bls_map_to_g2,
            58 => op_bls_pairing_identity,
            59 => op_bls_verify,
            60 => {
                // DISABLE_OP disables modpow unless the new cost model bounds it
                // (forked out by soft fork 8, re-enabled at hard fork 2).
                if (self.flags & DISABLE_OP) != 0 && (self.flags & NEW_COST_MODEL) == 0 {
                    return Err(ClvmError::Unimplemented(v));
                }
                op_modpow
            }
            61 => op_mod,
            62 if (self.flags & ENABLE_KECCAK_OPS_OUTSIDE_FORK) != 0 => op_keccak256,
            _ => {
                return if (self.flags & NO_UNKNOWN_OPS) != 0 {
                    Err(ClvmError::Unimplemented(v))
                } else {
                    op_unknown(arena, o, argument_list, max_cost, self)
                };
            }
        };
        f(arena, argument_list, max_cost, self)
    }

    fn quote_kw(&self) -> &[u8] {
        &[1]
    }

    fn apply_kw(&self) -> &[u8] {
        &[2]
    }
    fn print_kw(&self) -> &[u8] {
        b"$print$"
    }
    fn flags(&self) -> u32 {
        self.flags
    }
}
