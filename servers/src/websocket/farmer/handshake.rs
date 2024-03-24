use crate::version;
use crate::websocket::farmer::FarmerServerConfig;
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::protocols::harvester::HarvesterHandshake;
use dg_xch_core::protocols::shared::{Handshake, CAPABILITIES, PROTOCOL_VERSION};
use dg_xch_core::protocols::{
    ChiaMessage, MessageHandler, NodeType, PeerMap, ProtocolMessageTypes,
};
use dg_xch_serialize::ChiaSerialize;
use hyper_tungstenite::tungstenite::Message;
use log::{debug, info};
use std::collections::HashMap;
use std::io::{Cursor, Error};
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
        let handshake = Handshake::from_bytes(&mut cursor)?;
        debug!("New Peer: {}", &peer_id);
        if let Some(peer) = peers.read().await.get(&peer_id).cloned() {
            let (network_id, server_port) = {
                let cfg = self.config.clone();
                (cfg.network.clone(), cfg.websocket.port)
            };
            *peer.node_type.write().await = NodeType::from(handshake.node_type);
            peer.websocket
                .write()
                .await
                .send(Message::Binary(
                    ChiaMessage::new(
                        ProtocolMessageTypes::Handshake,
                        &Handshake {
                            network_id,
                            protocol_version: PROTOCOL_VERSION.to_string(),
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
                    .to_bytes(),
                ))
                .await
                .unwrap_or_default();
            if NodeType::Harvester as u8 == handshake.node_type {
                let farmer_public_keys = self.farmer_private_keys.keys().copied().collect();
                let pool_public_keys = self.pool_public_keys.keys().copied().collect();
                info! {"Harvester Connected. Sending Keys: ({:?}n {:?})", &farmer_public_keys, &pool_public_keys}
                peer.websocket
                    .write()
                    .await
                    .send(Message::Binary(
                        ChiaMessage::new(
                            ProtocolMessageTypes::HarvesterHandshake,
                            &HarvesterHandshake {
                                farmer_public_keys,
                                pool_public_keys,
                            },
                            None,
                        )
                        .to_bytes(),
                    ))
                    .await
                    .unwrap_or_default();
            }
        }
        Ok(())
    }
}
