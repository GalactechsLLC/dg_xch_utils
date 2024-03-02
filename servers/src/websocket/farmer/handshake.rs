use crate::version;
use crate::websocket::farmer::FarmerServerConfig;
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::protocols::harvester::HarvesterHandshake;
use dg_xch_core::protocols::shared::{Handshake, CAPABILITIES};
use dg_xch_core::protocols::{
    ChiaMessage, MessageHandler, NodeType, PeerMap, ProtocolMessageTypes,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hyper_tungstenite::tungstenite::Message;
use log::{debug, info};
use std::collections::HashMap;
use std::io::{Cursor, Error};
use std::str::FromStr;
use std::sync::Arc;

pub struct HandshakeHandle {
    pub config: Arc<FarmerServerConfig>,
    pub farmer_private_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub pool_public_keys: Arc<HashMap<Bytes48, SecretKey>>,
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
        let peer = peers.read().await.get(&peer_id).cloned();
        let protocol_version = if let Some(peer) = peer.as_ref() {
            *peer.protocol_version.read().await
        } else {
            ChiaProtocolVersion::default()
        };
        let handshake = Handshake::from_bytes(&mut cursor, protocol_version)?;
        debug!("New Peer: {}", &peer_id);
        if let Some(peer) = peers.read().await.get(&peer_id).cloned() {
            let (network_id, server_port) = {
                let cfg = self.config.clone();
                (cfg.network.clone(), cfg.websocket.port)
            };
            *peer.node_type.write().await = NodeType::from(handshake.node_type);
            let protocol_version = ChiaProtocolVersion::from_str(&handshake.protocol_version)
                .expect("ChiaProtocolVersion::from_str is Infallible");
            *peer.protocol_version.write().await = protocol_version;
            peer.websocket
                .lock()
                .await
                .send(Message::Binary(
                    ChiaMessage::new(
                        ProtocolMessageTypes::Handshake,
                        protocol_version,
                        &Handshake {
                            network_id,
                            //Server Will use version sent by peer
                            protocol_version: protocol_version.to_string(),
                            software_version: version(),
                            server_port,
                            node_type: NodeType::Farmer as u8,
                            capabilities: CAPABILITIES
                                .iter()
                                .map(|e| (e.0, e.1.to_string()))
                                .collect(),
                        },
                        msg.id,
                    )
                    .to_bytes(protocol_version),
                ))
                .await
                .unwrap_or_default();
            if NodeType::Harvester as u8 == handshake.node_type {
                let farmer_public_keys = self.farmer_private_keys.keys().copied().collect();
                let pool_public_keys = self.pool_public_keys.keys().copied().collect();
                info! {"Harvester Connected. Sending Keys: ({:?}n {:?})", &farmer_public_keys, &pool_public_keys}
                peer.websocket
                    .lock()
                    .await
                    .send(Message::Binary(
                        ChiaMessage::new(
                            ProtocolMessageTypes::HarvesterHandshake,
                            protocol_version,
                            &HarvesterHandshake {
                                farmer_public_keys,
                                pool_public_keys,
                            },
                            None,
                        )
                        .to_bytes(protocol_version),
                    ))
                    .await
                    .unwrap_or_default();
            }
        }
        Ok(())
    }
}
