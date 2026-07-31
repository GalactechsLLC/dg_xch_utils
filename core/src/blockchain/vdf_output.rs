use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::sized_bytes::Bytes100;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct VdfOutput {
    pub data: UnsizedBytes,
}

// A VDF input/output is a fixed-size `ClassgroupElement`; `VdfOutput` is its variable-size carrier in
// `BlockRecord`.
impl From<ClassgroupElement> for VdfOutput {
    fn from(value: ClassgroupElement) -> Self {
        VdfOutput {
            data: UnsizedBytes::new(AsRef::<[u8]>::as_ref(&value.data).to_vec()),
        }
    }
}

impl TryFrom<&VdfOutput> for ClassgroupElement {
    type Error = std::io::Error;

    fn try_from(value: &VdfOutput) -> Result<Self, Self::Error> {
        let data: [u8; 100] = value.data.as_slice().try_into().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid VDF output length")
        })?;
        Ok(Self {
            data: Bytes100::from(data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_vdf_output_with_invalid_length() {
        let short = VdfOutput {
            data: UnsizedBytes::new(vec![0; 99]),
        };
        let long = VdfOutput {
            data: UnsizedBytes::new(vec![0; 101]),
        };

        assert!(ClassgroupElement::try_from(&short).is_err());
        assert!(ClassgroupElement::try_from(&long).is_err());
    }
}
