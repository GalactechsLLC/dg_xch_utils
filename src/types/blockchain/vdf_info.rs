use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::vdf_output::VdfOutput;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VdfInfo {
    pub challenge: Bytes32,
    pub output: VdfOutput,
    pub number_of_iterations: u64,
}
