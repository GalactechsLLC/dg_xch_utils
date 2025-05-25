use crate::websocket::harvester::handshake::HandshakeHandle;
#[cfg(feature = "metrics")]
use crate::websocket::WebSocketMetrics;
use crate::websocket::{WebsocketServer, WebsocketServerConfig};
use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, ProtocolMessageTypes};
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub mod handshake;

pub struct HarvesterServerConfig {
    pub network: String,
    pub websocket: WebsocketServerConfig,
}

pub struct HarvesterServer {
    pub server: WebsocketServer,
    pub config: Arc<HarvesterServerConfig>,
}
impl HarvesterServer {
    pub fn new(
        config: HarvesterServerConfig,
        #[cfg(feature = "metrics")] metrics: Arc<Option<WebSocketMetrics>>,
    ) -> Result<Self, Error> {
        let config = Arc::new(config);
        let handles = Arc::new(RwLock::new(Self::handles(config.clone())));
        Ok(Self {
            server: WebsocketServer::new(
                &config.websocket,
                Arc::default(),
                handles,
                #[cfg(feature = "metrics")]
                metrics,
            )?,
            config,
        })
    }

    fn handles(config: Arc<HarvesterServerConfig>) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
        HashMap::from([(
            Uuid::new_v4(),
            Arc::new(ChiaMessageHandler::new(
                Arc::new(ChiaMessageFilter {
                    msg_type: Some(ProtocolMessageTypes::Handshake),
                    id: None,
                    custom_fn: None,
                }),
                Arc::new(HandshakeHandle { config }),
            )),
        )])
    }

    pub async fn run(&self, run: Arc<AtomicBool>) -> Result<(), Error> {
        self.server.run(run).await
    }
}
