use crate::blockchain::sized_bytes::Bytes32;
use crate::utils::hash_256;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Announcement {
    pub origin_info: Bytes32,
    pub message: Vec<u8>,
    pub morph_bytes: Option<Vec<u8>>,
}
impl Announcement {
    #[must_use]
    pub fn name(&self) -> Bytes32 {
        let mut buf = vec![];
        buf.extend(self.origin_info);
        match &self.morph_bytes {
            Some(m) => {
                let mut morph_buf = vec![];
                morph_buf.extend(m);
                morph_buf.extend(&self.message);
                buf.extend(hash_256(morph_buf));
            }
            None => buf.extend(&self.message),
        };
        hash_256(buf).into()
    }
}
