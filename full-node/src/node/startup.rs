//! FullNode construction and protocol-service wiring.

use super::*;

/// Open the configured storage backend. Only the embedded SQLite backend is built today.
///
/// # Errors
/// Returns [`ErrorKind::Unsupported`] for a `postgres://` URL (the industrial backend is a
/// `dg_xch_stores` concern not yet landed), or an I/O error if the SQLite database cannot be
/// opened or migrated.
pub async fn open_backend(backend: &Backend) -> Result<Arc<SqliteStore>, Error> {
    match backend {
        Backend::Sqlite(path) => {
            let store = SqliteStore::open(path)
                .await
                .map_err(|e| Error::other(format!("open sqlite {}: {e}", path.display())))?;
            Ok(Arc::new(store))
        }
        // The Postgres and mmap backends are constructed in main's dispatch (FullNode::boot_with_store);
        // this SQLite-typed helper only serves FullNode::boot's embedded path.
        Backend::Postgres(url) => Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "postgres backend ({url}) boots via FullNode::boot_with_store, not open_backend"
            ),
        )),
        Backend::Mmap(dir) => Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "mmap backend ({}) boots via FullNode::boot_with_store, not open_backend",
                dir.display()
            ),
        )),
    }
}

impl FullNode<SqliteStore> {
    /// Boot the node on the embedded SQLite backend without starting network listeners.
    ///
    /// # Errors
    /// Returns an I/O error if the backend cannot be opened.
    pub async fn boot(config: Config) -> Result<Self, Error> {
        let store = open_backend(&config.backend).await?;
        Self::boot_with_store(config, store)
    }
}

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// Wire the node around an already-opened store using constants selected by network id.
    ///
    /// # Errors
    /// Infallible today; kept fallible so store-dependent wiring can fail cleanly later.
    pub fn boot_with_store(config: Config, store: Arc<S>) -> Result<Self, Error> {
        let constants = constants_for(&config.network_id);
        Self::boot_with_store_constants(config, store, constants)
    }

    /// Wire the node around an already-opened store and explicit consensus constants.
    pub fn boot_with_store_constants(
        config: Config,
        store: Arc<S>,
        constants: ConsensusConstants,
    ) -> Result<Self, Error> {
        let engine = Engine::new(store.clone(), NativePrimitives, constants);
        let chaser = Chaser::new(engine, SyncConfig::default());
        let sync_metrics = chaser.metrics().clone();
        let claimed_peak = Arc::new(AtomicU32::new(0));
        let mempool = Arc::new(Mutex::new(Mempool::new(&constants)));
        let trust = Arc::new(TrustPolicy::from_config(
            &config.trusted_peers,
            &config.trusted_cidrs,
        ));
        let wallet = Arc::new(WalletNotifier::with_trust(trust.clone()));
        let wallet_sync_sem = Arc::new(LimitedSemaphore::new(
            WALLET_SYNC_ACTIVE_LIMIT,
            WALLET_SYNC_WAITING_LIMIT,
        ));
        let synced = Arc::new(AtomicBool::new(false));
        let wallet_compat = Arc::new(AtomicBool::new(false));
        let tx_announce = Arc::new(Mutex::new(Vec::new()));
        let tx_requested = Arc::new(Mutex::new(HashMap::new()));
        let slot_state = Arc::new(Mutex::new(SlotState::new(constants)));
        let state = Arc::new(Node::new(
            store.clone(),
            mempool.clone(),
            constants,
            synced.clone(),
            tx_announce.clone(),
        ));
        let node = Self {
            config,
            store,
            mempool,
            wallet,
            trust,
            wallet_sync_sem,
            state,
            synced,
            wallet_compat,
            run: Arc::new(AtomicBool::new(true)),
            deferred_indexes_started: Arc::new(AtomicBool::new(false)),
            service_indexes_shed: Arc::new(AtomicBool::new(false)),
            maintenance_tasks: Mutex::new(tokio::task::JoinSet::new()),
            constants,
            claimed_peak: claimed_peak.clone(),
            peak_book: Arc::new(PeakBook::new(claimed_peak)),
            new_peak_signal: Arc::new(Notify::new()),
            validated_tip: Arc::new(RwLock::new(None)),
            long_sync_anchor: Arc::new(RwLock::new(None)),
            sync_from_anchor: Arc::new(RwLock::new(None)),
            known_peers: Arc::new(RwLock::new(Vec::new())),
            inbound_peers: Arc::new(RwLock::new(HashMap::new())),
            tx_announce,
            tx_requested,
            slot_state,
            sp_inbox: Arc::new(Mutex::new(Vec::new())),
            sp_announce: Arc::new(Mutex::new(Vec::new())),
            sp_farmer_announce: Arc::new(Mutex::new(Vec::new())),
            net: Arc::new(NetCounters::default()),
            unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
            ub_inbox: Arc::new(Mutex::new(Vec::new())),
            ip_inbox: Arc::new(Mutex::new(Vec::new())),
            ub_announce: Arc::new(Mutex::new(Vec::new())),
            ub_timelord_announce: Arc::new(Mutex::new(Vec::new())),
            tx_inbox: Arc::new(Mutex::new(TxQueue::new(
                TX_INBOX_CAP,
                TX_INBOX_PER_PEER,
                constants.max_block_cost_clvm / 2,
            ))),
            wp_inbox: Arc::new(Mutex::new(Vec::new())),
            compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
            compact_vdf_announce: Arc::new(Mutex::new(Vec::new())),
            proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
            candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
            producer: Arc::new(ProducerMetrics::default()),
            farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
            sp_current_index: Arc::new(AtomicU32::new(0)),
            signage_points_total: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            follow_inflight_since: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sync_metrics,
            last_delta_hash: Mutex::new(None),
            tx_origin: Arc::new(Mutex::new(HashMap::new())),
            seed_ref_cache: Mutex::new(VecDeque::new()),
            chaser: Mutex::new(chaser),
            record_window: Arc::new(Mutex::new(BlockRecordCache::new(
                crate::record_window::record_window_capacity(&constants),
            ))),
        };

        Ok(node)
    }

    pub async fn run_tx_validator(self: Arc<Self>) {
        tx_validator(
            self.store.clone(),
            self.mempool.clone(),
            self.constants,
            self.tx_inbox.clone(),
            self.tx_announce.clone(),
            self.synced.clone(),
            self.run.clone(),
        )
        .await;
    }

    pub async fn run_weight_proof_worker(self: Arc<Self>) {
        weight_proof_worker(
            self.store.clone(),
            self.constants,
            self.wp_inbox.clone(),
            self.net.clone(),
            self.run.clone(),
        )
        .await;
    }

    pub async fn run_uncompact_scanner(self: Arc<Self>) {
        if self.config.uncompact {
            uncompact_scanner(
                self.store.clone(),
                self.inbound_peers.clone(),
                self.net.clone(),
                self.run.clone(),
            )
            .await;
        }
    }

    pub(crate) async fn stop_maintenance(&self) {
        let mut tasks = self.maintenance_tasks.lock().await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }

    /// Build the protocol servers and outbound supervisor state consumed by Portfu-owned tasks.
    ///
    /// # Errors
    /// Returns an I/O error if a protocol server fails to start.
    pub async fn start_services(self: &Arc<Self>) -> Result<crate::server::NodeServices, Error> {
        install_crypto_provider();
        let (peer_server, peer_run, inbound_peers) = self.build_peer_server()?;

        let mut supervisor = Supervisor::new(self.config.p2p);
        supervisor.set_handlers(self.outbound_handler_factory());
        {
            let hook_node = self.clone();
            supervisor.set_on_connect(Arc::new(move |peer| {
                let node = hook_node.clone();
                Box::pin(async move { outbound_on_connect(&node, peer.as_ref()).await })
            }));
        }

        if !self.config.manual_peers.is_empty() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let manual: Vec<TimestampedPeerInfo> = self
                .config
                .manual_peers
                .iter()
                .map(|(host, port)| TimestampedPeerInfo {
                    host: host.clone(),
                    port: *port,
                    timestamp: now,
                })
                .collect();
            let seeded = supervisor.seed_addresses(&manual).await;
            info!(
                "seeded manual peers seeded={} configured={}",
                seeded,
                manual.len()
            );
        }

        let peer_registry = supervisor.registry.clone();
        let registry: Arc<dyn OutboundPeers> = peer_registry.clone();
        Ok(crate::server::NodeServices {
            registry,
            peer_registry,
            inbound_peers,
            peer_run,
            peer_server: Arc::new(peer_server),
            introducer: self.config.introducer.clone(),
            supervisor: tokio::sync::Mutex::new(Some(supervisor)),
        })
    }

    /// Build the `/metrics` and `/health` read handles for the running node.
    pub fn metrics_sources(&self, services: &crate::server::NodeServices) -> MetricsSources<S> {
        MetricsSources {
            store: self.store.clone(),
            metrics: self.sync_metrics.clone(),
            claimed_peak: self.claimed_peak.clone(),
            registry: services.peer_registry.clone(),
            inbound_peers: services.inbound_peers.clone(),
            sync_from: self.config.sync_from,
            net: self.net.clone(),
            mempool: self.mempool.clone(),
            sp_current_index: self.sp_current_index.clone(),
            signage_points_total: self.signage_points_total.clone(),
            producer: self.producer.clone(),
            follow_inflight_since: self.follow_inflight_since.clone(),
            health: HealthState::new(),
        }
    }
}
