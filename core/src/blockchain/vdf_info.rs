use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfInfo {
    pub challenge: Bytes32,
    pub number_of_iterations: u64,
    pub output: ClassgroupElement,
}
