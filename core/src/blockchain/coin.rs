use crate::blockchain::sized_bytes::Bytes32;
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
        hash_256(self.to_bytes())
    }
}
