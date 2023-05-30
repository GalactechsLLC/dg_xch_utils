use crate::blockchain::sized_bytes::UnsizedBytes;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfProof {
    pub normalized_to_identity: bool,
    pub witness: UnsizedBytes,
    pub witness_type: u8,
}
