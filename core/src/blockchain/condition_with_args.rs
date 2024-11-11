use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::clvm::sexp::{AtomBuf, IntoSExp, SExp, NULL};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ConditionWithArgs {
    pub opcode: ConditionOpcode,
    pub vars: Vec<Vec<u8>>,
}
impl IntoSExp for &ConditionWithArgs {
    fn to_sexp(self) -> SExp {
        let mut vars = vec![];
        for var in self.vars.iter().rev() {
            vars.push(SExp::Atom(AtomBuf::from(var)));
        }
        let mut args_pair = NULL.clone();
        for var in vars.into_iter() {
            args_pair = var.cons(args_pair)
        }
        self.opcode.to_sexp().cons(args_pair)
    }
}
impl IntoSExp for ConditionWithArgs {
    fn to_sexp(self) -> SExp {
        (&self).to_sexp()
    }
}
