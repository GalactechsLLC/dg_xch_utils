use crate::clvm::utils::hash_256;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::sized_bytes::SizedBytes;
use serde::{Deserialize, Serialize};

#[derive(Hash, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Announcement {
    pub origin_info: Bytes32,
    pub message: Vec<u8>,
}
impl Announcement {
    pub fn name(&self) -> Bytes32 {
        self.hash().into()
    }

    pub fn hash(&self) -> Vec<u8> {
        let mut to_hash: Vec<u8> = Vec::new();
        to_hash.extend(&self.origin_info.to_bytes());
        to_hash.extend(&self.message);
        hash_256(&to_hash)
    }
}
