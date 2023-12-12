use crate::websocket::farmer::request_signed_values::RequestSignedValuesHandle;
use crate::websocket::farmer::signage_point::NewSignagePointHandle;
use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::consensus::constants::{ConsensusConstants, CONSENSUS_CONSTANTS_MAP, MAINNET};
use dg_xch_core::protocols::farmer::FarmerSharedState;
use dg_xch_core::protocols::{
    ChiaMessageFilter, ChiaMessageHandler, NodeType, ProtocolMessageTypes,
};
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub mod request_signed_values;
pub mod signage_point;

pub struct FarmerClient {
    pub client: WsClient,
    pub shared_state: Arc<FarmerSharedState>,
}
impl FarmerClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        shared_state: Arc<FarmerSharedState>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let constants = CONSENSUS_CONSTANTS_MAP
            .get(&client_config.network_id)
            .unwrap_or(&MAINNET);
        let handles = Arc::new(Mutex::new(handles(constants, shared_state.clone())));
        let client = WsClient::new(client_config, NodeType::Farmer, handles, run.clone()).await?;
        Ok(FarmerClient {
            client,
            shared_state,
        })
    }

    pub async fn join(self) -> Result<(), Error> {
        self.client.connection.lock().await.shutdown().await?;
        self.client.join().await
    }

    pub fn is_closed(&self) -> bool {
        self.client.handle.is_finished()
    }
}

fn handles(
    constants: &'static ConsensusConstants,
    shared_state: Arc<FarmerSharedState>,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from([
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
                    id: None,
                }),
                Arc::new(NewSignagePointHandle {
                    constants,
                    harvester_peers: shared_state.harvester_peers.clone(),
                    signage_points: shared_state.signage_points.clone(),
                    pool_state: shared_state.pool_state.clone(),
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
                }),
                Arc::new(RequestSignedValuesHandle {
                    quality_to_identifiers: shared_state.quality_to_identifiers.clone(),
                    recent_errors: Arc::new(Default::default()),
                    harvester_peers: shared_state.harvester_peers.clone(),
                }),
            )),
        ),
    ])
}
