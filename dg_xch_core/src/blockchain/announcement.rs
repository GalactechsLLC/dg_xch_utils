use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::ChiaSerialize;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Announcement {
    pub origin_info: Bytes32,
    pub message: Vec<u8>,
}
impl Announcement {
    pub fn name(&self) -> Bytes32 {
        self.hash().into()
    }
}
