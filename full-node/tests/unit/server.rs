use super::*;
use crate::{Backend, Config, RpcTlsMode};
use std::net::SocketAddr;

fn free_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("ephemeral address")
}

#[tokio::test]
async fn shutdown_cancels_node_and_protocol_services() {
    let db =
        std::env::temp_dir().join(format!("full-node-lifecycle-{}.sqlite", std::process::id()));
    let config = Config {
        p2p: Default::default(),
        listen: free_addr(),
        rpc: free_addr(),
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 0,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        performance: Default::default(),
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
        rpc_tls: RpcTlsMode::Local,
        debug_endpoints: false,
    };
    let node = Arc::new(FullNode::boot(config).await.expect("boot node"));
    let services = node.start_services().await.expect("build services");
    let sources = Arc::new(node.metrics_sources(&services));
    let active = ActiveNode::Sqlite(BackendHandle {
        node: node.clone(),
        sources,
    });

    active.shutdown().await;
    services.drain().await;

    assert!(!active.is_running());
    assert!(!services.peer_run.load(Ordering::Relaxed));
    assert!(services.supervisor.lock().await.is_none());
}
