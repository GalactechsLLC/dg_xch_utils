use crate::types::blockchain::peer_info::TimestampedPeerInfo;

pub struct RequestPeersIntroducer {}

pub struct RespondPeersIntroducer {
    pub peer_list: Vec<TimestampedPeerInfo>,
}
