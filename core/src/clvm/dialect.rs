use crate::clvm::sexp::SExp;
use std::io::{Error, ErrorKind};
pub trait Dialect {
    fn quote_kw(&self) -> &[u8];
    fn apply_kw(&self) -> &[u8];
    fn print_kw(&self) -> &[u8];
    fn op(&self, op: SExp, args: SExp, max_cost: u64) -> Result<(u64, SExp), Error>;
}
use crate::clvm::core_ops::{op_cons, op_eq, op_first, op_if, op_listp, op_raise, op_rest};
use crate::clvm::more_ops::{
    op_add, op_all, op_any, op_ash, op_coinid, op_concat, op_div, op_div_deprecated, op_divmod,
    op_gr, op_gr_bytes, op_logand, op_logior, op_lognot, op_logxor, op_lsh, op_multiply, op_not,
    op_point_add, op_pubkey_for_exp, op_sha256, op_softfork, op_strlen, op_substr, op_subtract,
    op_unknown,
};

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
    pub fn new(flags: u32) -> ChiaDialect {
        ChiaDialect { flags }
    }
}

impl Dialect for ChiaDialect {
    fn op(&self, o: SExp, argument_list: SExp, max_cost: u64) -> Result<(u64, SExp), Error> {
        match &o {
            SExp::Atom(buf) => {
                let b = &buf.data;
                if b.len() != 1 {
                    return if (self.flags & NO_UNKNOWN_OPS) != 0 {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("unimplemented operator: {o:?}"),
                        ));
                    } else {
                        op_unknown(&o, &argument_list, max_cost, self)
                    };
                }
                match b.first() {
                    Some(v) => {
                        let f = match *v {
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
                            36 => op_softfork,
                            48 => op_coinid,
                            // 49 => op_bls_g1_subtract,
                            // 50 => op_bls_g1_multiply,
                            // 51 => op_bls_g1_negate,
                            // 52 => op_bls_g2_add,
                            // 53 => op_bls_g2_subtract,
                            // 54 => op_bls_g2_multiply,
                            // 55 => op_bls_g2_negate,
                            // 56 => op_bls_map_to_g1,
                            // 57 => op_bls_map_to_g2,
                            // 58 => op_bls_pairing_identity,
                            // 59 => op_bls_verify,
                            // 60 => op_modpow,
                            // 61 => op_mod,
                            _ => {
                                return if (self.flags & NO_UNKNOWN_OPS) != 0 {
                                    Err(Error::new(
                                        ErrorKind::InvalidData,
                                        format!("unimplemented operator: {o:?}"),
                                    ))
                                } else {
                                    op_unknown(&o, &argument_list, max_cost, self)
                                };
                            }
                        };
                        f(&argument_list, max_cost, self)
                    }
                    None => Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("no operator found: {o:?}"),
                    )),
                }
            }
            SExp::Pair(_) => Err(Error::new(
                ErrorKind::InvalidData,
                format!("Expected Atom, got Pair: {o:?}"),
            )),
        }
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
}
