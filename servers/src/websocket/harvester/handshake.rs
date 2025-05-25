use crate::version;
use crate::websocket::harvester::HarvesterServerConfig;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::shared::{Handshake, CAPABILITIES};
use dg_xch_core::protocols::{
    ChiaMessage, MessageHandler, NodeType, PeerMap, ProtocolMessageTypes,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hyper_tungstenite::tungstenite::Message;
use std::io::{Cursor, Error, ErrorKind};
use std::str::FromStr;
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
        if let Some(peer) = peers.read().await.get(&peer_id).cloned() {
            let mut cursor = Cursor::new(&msg.data);
            let handshake =
                Handshake::from_bytes(&mut cursor, *peer.protocol_version.read().await)?;
            *peer.node_type.write().await = NodeType::from(handshake.node_type);
            let protocol_version = ChiaProtocolVersion::from_str(&handshake.protocol_version)
                .expect("ChiaProtocolVersion::from_str is Infallible");
            *peer.protocol_version.write().await = protocol_version;
            peer.websocket
                .write()
                .await
                .send(Message::Binary(
                    ChiaMessage::new(
                        ProtocolMessageTypes::Handshake,
                        protocol_version,
                        &Handshake {
                            network_id: self.config.network.clone(),
                            protocol_version: protocol_version.to_string(),
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
                    .to_bytes(protocol_version)
                    .into(),
                ))
                .await
        } else {
            Err(Error::new(ErrorKind::NotFound, "Failed to find peer"))
        }
    }
}
