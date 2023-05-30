use crate::websocket::{
    get_client, get_client_tls, perform_handshake, Client, ClientSSLConfig, NodeType,
};
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::{JoinError, JoinHandle};

pub struct FullnodeClient {
    pub client: Arc<Mutex<Client>>,
    handle: JoinHandle<()>,
}
impl FullnodeClient {
    pub async fn new(
        host: &str,
        port: u16,
        network_id: &str,
        additional_headers: &Option<HashMap<String, String>>,
        run: Arc<Mutex<bool>>,
    ) -> Result<Self, Error> {
        let (client, mut stream) = get_client(host, port, additional_headers).await?;
        let handle = tokio::spawn(async move { stream.run(run).await });
        let client = Arc::new(Mutex::new(client));
        perform_handshake(client.clone(), network_id, port, NodeType::FullNode).await?;
        Ok(FullnodeClient { client, handle })
    }
    pub async fn new_ssl(
        host: &str,
        port: u16,
        ssl_info: ClientSSLConfig<'_>,
        network_id: &str,
        additional_headers: &Option<HashMap<String, String>>,
        run: Arc<Mutex<bool>>,
    ) -> Result<Self, Error> {
        let (client, mut stream) = get_client_tls(host, port, ssl_info, additional_headers).await?;
        let handle = tokio::spawn(async move { stream.run(run).await });
        let client = Arc::new(Mutex::new(client));
        perform_handshake(client.clone(), network_id, port, NodeType::FullNode).await?;
        Ok(FullnodeClient { client, handle })
    }

    pub async fn join(self) -> Result<(), JoinError> {
        self.handle.await
    }
}
