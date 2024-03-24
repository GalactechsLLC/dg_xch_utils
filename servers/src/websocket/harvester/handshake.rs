use crate::version;
use crate::websocket::harvester::HarvesterServerConfig;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::shared::{Handshake, CAPABILITIES, PROTOCOL_VERSION};
use dg_xch_core::protocols::{
    ChiaMessage, MessageHandler, NodeType, PeerMap, ProtocolMessageTypes,
};
use dg_xch_serialize::ChiaSerialize;
use hyper_tungstenite::tungstenite::Message;
use std::io::{Cursor, Error, ErrorKind};
use std::sync::Arc;

pub struct HandshakeHandle {
    pub config: Arc<HarvesterServerConfig>,
}
#[async_trait]
impl MessageHandler for HandshakeHandle {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(&msg.data);
        let handshake = Handshake::from_bytes(&mut cursor)?;
        if let Some(peer) = peers.read().await.get(&peer_id).cloned() {
            *peer.node_type.write().await = NodeType::from(handshake.node_type);
            peer.websocket
                .write()
                .await
                .send(Message::Binary(
                    ChiaMessage::new(
                        ProtocolMessageTypes::Handshake,
                        &Handshake {
                            network_id: self.config.network.clone(),
                            protocol_version: PROTOCOL_VERSION.to_string(),
                            software_version: version(),
                            server_port: self.config.websocket.port,
                            node_type: NodeType::Harvester as u8,
                            capabilities: CAPABILITIES
                                .iter()
                                .map(|e| (e.0, e.1.to_string()))
                                .collect(),
                        },
                        msg.id,
                    )
                    .to_bytes(),
                ))
                .await
        } else {
            Err(Error::new(ErrorKind::NotFound, "Failed to find peer"))
        }
    }
}
