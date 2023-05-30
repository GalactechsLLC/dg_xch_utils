use crate::blockchain::sized_bytes::UnsizedBytes;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfOutput {
    pub data: UnsizedBytes,
}
