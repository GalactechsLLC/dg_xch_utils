use crate::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::hash_256;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

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
        buf.extend(self.origin_info.as_slice());
        let msg = match &self.morph_bytes {
            Some(m) => {
                let mut morph_buf = vec![];
                morph_buf.extend(m);
                morph_buf.extend(&self.message);
                Cow::Owned(hash_256(morph_buf))
            }
            None => Cow::Borrowed(&self.message),
        };
        buf.extend(msg.as_ref());
        Bytes32::new(&hash_256(buf))
    }
}
