use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::clvm::sexp::{IntoSExp, SExp, NULL};
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
        for var in &self.vars {
            vars.push(var.as_slice().to_sexp());
        }
        let mut args_pair = NULL.clone();
        for var in vars.into_iter().rev() {
            args_pair = args_pair.cons(var)
        }
        self.opcode.to_sexp().cons(args_pair)
    }
}
