use super::*;
use std::sync::atomic::AtomicUsize;

pub(super) fn fixture() -> FullBlock {
    serde_json::from_str(include_str!("../../fixtures/full_block_5000000.json")).unwrap()
}

pub(super) async fn node() -> (tempfile::TempDir, Arc<FullNode<SqliteStore>>) {
    let directory = tempfile::tempdir().unwrap();
    let node = FullNode::boot(Config {
        p2p: P2pSettings::default(),
        listen: "127.0.0.1:0".parse().unwrap(),
        rpc: "127.0.0.1:0".parse().unwrap(),
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(directory.path().join("chain.sqlite")),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 4_999_000,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        chain_definition: None,
        performance: Default::default(),
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
        rpc_tls: crate::config::RpcTlsMode::Local,
        debug_endpoints: false,
    })
    .await
    .unwrap();
    (directory, Arc::new(node))
}

pub(super) struct Source {
    pub blocks: Vec<FullBlock>,
    pub calls: AtomicUsize,
    pub id: u64,
}

impl Source {
    pub fn new(id: u64, blocks: Vec<FullBlock>) -> Arc<Self> {
        Arc::new(Self {
            blocks,
            calls: AtomicUsize::new(0),
            id,
        })
    }
}

#[async_trait]
impl BlockRangeSource for Source {
    fn peer_id(&self) -> u64 {
        self.id
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .blocks
            .iter()
            .filter(|block| (start..=end).contains(&block.height()))
            .cloned()
            .collect())
    }
}
