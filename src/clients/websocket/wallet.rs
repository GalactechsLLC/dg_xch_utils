use crate::clients::websocket::{
    get_client, get_client_tls, perform_handshake, Client, ClientSSLConfig, NodeType,
};
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct WalletClient {
    pub client: Arc<Mutex<Client>>,
}
impl WalletClient {
    pub async fn new(
        host: &str,
        port: u16,
        network_id: &str,
        additional_headers: &Option<HashMap<String, String>>,
        run: Arc<Mutex<bool>>,
    ) -> Result<Self, Error> {
        let (client, mut stream) = get_client(host, port, additional_headers).await?;
        tokio::spawn(async move { stream.run(run).await });
        let client = Arc::new(Mutex::new(client));
        let _ = perform_handshake(client.clone(), network_id, port, NodeType::Wallet).await;
        Ok(WalletClient { client })
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
        tokio::spawn(async move { stream.run(run).await });
        let client = Arc::new(Mutex::new(client));
        let _ = perform_handshake(client.clone(), network_id, port, NodeType::Wallet).await;
        Ok(WalletClient { client })
    }
}
