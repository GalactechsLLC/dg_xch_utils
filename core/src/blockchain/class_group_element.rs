use crate::blockchain::sized_bytes::Bytes100;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ClassgroupElement {
    pub data: Bytes100,
}

impl ClassgroupElement {
    /// Returns the VDF identity element.
    #[must_use]
    pub fn get_default_element() -> Self {
        let mut bytes = [0u8; 100];
        bytes[0] = 0x08;
        Self {
            data: Bytes100::from(bytes),
        }
    }
}
