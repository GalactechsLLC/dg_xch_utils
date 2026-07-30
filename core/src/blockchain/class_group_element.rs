use crate::blockchain::sized_bytes::Bytes100;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ClassgroupElement {
    pub data: Bytes100,
}

impl ClassgroupElement {
    /// The default (identity) classgroup element — chia's `ClassgroupElement.get_default_element()`.
    ///
    /// The compressed IBQF identity form is the 100-byte value whose first byte is `0x08` (bit 3 flags
    /// the default generator) and whose remaining 99 bytes are zero.
    ///
    /// NOTE: this is deliberately NOT wired to `Default::default()`. A derived/Rust `Default` for
    /// `ClassgroupElement` would be all-zeros (`[0u8; 100]`), which is a *different* value and the wrong
    /// VDF identity — using it would break VDF verification. This mirrors chia_rs, where the streamable
    /// `default()` (all-zeros) and `get_default_element()` (the `0x08` identity) are distinct.
    #[must_use]
    pub fn get_default_element() -> Self {
        let mut bytes = [0u8; 100];
        bytes[0] = 0x08;
        Self {
            data: Bytes100::from(bytes),
        }
    }
}
