use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NPC {
    pub coin_name: Bytes32,
    pub puzzle_hash: Bytes32,
    pub conditions: Vec<(u8, Vec<(u8, String)>)>,
}
