use crate::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPeersIntroducer {} //Min Version 0.0.34

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPeersIntroducer {
    pub peer_list: Vec<TimestampedPeerInfo>, //Min Version 0.0.34
}
