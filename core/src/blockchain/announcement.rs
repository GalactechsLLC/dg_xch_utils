use crate::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{hash_256, ChiaSerialize};
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Announcement {
    pub origin_info: Bytes32,
    pub message: Vec<u8>,
}
impl Announcement {
    pub fn name(&self) -> Bytes32 {
        Bytes32::new(&self.hash())
    }
    pub fn hash(&self) -> Vec<u8>
    where
        Self: Sized,
    {
        hash_256(self.to_bytes())
    }
}
