use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::protocols::{ChiaMessageHandler, NodeType};
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct WalletClient {
    pub client: WsClient,
}
impl WalletClient {
    pub async fn new(client_config: Arc<WsClientConfig>) -> Result<Self, Error> {
        let handles = Arc::new(Mutex::new(handles()));
        let client = WsClient::new(client_config, NodeType::Wallet, handles).await?;
        Ok(WalletClient { client })
    }

    pub async fn join(self) -> Result<(), Error> {
        self.client.connection.lock().await.shutdown().await?;
        self.client.join().await
    }

    pub fn is_closed(&self) -> bool {
        self.client.handle.is_finished()
    }
}

fn handles() -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from([])
}
