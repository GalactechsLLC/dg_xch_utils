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
        let mut args_pair = None;
        for var in vars.into_iter().rev() {
            match args_pair {
                None => args_pair = Some(var),
                Some(pair) => args_pair = Some(pair.cons(var)),
            }
        }
        self.opcode.to_sexp().cons(args_pair.unwrap_or_else(|| NULL.clone()))
    }
}
impl IntoSExp for ConditionWithArgs {
    fn to_sexp(self) -> SExp {
        (&self).to_sexp()
    }
}
