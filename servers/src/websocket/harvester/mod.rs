use crate::websocket::harvester::handshake::HandshakeHandle;
use crate::websocket::{WebsocketServer, WebsocketServerConfig};
use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, ProtocolMessageTypes};
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;
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
    pub fn new(config: HarvesterServerConfig) -> Result<Self, Error> {
        let config = Arc::new(config);
        let handles = Arc::new(Mutex::new(Self::handles(config.clone())));
        Ok(Self {
            server: WebsocketServer::new(&config.websocket, Default::default(), handles)?,
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
                }),
                Arc::new(HandshakeHandle {
                    config: config.clone(),
                }),
            )),
        )])
    }

    pub async fn run(&self, run: Arc<AtomicBool>) -> Result<(), Error> {
        self.server.run(run).await
    }
}
