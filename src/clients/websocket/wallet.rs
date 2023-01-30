use crate::clients::websocket::{get_client, get_client_tls, perform_handshake, Client, NodeType};
use std::io::Error;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct WalletClient {
    pub client: Arc<Mutex<Client>>,
}
impl WalletClient {
    pub async fn new(host: &str, port: u16, network_id: &str) -> Result<Self, Error> {
        let (client, mut stream) = get_client(host, port).await?;
        tokio::spawn(async move { stream.run().await });
        let client = Arc::new(Mutex::new(client));
        let _ = perform_handshake(client.clone(), network_id, port, NodeType::Wallet).await;
        Ok(WalletClient { client })
    }
    pub async fn new_ssl(
        host: &str,
        port: u16,
        ssl_crt_path: &str,
        ssl_key_path: &str,
        ssl_ca_crt_path: &str,
        network_id: &str,
    ) -> Result<Self, Error> {
        let (client, mut stream) =
            get_client_tls(host, port, ssl_crt_path, ssl_key_path, ssl_ca_crt_path).await?;
        tokio::spawn(async move { stream.run().await });
        let client = Arc::new(Mutex::new(client));
        let _ = perform_handshake(client.clone(), network_id, port, NodeType::Wallet).await;
        Ok(WalletClient { client })
    }
}
