use std::env;
use std::io::Error;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use log::{info, warn};
use simple_logger::SimpleLogger;
use tokio::join;
use tokio::task::JoinHandle;
use dg_xch_core::consensus::constants::{CONSENSUS_CONSTANTS_MAP, MAINNET};
use crate::models::config::Config;

pub mod models;
pub mod tasks;

#[tokio::main]
async fn main() -> Result<(), Error> {
    SimpleLogger::new().env().init().unwrap_or_default();
    let config_path = env::var("CONFIG_PATH").unwrap_or_else(|| String::from("./farmer.yaml"));
    let path = Path::new(&config_path);
    if !path.exists() {
        warn!("No Config Found at {:?}, will use default", config_path);
    }
    let config_arc = Arc::new(Config::try_from(path).unwrap_or_default());
    let constants = CONSENSUS_CONSTANTS_MAP
        .get(&config.selected_network)
        .unwrap_or(&MAINNET); //Defaults to mainnet
    info!(
        "Selected Network: {}, AggSig: {}",
        &config.selected_network,
        &encode(constants.agg_sig_me_additional_data)
    );
    let pool_update_config_arc = config_arc.clone();
    let pool_state_run = shutdown_receiver.clone();
    let shutdown_trigger = AtomicBool::new(true);
    let pool_state_handle: JoinHandle<()> = tokio::spawn(async move {
        pool_updater(&shutdown_trigger, pool_update_config_arc).await
    });
    let client_handle: JoinHandle<Result<(), Error>> = tokio::spawn(async move {
        client_handler().await
    });
    let _ = join!(pool_state_handle, client_handle);
}