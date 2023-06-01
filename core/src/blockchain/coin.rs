use crate::blockchain::sized_bytes::{Bytes32, u64_to_bytes};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{hash_256, ChiaSerialize};
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Coin {
    pub parent_coin_info: Bytes32,
    pub puzzle_hash: Bytes32,
    pub amount: u64,
}
impl Coin {
    pub fn name(&self) -> Bytes32 {
        self.hash().into()
    }
    pub fn hash(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut to_hash: Vec<u8> = Vec::new();
        to_hash.extend(&self.parent_coin_info.to_bytes());
        to_hash.extend(&self.puzzle_hash.to_bytes());
        to_hash.extend(u64_to_bytes(self.amount));
        hash_256(&to_hash)
    }
}
