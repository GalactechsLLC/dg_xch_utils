use crate::blockchain::condition_opcode::ConditionOpcode;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ConditionWithArgs {
    pub opcode: ConditionOpcode,
    pub vars: Vec<Vec<u8>>,
}
