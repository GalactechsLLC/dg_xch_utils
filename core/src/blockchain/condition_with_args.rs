use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::clvm::sexp::{AtomBuf, IntoSExp, SExp};
use crate::constants::NULL_SEXP;
use dg_xch_macros::ChiaSerial;
use log::warn;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ConditionWithArgs {
    pub opcode: ConditionOpcode,
    pub vars: Vec<Vec<u8>>,
}
impl TryFrom<&SExp> for ConditionWithArgs {
    type Error = Error;
    fn try_from(sexp: &SExp) -> Result<Self, Self::Error> {
        let mut opcode = ConditionOpcode::Unknown;
        let mut vars = vec![];
        let mut first = true;
        for arg in sexp.iter().take(4) {
            match arg {
                SExp::Atom(arg) => {
                    if first {
                        first = false;
                        if arg.data.len() != 1 {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                "Invalid OpCode for Condition",
                            ));
                        }
                        opcode = ConditionOpcode::from(arg.data[0]);
                    } else {
                        vars.push(arg.data.clone());
                    }
                }
                SExp::Pair(_) => {
                    warn!("Got pair in opcode args");
                    break;
                }
            }
        }
        if vars.is_empty() {
            Err(Error::new(
                ErrorKind::InvalidData,
                "Invalid Condition No Vars",
            ))
        } else {
            Ok(ConditionWithArgs { opcode, vars })
        }
    }
}

impl TryFrom<&SExp> for Vec<ConditionWithArgs> {
    type Error = Error;
    fn try_from(sexp: &SExp) -> Result<Self, Self::Error> {
        let mut results = Vec::new();
        for arg in sexp.iter() {
            let arg: Result<ConditionWithArgs, Error> = arg.try_into();
            match arg {
                Ok(condition) => {
                    results.push(condition);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(results)
    }
}

impl IntoSExp for &ConditionWithArgs {
    fn to_sexp(self) -> SExp {
        let mut vars = vec![];
        for var in self.vars.iter().rev() {
            vars.push(SExp::Atom(AtomBuf::from(var)));
        }
        let mut args_pair = NULL_SEXP.clone();
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
