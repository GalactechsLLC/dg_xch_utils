use crate::websocket::{WsClient, WsClientConfig};
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::NodeType;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct FullnodeClient {
    pub client: WsClient,
}
impl FullnodeClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let handles = Arc::new(RwLock::new(Default::default()));
        let client = WsClient::with_ca(
            client_config,
            NodeType::FullNode,
            handles,
            run,
            CHIA_CA_CRT.as_bytes(),
            CHIA_CA_KEY.as_bytes(),
        )
        .await?;
        Ok(FullnodeClient { client })
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
