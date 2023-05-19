use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPeersIntroducer {}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPeersIntroducer {
    pub peer_list: Vec<TimestampedPeerInfo>,
}
