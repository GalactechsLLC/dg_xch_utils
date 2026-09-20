use crate::farmer::config::Config;
use crate::utils::rpc_client_from_config;
use dg_xch_clients::rpc::full_node::FullnodeAPI;
use dg_xch_core::protocols::farmer::FarmerSharedState;
use log::error;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

pub async fn update_blockchain<T, C: Clone>(
    farmer_state: Arc<FarmerSharedState<T>>,
    config: Arc<RwLock<Config<C>>>,
) {
    let config = config.read().await.clone();
    let mut full_node_rpc_res = rpc_client_from_config(&config, &None);
    let mut last_update = Instant::now();
    loop {
        let full_node_rpc = match &full_node_rpc_res {
            Ok(full_node_rpc) => full_node_rpc.clone(),
            Err(_) => {
                error!("Failed to Connect FullNode RPC. Trying again in 10 seconds.");
                tokio::time::sleep(Duration::from_secs(10)).await;
                full_node_rpc_res = rpc_client_from_config(&config, &None);
                continue;
            }
        };
        if last_update.elapsed().as_secs() > 5 {
            last_update = Instant::now();
            let bc_state = full_node_rpc.get_blockchain_state().await;
            match bc_state {
                Ok(bc_state) => {
                    if let Some(metrics) = &*farmer_state.metrics.read().await {
                        metrics.blockchain_height.set(
                            bc_state.peak.as_ref().map(|p| p.height).unwrap_or_default() as u64,
                        );
                        metrics.blockchain_synced.set(bc_state.sync.synced as u64);
                        metrics
                            .blockchain_netspace
                            .set((bc_state.space / 1024u128 / 1024u128 / 1024u128) as u64); //Convert to GiB for better fitting into u64
                    }
                    *farmer_state.fullnode_state.write().await = Some(bc_state);
                }
                Err(e) => {
                    error!("{e:?}");
                }
            }
        }
        if !farmer_state.signal.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
