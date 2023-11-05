use crate::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfProof {
    pub witness_type: u8,
    pub witness: UnsizedBytes,
    pub normalized_to_identity: bool,
}
