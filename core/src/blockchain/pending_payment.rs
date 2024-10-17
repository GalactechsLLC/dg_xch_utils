use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PendingPayment {
    pub puzzle_hash: Bytes32,
    pub amount: u64,
}
impl PendingPayment {
    pub fn to_create_coin_condition(&self) -> ConditionWithArgs {
        ConditionWithArgs {
            opcode: ConditionOpcode::CreateCoin,
            vars: vec![
                self.puzzle_hash.to_bytes(ChiaProtocolVersion::default()),
                self.amount.to_bytes(ChiaProtocolVersion::default()),
            ],
        }
    }
}
