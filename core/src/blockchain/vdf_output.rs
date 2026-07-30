use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::sized_bytes::Bytes100;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfOutput {
    pub data: UnsizedBytes,
}

// A VDF input/output is a `ClassgroupElement` (fixed 100-byte `Bytes100`); `VdfOutput` is the
// variable-length (`UnsizedBytes`) carrier used inside `BlockRecord`. In practice the carrier always
// holds exactly the 100 bytes of the element, so these conversions round-trip losslessly.
impl From<ClassgroupElement> for VdfOutput {
    fn from(value: ClassgroupElement) -> Self {
        VdfOutput {
            data: UnsizedBytes::new(AsRef::<[u8]>::as_ref(&value.data).to_vec()),
        }
    }
}

impl From<&VdfOutput> for ClassgroupElement {
    fn from(value: &VdfOutput) -> Self {
        ClassgroupElement {
            data: Bytes100::from(value.data.as_slice().to_vec()),
        }
    }
}
