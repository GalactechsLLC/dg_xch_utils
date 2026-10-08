mod common;

use async_trait::async_trait;
use common::{fast_settings, peer, spawn_full_node, wait_until};
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_p2p::{FullNodeApi, Supervisor};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;

static NETWORK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct DiscoveryApi {
    addresses: RwLock<Vec<TimestampedPeerInfo>>,
    queries: AtomicUsize,
}

#[async_trait]
impl FullNodeApi for DiscoveryApi {
    async fn block_by_height(
        &self,
        _: u32,
    ) -> Option<Box<dg_xch_core::blockchain::full_block::FullBlock>> {
        None
    }

    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        let addresses = self.addresses.read().await.clone();
        self.queries.fetch_add(1, Ordering::Relaxed);
        addresses
    }
}

#[tokio::test]
async fn connected_node_supplies_more_peers_without_an_introducer() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let leaf = spawn_full_node(Arc::new(DiscoveryApi::default())).await;
    let api = Arc::new(DiscoveryApi::default());
    let address = peer("127.0.0.1", leaf.port, 1);
    // A duplicate advertisement must not consume another outbound slot.
    *api.addresses.write().await = vec![address.clone(), address];
    let bootstrap = spawn_full_node(api).await;
    let mut supervisor = Supervisor::new(fast_settings());
    supervisor
        .seed_addresses(&[peer("127.0.0.1", bootstrap.port, 1)])
        .await;
    supervisor.start_outbound();
    let connected = wait_until(
        || async { supervisor.registry.outbound_count().await == 2 },
        common::network::NETWORK_TIMEOUT,
    )
    .await;
    let endpoints: Vec<_> = supervisor
        .registry
        .outbound_peers()
        .await
        .into_iter()
        .map(|p| p.endpoint.clone())
        .collect();
    supervisor.stop().await;
    bootstrap.run.store(false, Ordering::Relaxed);
    leaf.run.store(false, Ordering::Relaxed);
    assert!(
        connected,
        "discovery must run before the 120-second heartbeat"
    );
    assert!(endpoints.contains(&("127.0.0.1".to_string(), leaf.port)));
}

#[tokio::test]
async fn heartbeat_refresh_discovers_addresses_learned_after_connection() {
    let _guard = NETWORK_TEST_LOCK.lock().await;
    let api = Arc::new(DiscoveryApi::default());
    let bootstrap = spawn_full_node(api.clone()).await;
    let leaf = spawn_full_node(Arc::new(DiscoveryApi::default())).await;
    let mut settings = fast_settings();
    settings.heartbeat = Duration::from_millis(250);
    let mut supervisor = Supervisor::new(settings);
    supervisor
        .seed_addresses(&[peer("127.0.0.1", bootstrap.port, 1)])
        .await;
    supervisor.start_outbound();
    assert!(
        wait_until(
            || async {
                supervisor.registry.outbound_count().await == 1
                    && api.queries.load(Ordering::Relaxed) > 0
            },
            common::network::NETWORK_TIMEOUT
        )
        .await
    );
    *api.addresses.write().await = vec![peer("127.0.0.1", leaf.port, 1)];
    let connected = wait_until(
        || async { supervisor.registry.outbound_count().await == 2 },
        common::network::NETWORK_TIMEOUT,
    )
    .await;
    supervisor.stop().await;
    bootstrap.run.store(false, Ordering::Relaxed);
    leaf.run.store(false, Ordering::Relaxed);
    assert!(
        connected,
        "heartbeat responses must replenish the address book"
    );
}
