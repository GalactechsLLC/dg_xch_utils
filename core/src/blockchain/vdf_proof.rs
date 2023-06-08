use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfProof {
    pub normalized_to_identity: bool,
    pub witness: Vec<u8>,
    pub witness_type: u8,
}
