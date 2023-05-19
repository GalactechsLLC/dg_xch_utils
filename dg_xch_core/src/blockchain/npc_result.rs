use crate::blockchain::npc::NPC;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub clvm_cost: u64,
    pub npc_list: Vec<NPC>,
}
