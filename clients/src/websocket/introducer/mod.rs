use crate::websocket::{WsClient, WsClientConfig};
use async_trait::async_trait;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::introducer::RespondPeersIntroducer;
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_serialize::ChiaSerialize;
use log::{debug, error, info};
use rustls::crypto::ring::default_provider;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Default)]
pub struct IntroducerState {
    pub peer_list: HashSet<TimestampedPeerInfo>,
}

pub struct IntroducerClient {
    pub client: WsClient,
    pub state: Arc<RwLock<IntroducerState>>,
}
impl IntroducerClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        run: Arc<AtomicBool>,
        state: Arc<RwLock<IntroducerState>>,
        timeout: u64,
    ) -> Result<Self, Error> {
        default_provider().install_default().unwrap_or_default();
        struct IgnoreExtraMessagesHandler {}
        #[async_trait]
        impl MessageHandler for IgnoreExtraMessagesHandler {
            async fn handle(
                &self,
                msg: Arc<ChiaMessage>,
                _peer_id: Arc<Bytes32>,
                _peers: PeerMap,
            ) -> Result<(), Error> {
                debug!("Got websocket Message: {}", msg.msg_type);
                Ok(())
            }
        }
        let handles = Arc::new(RwLock::new(HashMap::from([
            (
                Uuid::new_v4(),
                Arc::new(ChiaMessageHandler {
                    filter: Arc::new(ChiaMessageFilter {
                        msg_type: Some(ProtocolMessageTypes::RespondPeersIntroducer),
                        id: None,
                        custom_fn: None,
                    }),
                    handle: Arc::new(RespondPeersHandler {
                        state: state.clone(),
                    }),
                }),
            ),
            (
                Uuid::new_v4(),
                Arc::new(ChiaMessageHandler {
                    filter: Arc::new(ChiaMessageFilter {
                        msg_type: None,
                        id: None,
                        custom_fn: Some(Box::new(|_msg| true)),
                    }),
                    handle: Arc::new(IgnoreExtraMessagesHandler {}),
                }),
            ),
        ])));
        let client = WsClient::with_ca(
            client_config,
            NodeType::Introducer,
            handles,
            run,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
            timeout,
        )
        .await?;
        Ok(IntroducerClient { client, state })
    }

    pub async fn join(self) -> Result<(), Error> {
        self.client.connection.write().await.shutdown().await?;
        self.client.join().await
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.client.handle.is_finished()
    }
}

pub struct RespondPeersHandler {
    pub state: Arc<RwLock<IntroducerState>>,
}
#[async_trait]
impl MessageHandler for RespondPeersHandler {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        if msg.msg_type != ProtocolMessageTypes::RespondPeersIntroducer {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Respond Peers Handler expects a RespondPeersIntroducer Message got {}",
                    msg.msg_type
                ),
            ))
        } else {
            let mut cursor = Cursor::new(&msg.data);
            match peers.read().await.get(&peer_id) {
                None => {
                    error!("Peer Disconnected before Peer Version could be determined");
                }
                Some(peer) => {
                    let peer_list = RespondPeersIntroducer::from_bytes(
                        &mut cursor,
                        *peer.protocol_version.read().await,
                    )?;
                    info!("Got Response for Peers Request: {peer_list:?}");
                    self.state
                        .write()
                        .await
                        .peer_list
                        .extend(peer_list.peer_list.into_iter());
                }
            }
            Ok(())
        }
    }
}
