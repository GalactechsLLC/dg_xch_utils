use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::{ChiaMessageHandler, NodeType};
use std::collections::HashMap;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct WalletClient {
    pub client: WsClient,
}
impl WalletClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        run: Arc<AtomicBool>,
        timeout: u64,
    ) -> Result<Self, Error> {
        let handles = Arc::new(RwLock::new(handles()));
        let client = WsClient::with_ca(
            client_config,
            NodeType::Wallet,
            handles,
            run,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
            timeout,
        )
        .await?;
        Ok(WalletClient { client })
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

fn handles() -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    HashMap::from([])
}
