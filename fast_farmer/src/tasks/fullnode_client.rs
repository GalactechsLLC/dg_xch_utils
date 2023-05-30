use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use log::{error, info};
use dg_xch_clients::websocket::ClientSSLConfig;
use dg_xch_clients::websocket::farmer::FarmerClient;
use crate::models::config::Config;

static PUBLIC_CRT: &str = "farmer/public_farmer.crt";
static PUBLIC_KEY: &str = "farmer/public_farmer.key";
static PRIVATE_CRT: &str = "farmer/private_farmer.crt";
static PRIVATE_KEY: &str = "farmer/private_farmer.key";
static CA_PUBLIC_CRT: &str = "ca/chia_ca.crt";
static CA_PUBLIC_KEY: &str = "ca/chia_ca.key";
static CA_PRIVATE_CRT: &str = "ca/private_ca.crt";
static CA_PRIVATE_KEY: &str = "ca/private_ca.key";

pub async fn client_handler(shutdown_trigger: &AtomicBool, config: Arc<Config>) {
    loop {
        if !shutdown_trigger.load(Ordering::Relaxed) {
            break;
        }
        info!("Initializing SSL Farmer Client: {host}:{port}");
        let network_id = config.selected_network.as_str();
        {
            if let Some(c) = &*farmer.full_node_client.lock().await {
                c.client.lock().await.shutdown().await.unwrap_or_default();
            }
        }
        let client = None;
        while client.is_none() {
            match FarmerClient::new_ssl(
                config.host,
                config.port,
                ClientSSLConfig {
                    ssl_crt_path: format!("{}/{}", &config.ssl_root_path, PUBLIC_CRT).as_str(),
                    ssl_key_path: format!("{}/{}", &config.farmer.ssl_root_path, PUBLIC_KEY).as_str(),
                    ssl_ca_crt_path: format!("{}/{}", &config.farmer.ssl_root_path, CA_PUBLIC_CRT).as_str(),
                },
                network_id,
                additional_headers,
                client_run.clone(),
            )
                .await
            {
                Ok(c) => Some(c),
                Err(e) => {
                    error!(
                        "Failed to Start Farmer Client, Waiting and trying again: {:?}",
                        e
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if !*shutdown_receiver.lock().await {
                        break;
                    }
                    continue;
                }
            };
        }
    }
}