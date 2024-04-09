use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::protocols::{ChiaMessageHandler, NodeType};
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct FullnodeClient {
    pub client: WsClient,
}
impl FullnodeClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let handles = Arc::new(RwLock::new(handles()));
        let client = WsClient::new(client_config, NodeType::FullNode, handles, run.clone()).await?;
        Ok(FullnodeClient { client })
    }

    pub async fn join(self) -> Result<(), Error> {
        self.client.connection.write().await.shutdown().await?;
        self.client.join().await
    }

    pub fn is_closed(&self) -> bool {
        self.client.handle.is_finished()
    }
}

fn handles() -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from([])
}
