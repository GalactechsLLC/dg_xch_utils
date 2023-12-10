use crate::websocket::{WsClient, WsClientConfig};
use crate::websocket::farmer::signage_point::NewSignagePointHandle;
use crate::websocket::farmer::request_signed_values::RequestSignedValuesHandle;
use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, NodeType, PeerMap, ProtocolMessageTypes};
use dg_xch_core::protocols::farmer::{FarmerIdentifier, FarmerPoolState, NewSignagePoint};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::collections::HashMap;
use std::io::{Error};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use uuid::Uuid;
use dg_xch_core::consensus::constants::{CONSENSUS_CONSTANTS_MAP, ConsensusConstants, MAINNET};

pub mod request_signed_values;
pub mod signage_point;

pub struct FarmerClient {
    pub client: WsClient,
}
impl FarmerClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
        signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
        pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
        cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
        harvester_peers: PeerMap,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let constants = CONSENSUS_CONSTANTS_MAP.get(&client_config.network_id).unwrap_or(&MAINNET);
        let handles = Arc::new(Mutex::new(handles(
            constants,
            quality_to_identifiers,
            signage_points,
            pool_state,
            cache_time,
            harvester_peers,
        )));
        let client = WsClient::new(
            client_config,
            NodeType::Farmer,
            handles,
            run,
        ).await?;
        Ok(FarmerClient { client })
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
    quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
    signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
    cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
    harvester_peers: PeerMap,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from(
        [(Uuid::new_v4(), Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
                id: None,
            }),
            Arc::new(NewSignagePointHandle {
                constants,
                harvester_peers: harvester_peers.clone(),
                signage_points: signage_points.clone(),
                pool_state: pool_state.clone(),
                cache_time: cache_time.clone(),
            }),
        ))),
        (Uuid::new_v4(), Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: Some(ProtocolMessageTypes::RequestSignedValues),
                id: None,
            }),
            Arc::new(RequestSignedValuesHandle {
                quality_to_identifiers: quality_to_identifiers.clone(),
                harvester_peers: harvester_peers.clone(),
            })
        )))]
    )
}