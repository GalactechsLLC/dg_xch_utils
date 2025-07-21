use crate::websocket::harvester::harvester_handshake::HarvesterHandshakeHandle;
use crate::websocket::harvester::new_signage_point_harvester::NewSignagePointHarvesterHandle;
use crate::websocket::harvester::request_signatures::RequestSignaturesHandle;
use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::consensus::constants::{ConsensusConstants, CONSENSUS_CONSTANTS_MAP, MAINNET};
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::harvester::HarvesterState;
use dg_xch_core::protocols::{
    ChiaMessageFilter, ChiaMessageHandler, NodeType, ProtocolMessageTypes,
};
use dg_xch_pos::PlotManagerAsync;
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;
pub mod harvester_handshake;
pub mod new_signage_point_harvester;
pub mod request_signatures;

pub struct HarvesterClient {
    pub client: WsClient,
}
impl HarvesterClient {
    pub async fn new<T: PlotManagerAsync + Send + Sync + 'static>(
        client_config: Arc<WsClientConfig>,
        plot_manager: Arc<RwLock<T>>,
        plots_ready: Arc<AtomicBool>,
        harvester_state: Arc<RwLock<HarvesterState>>,
        run: Arc<AtomicBool>,
        timeout: u64,
    ) -> Result<Self, Error> {
        let constants = CONSENSUS_CONSTANTS_MAP
            .get(&client_config.network_id)
            .unwrap_or(&MAINNET);
        let handles = Arc::new(RwLock::new(handles(
            constants,
            plot_manager.clone(),
            plots_ready,
            harvester_state,
        )));
        let client = WsClient::with_ca(
            client_config,
            NodeType::Harvester,
            handles,
            run,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
            timeout,
        )
        .await?;
        Ok(HarvesterClient { client })
    }

    pub async fn join(self) -> Result<(), Error> {
        self.client.join().await
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.client.handle.is_finished()
    }
}

fn handles<T: PlotManagerAsync + Send + Sync + 'static>(
    constants: &'static ConsensusConstants,
    plot_manager: Arc<RwLock<T>>,
    plots_ready: Arc<AtomicBool>,
    harvester_state: Arc<RwLock<HarvesterState>>,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from([
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::HarvesterHandshake),
                    id: None,
                    custom_fn: None,
                }),
                Arc::new(HarvesterHandshakeHandle {
                    plot_manager: plot_manager.clone(),
                    harvester_state,
                }),
            )),
        ),
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::NewSignagePointHarvester),
                    id: None,
                    custom_fn: None,
                }),
                Arc::new(NewSignagePointHarvesterHandle {
                    constants,
                    plot_manager: plot_manager.clone(),
                    plots_ready,
                }),
            )),
        ),
        (
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::RequestSignatures),
                    id: None,
                    custom_fn: None,
                }),
                Arc::new(RequestSignaturesHandle { plot_manager }),
            )),
        ),
    ])
}
