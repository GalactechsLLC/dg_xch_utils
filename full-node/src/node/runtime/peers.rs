use super::*;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    // Build the per-connection handler factory for OUTBOUND peer links: each dial gets a fresh
    // full_node_handlers_client map sharing this node's store/mempool/claimed-peak/claimed-tip, so a peer's
    // NewPeak updates the sync target and RequestBlock/RequestBlocks are served — while gossip is
    // graceful-ignored and the client never re-handshakes. This closes the live "No Matches" gap.
    // `pub` so integration tests can stand up the PRODUCTION outbound handler stack (StoreApi
    // gates + client dispatch) against a mock peer — the announce-pull tests dial with exactly
    // the map a live outbound slot gets.
    #[must_use]
    pub fn outbound_handler_factory(&self) -> HandlerFactory {
        let store = self.store.clone();
        let mempool = self.mempool.clone();
        let claimed_peak = self.claimed_peak.clone();
        let peak_book = self.peak_book.clone();
        let new_peak_signal = self.new_peak_signal.clone();
        let known_peers = self.known_peers.clone();
        let constants = self.constants;
        let tx_requested = self.tx_requested.clone();
        let slot_state = self.slot_state.clone();
        let sp_inbox = self.sp_inbox.clone();
        let unfinished = self.unfinished.clone();
        let ub_inbox = self.ub_inbox.clone();
        let ip_inbox = self.ip_inbox.clone();
        let synced_flag = self.synced.clone();
        let tx_inbox = self.tx_inbox.clone();
        let tx_announce = self.tx_announce.clone();
        let tx_origin = self.tx_origin.clone();
        let wp_inbox = self.wp_inbox.clone();
        let compact_vdf_inbox = self.compact_vdf_inbox.clone();
        let proof_candidates = self.proof_candidates.clone();
        let candidates = self.candidates.clone();
        let producer = self.producer.clone();
        let farmed_headers = self.farmed_headers.clone();
        let wallet = self.wallet.clone();
        let trust = self.trust.clone();
        let wallet_sync_sem = self.wallet_sync_sem.clone();
        let net = self.net.clone();
        let network_id = self.config.network_id.clone();
        let port = self.config.listen.port();
        let record_window = self.record_window.clone();
        let sync_metrics = self.sync_metrics.clone();
        let wallet_compat = self.wallet_compat.clone();
        Arc::new(move || {
            let api: Arc<dyn FullNodeApi> = Arc::new(StoreApi {
                store: store.clone(),
                mempool: mempool.clone(),
                constants,
                claimed_peak: claimed_peak.clone(),
                peak_book: peak_book.clone(),
                // One factory invocation = one outbound dial: mint this connection's claim key. Its
                // Drop (with the connection's handler map) retracts the claim — the
                // sync_store.peer_disconnected for the outbound side, where the dispatch peer id
                // cannot distinguish connections (it is our own cert hash).
                claim_guard: Some(Arc::new(peak_book.outbound_guard())),
                new_peak_signal: new_peak_signal.clone(),
                known_peers: known_peers.clone(),
                tx_requested: tx_requested.clone(),
                slot_state: slot_state.clone(),
                sp_inbox: sp_inbox.clone(),
                unfinished: unfinished.clone(),
                ub_inbox: ub_inbox.clone(),
                ip_inbox: ip_inbox.clone(),
                synced: synced_flag.clone(),
                wallet_compat: wallet_compat.clone(),
                tx_inbox: tx_inbox.clone(),
                tx_announce: tx_announce.clone(),
                tx_origin: tx_origin.clone(),
                wp_inbox: wp_inbox.clone(),
                compact_vdf_inbox: compact_vdf_inbox.clone(),
                proof_candidates: proof_candidates.clone(),
                candidates: candidates.clone(),
                producer: producer.clone(),
                farmed_headers: farmed_headers.clone(),
                wallet: wallet.clone(),
                trust: trust.clone(),
                wallet_sync_sem: wallet_sync_sem.clone(),
                record_window: record_window.clone(),
                sync_metrics: sync_metrics.clone(),
            });
            full_node_handlers_client_counted(api, network_id.clone(), port, net.clone())
        })
    }
}
