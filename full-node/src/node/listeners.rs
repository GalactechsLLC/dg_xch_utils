use super::*;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// Attach live node state consumed by the Portfu RPC routes.
    pub fn attach_rpc_live(&self, node_id: Bytes32) {
        self.state.attach_live(crate::rpc::NodeLive {
            node_id,
            network_id: self.config.network_id.clone(),
            local_port: self.config.listen.port(),
            claimed_peak: self.claimed_peak.clone(),
            slot_state: self.slot_state.clone(),
            unfinished: self.unfinished.clone(),
            inbound_peers: self.inbound_peers.clone(),
        });
    }

    /// Build the P2P peer server (`--listen`) for the Portfu listener task.
    /// Returns the server, its run flag, and the shared inbound peer map.
    ///
    /// # Errors
    /// Returns an I/O error if the TLS config or socket cannot be initialized.
    pub fn build_peer_server(&self) -> Result<(WebsocketServer, Arc<AtomicBool>, PeerMap), Error> {
        let api: Arc<dyn FullNodeApi> = Arc::new(StoreApi {
            store: self.store.clone(),
            mempool: self.mempool.clone(),
            constants: self.constants,
            claimed_peak: self.claimed_peak.clone(),
            peak_book: self.peak_book.clone(),
            // Inbound claims key by the REAL inbound peer id (distinct per connection) and are
            // retracted by the driver's per-tick reconcile against the live inbound map.
            claim_guard: None,
            new_peak_signal: self.new_peak_signal.clone(),
            known_peers: self.known_peers.clone(),
            tx_requested: self.tx_requested.clone(),
            slot_state: self.slot_state.clone(),
            sp_inbox: self.sp_inbox.clone(),
            unfinished: self.unfinished.clone(),
            ub_inbox: self.ub_inbox.clone(),
            ip_inbox: self.ip_inbox.clone(),
            synced: self.synced.clone(),
            wallet_compat: self.wallet_compat.clone(),
            tx_inbox: self.tx_inbox.clone(),
            tx_announce: self.tx_announce.clone(),
            tx_origin: self.tx_origin.clone(),
            wp_inbox: self.wp_inbox.clone(),
            compact_vdf_inbox: self.compact_vdf_inbox.clone(),
            proof_candidates: self.proof_candidates.clone(),
            candidates: self.candidates.clone(),
            producer: self.producer.clone(),
            farmed_headers: self.farmed_headers.clone(),
            wallet: self.wallet.clone(),
            trust: self.trust.clone(),
            wallet_sync_sem: self.wallet_sync_sem.clone(),
            record_window: self.record_window.clone(),
            sync_metrics: self.sync_metrics.clone(),
        });
        let handlers = full_node_handlers_counted(
            api,
            self.config.network_id.clone(),
            self.config.listen.port(),
            self.net.clone(),
        );
        // The FullNode-owned inbound map (see the field note): the server inserts/removes sessions in
        // it, and notify_new_peak reads it to reach wallet-type peers with NewPeakWallet.
        let peers: PeerMap = self.inbound_peers.clone();
        let mut server = WebsocketServer::new(
            &WebsocketServerConfig {
                host: self.config.listen.ip().to_string(),
                port: self.config.listen.port(),
                ssl_info: None,
            },
            peers.clone(),
            Arc::new(RwLock::new(handlers)),
        )?;
        // Police inbound peers against the composed rate limits: a compliant peer
        // stays within budget, a flooding/oversize peer is closed and evicted at the read loop.
        server.rate_limited = true;
        let run = Arc::new(AtomicBool::new(true));
        // Hand the shared inbound map back so /metrics can gauge its live length — the
        // retention bisect instrument for the inbound peer sessions (the collection whose
        // unbounded growth was the second live-only retainer; the PeerRegistry-derived
        // `fullnode_inbound_peers` never observed it because the server path bypasses admit_inbound).
        Ok((server, run, peers))
    }
}
