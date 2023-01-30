use crate::types::blockchain::npc::NPC;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub clvm_cost: u64,
    pub npc_list: Vec<NPC>,
}
