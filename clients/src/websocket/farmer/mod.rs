use crate::websocket::farmer::request_signed_values::RequestSignedValuesHandle;
use crate::websocket::farmer::signage_point::NewSignagePointHandle;
use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::consensus::constants::{ConsensusConstants, CONSENSUS_CONSTANTS_MAP, MAINNET};
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::farmer::FarmerSharedState;
use dg_xch_core::protocols::{
    ChiaMessageFilter, ChiaMessageHandler, NodeType, ProtocolMessageTypes,
};
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub mod request_signed_values;
pub mod signage_point;

pub struct FarmerClient<T> {
    pub client: WsClient,
    pub shared_state: Arc<FarmerSharedState<T>>,
}
impl<T> FarmerClient<T> {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        shared_state: Arc<FarmerSharedState<T>>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let constants = CONSENSUS_CONSTANTS_MAP
            .get(&client_config.network_id)
            .cloned()
            .unwrap_or(MAINNET.clone());
        let handles = Arc::new(RwLock::new(handles(constants, &shared_state)));
        let client = WsClient::with_ca(
            client_config,
            NodeType::Farmer,
            handles,
            run,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
        )
        .await?;
        *shared_state.upstream_handshake.write().await = client.handshake.clone();
        Ok(FarmerClient {
            client,
            shared_state,
        })
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

fn handles<T>(
    constants: Arc<ConsensusConstants>,
    shared_state: &FarmerSharedState<T>,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from([
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
                    id: None,
                    custom_fn: None,
                }),
                Arc::new(NewSignagePointHandle {
                    constants,
                    harvester_peers: shared_state.harvester_peers.clone(),
                    signage_points: shared_state.signage_points.clone(),
                    pool_state: shared_state.pool_states.clone(),
                    cache_time: shared_state.cache_time.clone(),
                    running_state: shared_state.running_state.clone(),
                    most_recent_sp: shared_state.most_recent_sp.clone(),
                    #[cfg(feature = "metrics")]
                    metrics: shared_state.metrics.clone(),
                }),
            )),
        ),
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::RequestSignedValues),
                    id: None,
                    custom_fn: None,
                }),
                Arc::new(RequestSignedValuesHandle {
                    quality_to_identifiers: shared_state.quality_to_identifiers.clone(),
                    recent_errors: Arc::default(),
                    harvester_peers: shared_state.harvester_peers.clone(),
                }),
            )),
        ),
    ])
}
