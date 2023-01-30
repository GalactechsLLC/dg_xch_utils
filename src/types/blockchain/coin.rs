use crate::clvm::utils::hash_256;
use crate::types::blockchain::sized_bytes::u64_to_bytes;
use crate::types::blockchain::sized_bytes::{Bytes32, SizedBytes};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Coin {
    pub amount: u64,
    pub parent_coin_info: Bytes32,
    pub puzzle_hash: Bytes32,
}
impl Coin {
    pub fn name(&self) -> Bytes32 {
        self.hash().into()
    }

    pub fn hash(&self) -> Vec<u8> {
        let mut to_hash: Vec<u8> = Vec::new();
        to_hash.extend(&self.parent_coin_info.to_bytes());
        to_hash.extend(&self.puzzle_hash.to_bytes());
        to_hash.extend(u64_to_bytes(self.amount));
        hash_256(&to_hash)
    }
}
