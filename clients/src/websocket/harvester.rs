use crate::websocket::{
    get_client, get_client_tls, perform_handshake, Client, ClientSSLConfig, NodeType,
};
use log::debug;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::task::JoinHandle;

pub struct HarvesterClient {
    pub client: Client,
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
        perform_handshake(&client, network_id, port, NodeType::Harvester).await?;
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
        debug!("Performing Handshake");
        perform_handshake(&client, network_id, port, NodeType::Harvester).await?;
        debug!("Harvester Handshake Complete");
        Ok(HarvesterClient { client, handle })
    }

    pub async fn join(mut self) -> Result<(), Error> {
        self.handle.await.map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Failed to join harvester: {:?}", e),
            )
        })?;
        self.client.shutdown().await
    }
}
