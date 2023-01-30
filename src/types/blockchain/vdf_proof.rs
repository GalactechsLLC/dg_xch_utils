use crate::types::blockchain::sized_bytes::UnsizedBytes;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VdfProof {
    pub normalized_to_identity: bool,
    pub witness: UnsizedBytes,
    pub witness_type: u8,
}
