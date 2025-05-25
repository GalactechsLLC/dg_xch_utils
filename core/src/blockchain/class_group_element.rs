use crate::blockchain::sized_bytes::Bytes100;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ClassgroupElement {
    pub data: Bytes100,
}
