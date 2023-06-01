use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::vdf_output::VdfOutput;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfInfo {
    pub challenge: Bytes32,
    pub output: VdfOutput,
    pub number_of_iterations: u64,
}
