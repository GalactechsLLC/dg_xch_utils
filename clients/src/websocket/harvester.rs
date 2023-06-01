use crate::websocket::{
    get_client, get_client_tls, perform_handshake, Client, ClientSSLConfig, NodeType,
};
use log::debug;
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;
use tokio::task::{JoinError, JoinHandle};

pub struct HarvesterClient {
    pub client: Arc<Mutex<Client>>,
    handle: JoinHandle<()>,
}
impl HarvesterClient {
    pub async fn new(
        host: &str,
        port: u16,
        network_id: &str,
        additional_headers: &Option<HashMap<String, String>>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let (client, mut stream) = get_client(host, port, additional_headers).await?;
        let handle = tokio::spawn(async move { stream.run(run).await });
        let client = Arc::new(Mutex::new(client));
        perform_handshake(client.clone(), network_id, port, NodeType::Harvester).await?;
        Ok(HarvesterClient { client, handle })
    }
    pub async fn new_ssl(
        host: &str,
        port: u16,
        ssl_info: ClientSSLConfig<'_>,
        network_id: &str,
        additional_headers: &Option<HashMap<String, String>>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        debug!("Starting Harvester SSL Connection");
        let (client, mut stream) = get_client_tls(host, port, ssl_info, additional_headers).await?;
        debug!("Spawning Stream Handler for Harvester SSL Connection");
        let handle = tokio::spawn(async move { stream.run(run).await });
        let client = Arc::new(Mutex::new(client));
        debug!("Performing Handshake");
        perform_handshake(client.clone(), network_id, port, NodeType::Harvester).await?;
        debug!("Harvester Handshake Complete");
        Ok(HarvesterClient { client, handle })
    }

    pub async fn join(self) -> Result<(), JoinError> {
        self.handle.await
    }
}
