//! Store-backed peer protocol handlers and wallet request surface.

use super::*;

// The store-backed peer protocol surface: peers pull blocks from us (RequestBlock/RequestBlocks), fetch a
// mempool transaction, and announce their tip (NewPeak → recorded as a sync target). Blind to consensus.
pub(super) struct StoreApi<S> {
    pub(super) store: Arc<S>,
    pub(super) mempool: Arc<Mutex<Mempool>>,
    pub(super) constants: ConsensusConstants,
    pub(super) claimed_peak: Arc<AtomicU32>,
    // The per-peer peak-claim book (`sync_store`): on_new_peak records the announcing peer's
    // (hash, height, weight) claim here; the sync bands select the heaviest verified-selectable claim.
    pub(super) peak_book: Arc<PeakBook>,
    // OUTBOUND connections only: the minted per-connection claim key whose Drop retracts the claim
    // (`sync_store.peer_disconnected`). `None` on the shared inbound server api, which keys claims
    // by the real inbound peer id and reconciles them against the live inbound map each driver tick.
    pub(super) claim_guard: Option<Arc<ClaimGuard>>,
    pub(super) new_peak_signal: Arc<Notify>,
    // Live outbound peers as TimestampedPeerInfo — the RequestPeers gossip answer, refreshed by the driver.
    pub(super) known_peers: Arc<RwLock<Vec<TimestampedPeerInfo>>>,
    pub(super) tx_requested: Arc<Mutex<HashMap<Bytes32, PendingTx>>>,
    // The slot state machine — handlers read it to answer/filter SP gossip; only the
    // driver writes it (validation needs the record ancestry + next-SSI context).
    pub(super) slot_state: Arc<Mutex<SlotState>>,
    // Received signage points / EOS bundles awaiting driver-side validation into the slot state.
    pub(super) sp_inbox: Arc<Mutex<Vec<SpEvent>>>,
    // The unfinished-block cache (announce dedup + request tracking + serve path) and
    // the received-block inbox the driver validates through validate_unfinished_header_block.
    pub(super) unfinished: Arc<Mutex<UnfinishedCache>>,
    pub(super) ub_inbox: Arc<Mutex<Vec<UnfinishedBlock>>>,
    // Timelord infusion-return inbox (`new_infusion_point_vdf`): the infusion-point VDFs that finish
    // one of OUR cached unfinished blocks into a FullBlock. Queued off the read loop; the driver
    // (process_ip_inbox) assembles the FullBlock, runs it through the engine, and sets the new peak.
    pub(super) ip_inbox: Arc<Mutex<Vec<NewInfusionPointVDF>>>,
    // The sync-status flag: slot/unfinished gossip is tip-context, so a deep-syncing node pulls
    // nothing it cannot validate (the ignore-while-syncing guard on these handlers).
    pub(super) synced: Arc<AtomicBool>,
    // Simulator only: serve wallets a v1-shaped proof of space in headers (stock wallets cannot
    // deserialize a v2 proof). Off on a production node.
    pub(super) wallet_compat: Arc<AtomicBool>,
    // Received bundles awaiting the validator worker (never validated on the read loop). A trusted
    // peer's bundle takes the high-priority lane (the high-priority lane).
    pub(super) tx_inbox: Arc<Mutex<TxQueue>>,
    // Accepted transactions queued for NewTransaction re-broadcast — shared with the FullNode/rpc so a
    // wallet's p2p SendTransaction admission announces through the SAME seam as push_tx and the
    // gossip worker (the announce fires for every successful admission).
    pub(super) tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    // txid -> (origin identity, when recorded): the peer a gossiped bundle arrived FROM, recorded at
    // receipt (`on_respond_transaction`) with its remote host so the announce drain can exclude an
    // OUTBOUND origin — whose dispatch id is our shared client-cert hash.
    // The SAME Arc the FullNode holds, so the drain sees receipt-time records. Bounded — record_tx_origin.
    pub(super) tx_origin: Arc<Mutex<HashMap<Bytes32, (TxOrigin, Instant)>>>,
    // RequestProofOfWeight requests awaiting the weight-proof worker (built off the read path).
    pub(super) wp_inbox: Arc<Mutex<Vec<WpRequest>>>,
    // Compact-VDF consume: pulled RespondCompactVDF proofs awaiting the driver's
    // validate + swap + re-gossip pass (a VDF verify never runs on the websocket read loop).
    pub(super) compact_vdf_inbox: Arc<Mutex<Vec<RespondCompactVDF>>>,
    // Farmer interface: accepted DeclareProofOfSpace declarations, held as block candidates
    // until assembly consumes them. Bounded FIFO — evicting a stale candidate is harmless.
    pub(super) proof_candidates: Arc<Mutex<ProofCandidateStore>>,
    // Candidate unfinished blocks awaiting the farmer's foliage signatures, keyed by quality
    // string. declare builds + stores here;
    // signed_values retrieves, splices the real signatures, and emits.
    pub(super) candidates: Arc<Mutex<CandidateBlockStore>>,
    // Block-producer pipeline counters (the first-block funnel) — shared with FullNode + the
    // driver broadcasts + the /metrics scrape.
    pub(super) producer: Arc<ProducerMetrics>,
    // Header hashes of unfinished blocks WE farmed (= FullBlock.header_hash = hash of the spliced
    // foliage), recorded at signed_values splice time so the follow driver can recognise our own block
    // when it confirms (S8). Bounded FIFO; a farmed block that never confirms just ages out.
    pub(super) farmed_headers: Arc<Mutex<VecDeque<Bytes32>>>,
    // The wallet coin-state subscription registry (shared with the server's confirm path, which pushes
    // CoinStateUpdate deltas into it). RegisterForPh/CoinUpdates register here and get the delivery
    // receiver the dispatch layer bridges to the socket.
    pub(super) wallet: Arc<WalletNotifier>,
    // The FullNode's consensus-walk record window + sync metrics, shared so the on-connect TIMELORD
    // greeting (`timelord_peak`) can run the same build as `broadcast_new_peak_timelord`.
    pub(super) record_window: Arc<Mutex<BlockRecordCache>>,
    pub(super) sync_metrics: Arc<SyncMetrics>,
    // The shared trusted-peer policy — resolves the register initial-state response budget
    // (`max_subscribe_response_items(peer)`) per-peer: the untrusted 100,000 by default, the
    // trusted 500,000 for a configured trusted peer. One budget is DECREMENTED across the puzzle-hash query and then the hint query of
    // a single RegisterForPhUpdates, and caps the RegisterForCoinUpdates initial read — so one
    // dust-storm puzzle hash cannot materialize an unbounded CoinState set into a single reply. Also
    // decides tx-queue priority (on_transaction) — the SAME `Arc` the WalletNotifier holds.
    pub(super) trust: Arc<TrustPolicy>,
    // Bounds CONCURRENT heavy wallet-serve DB work (`wallet_sync_api_sem`):
    // shared node-wide across every peer's handler map; additions/removals acquire it and REJECT on
    // overflow. The read-loop rate limiter bounds message rate; this bounds the
    // concurrent block-delta scans those messages fan out into. Only the coin-index tier serves
    // additions/removals, hence unread (not unbounded) without that feature.
    #[cfg_attr(not(feature = "coin-index"), allow(dead_code))]
    pub(super) wallet_sync_sem: Arc<LimitedSemaphore>,
}

// `max_duplicate_unfinished_blocks`: variants of one reward hash worth fetching.
pub(super) const MAX_DUPLICATE_UNFINISHED_BLOCKS: usize = 3;

// Transaction-inbox bounds (per-peer queues under an aggregate cap).
pub(super) const TX_INBOX_CAP: usize = 256;
pub(super) const TX_INBOX_PER_PEER: usize = 32;

// How many of OUR farmed unfinished-block header hashes to remember for the S8 confirm match. A few
// slots of candidates is ample; a farmed block that never confirms just ages out of this FIFO.
pub(super) const FARMED_HEADER_CAP: usize = 256;

// The wall-clock budget for assembling a block generator from the mempool on a winning declare —
// `block_creation_timeout` config default (2.0s).
pub(super) const BLOCK_CREATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

// One received slot-gossip payload, queued for the driver's validation pass.
pub(super) enum SpEvent {
    SignagePoint(Box<RespondSignagePoint>),
    EndOfSubSlot(Box<RespondEndOfSubSlot>),
}

// The received-gossip inbox cap: a peer cannot grow driver work without bound between ticks.
pub(super) const SP_INBOX_CAP: usize = 256;

// The infusion-point inbox cap. One infusion-point VDF per farmed unfinished block per slot; a handful
// in flight between driver ticks is ample, and a full inbox drops the excess (the timelord re-sends on
// the next NewPeakTimelord). Bounds driver work a misbehaving timelord could otherwise grow without limit.
pub(super) const IP_INBOX_CAP: usize = 64;

// One queued RequestProofOfWeight: who asked, over which link map, for which tip, under which
// request id (the requester's oneshot matches RespondProofOfWeight by type + this id).
pub(super) struct WpRequest {
    pub(super) peer: Bytes32,
    pub(super) peers: PeerMap,
    pub(super) tip: Bytes32,
    pub(super) id: Option<u16>,
}

// The weight-proof request inbox cap. Building a proof walks sub-epochs of store history — a
// handful of queued requests is plenty, the rest drop (the peer retries or asks another node).
pub(super) const WP_INBOX_CAP: usize = 8;

mod protocol;

// The on-connect NewPeak greeting construction (on_connect), shared by
// the inbound Handshake greeting ([`FullNodeApi::full_node_peak`]) and the outbound dial hook
// ([`outbound_on_connect`]): fork point = the peak height itself, unfinished hash =
// `reward_chain_block.get_unfinished().get_hash()`.
pub(super) async fn on_connect_new_peak<S: BlockStore + Send + Sync>(store: &S) -> Option<NewPeak> {
    let (hash, height) = store.get_peak().await.ok().flatten()?;
    let block = store.get_block(&hash).await.ok().flatten()?;
    let unfinished = block.reward_chain_block.get_unfinished();
    let bytes = unfinished.to_bytes(ChiaProtocolVersion::default()).ok()?;
    Some(NewPeak {
        header_hash: hash,
        height,
        weight: block.reward_chain_block.weight,
        fork_point_with_previous_peak: height,
        unfinished_reward_block_hash: dg_xch_core::utils::hash_256(&bytes).into(),
    })
}

pub(super) async fn on_connect_mempool_filter(
    synced: &AtomicBool,
    mempool: &Mutex<Mempool>,
) -> Option<Vec<u8>> {
    if !synced.load(Ordering::Relaxed) {
        return None;
    }
    let ids: Vec<Vec<u8>> = {
        let mp = mempool.lock().await;
        mp.items_by_fee()
            .into_iter()
            .map(|item| SizedBytes::bytes(&item.name).to_vec())
            .collect()
    };
    Some(dg_xch_core::consensus::block_filter::chia_block_filter(
        &ids,
    ))
}

/// The outbound half of the on-connect greetings: `on_connect` fires for OUTGOING
/// connections too (after
/// the handshake), and every peer we dial is a FULL_NODE link — so it gets the mempool-sync
/// request when we are synced  and the NewPeak greeting.
/// Without this, a node never mempool-syncs from the peers IT dials — which on a fresh boot is
/// every peer it has. Run by the supervisor's on-connect hook against each registered outbound
/// dial; fire-and-forget (a send failure surfaces as the connection dropping).
pub async fn outbound_on_connect<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
    peer: &OutboundPeer,
) {
    let version = outbound_peer_version(peer);
    // The mempool request goes first, then the peak.
    if let Some(filter) = on_connect_mempool_filter(&node.synced, &node.mempool).await {
        let req = dg_xch_core::protocols::full_node::RequestMempoolTransactions { filter };
        if let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::RequestMempoolTransactions,
            version,
            &req,
            None,
        ) {
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::RequestMempoolTransactions,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg).await;
        }
    }
    if let Some(peak) = on_connect_new_peak(node.store.as_ref()).await
        && let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            version,
            &peak,
            None,
        )
    {
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            msg.data.as_slice().len(),
        );
        let _ = peer.client.send(msg).await;
    }
}

// additions/removals request caps: the max coin/puzzle hashes a single
// proof request may carry, and the max distinct puzzle hashes the all-additions (no-proof) answer holds
// before it rejects rather than build an oversized response.
#[cfg(feature = "coin-index")]
pub(super) const MAX_COIN_HASHES_PER_REQUEST: usize = 50;
#[cfg(feature = "coin-index")]
pub(super) const MAX_COINS_MAP_SIZE: usize = 100;

// `max_subscribe_response_items` for an UNTRUSTED peer (initial-config.yaml:441,
// default): the untrusted response budget. Production resolves it per-peer from
// `TrustPolicy`; this named constant is the untrusted-tier value the coin-index wallet-query tests
// inject via `api_tuned` (its only consumers live in the coin-index-gated `wallet_queries` module).
#[cfg(all(test, feature = "coin-index"))]
pub(super) const MAX_SUBSCRIBE_RESPONSE_ITEMS: usize = 100_000;

// `wallet_sync_api_sem = LimitedSemaphore.create(active_limit=2, waiting_limit=20)`
//: the node-wide concurrency bound on the heavy wallet-serve handlers.
pub(super) const WALLET_SYNC_ACTIVE_LIMIT: usize = 2;
pub(super) const WALLET_SYNC_WAITING_LIMIT: usize = 20;

// A CoinRecord rendered as the wallet-protocol CoinState: both heights are
// carried, with an unspent coin's spent_height left None.
#[cfg(feature = "coin-index")]
pub(super) fn coin_state_of(cr: &CoinRecord) -> CoinState {
    CoinState {
        coin: cr.coin,
        created_height: Some(cr.confirmed_block_index),
        spent_height: (cr.spent_block_index != 0).then_some(cr.spent_block_index),
    }
}

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    // `blockchain.height_to_hash` as the wallet-sync handlers use it (
    // 2061, 2096): the MAIN-CHAIN header hash at a height, `None` when the height is above the
    // peak / unknown — which the callers turn into the REORG reject.
    async fn height_to_hash(&self, height: u32) -> Option<Bytes32> {
        self.store
            .get_block_record_by_height(height)
            .await
            .ok()
            .flatten()
            .map(|r| r.header_hash)
    }

    // The served HeaderBlock: the real BIP158
    // transactions_filter for a transaction block (every added coin's puzzle hash — tx additions
    // plus reward claims, exactly the coin store's added-at-height rows — then every removed
    // coin's name: ), and the encoded-empty
    // filter b"\x00" for a non-transaction block or when the wallet asked filters off
    //. `header_block_from_full_block` already carries b"\x00", so only
    // the tx-block + want_filter case computes. None = a store failure (no reply).
    // Coin-index tier: without the added/removed-at-height indexes the b"\x00" default stands
    // (that tier serves no wallet-sync surface at all).
    // A wallet without a v2 decoder cannot deserialize a v2 proof of space, and a light wallet does
    // not use the proof (it reads the header for the timestamp + filter). With `wallet_compat` set,
    // the header's proof is re-encoded in the v1 wire shape so `RespondBlockHeader` deserializes;
    // the stored block keeps its v2 proof.
    fn wallet_compat_header(
        &self,
        mut hb: dg_xch_core::blockchain::header_block::HeaderBlock,
    ) -> dg_xch_core::blockchain::header_block::HeaderBlock {
        if self.wallet_compat.load(Ordering::Relaxed) {
            let p = &hb.reward_chain_block.proof_of_space;
            hb.reward_chain_block.proof_of_space =
                dg_xch_core::blockchain::proof_of_space::ProofOfSpace::v1(
                    p.challenge,
                    p.pool_public_key,
                    p.pool_contract_puzzle_hash,
                    p.plot_public_key,
                    if p.size == 0 { 32 } else { p.size },
                    p.proof.clone(),
                );
        }
        hb
    }

    #[cfg(feature = "coin-index")]
    pub(crate) async fn served_header_block(
        &self,
        block: &FullBlock,
        want_filter: bool,
    ) -> Option<dg_xch_core::blockchain::header_block::HeaderBlock> {
        let mut hb = dg_xch_node::header_block_from_full_block(block);
        if want_filter && block.is_transaction_block() {
            let height = block.height();
            let added = self.store.get_coins_added_at_height(height).await.ok()?;
            let removed = self.store.get_coins_removed_at_height(height).await.ok()?;
            let mut items: Vec<Vec<u8>> = Vec::with_capacity(added.len() + removed.len());
            for cr in &added {
                items.push(cr.coin.puzzle_hash.bytes().to_vec());
            }
            for cr in &removed {
                items.push(cr.coin.name().bytes().to_vec());
            }
            hb.transactions_filter = dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
                chia_block_filter(&items),
            );
        }
        Some(self.wallet_compat_header(hb))
    }

    #[cfg(not(feature = "coin-index"))]
    async fn served_header_block(
        &self,
        block: &FullBlock,
        _want_filter: bool,
    ) -> Option<dg_xch_core::blockchain::header_block::HeaderBlock> {
        Some(self.wallet_compat_header(dg_xch_node::header_block_from_full_block(block)))
    }

    /// The initial `CoinState` set for a puzzle-hash subscription (`register_for_ph_updates`):
    /// spent + unspent coins carrying the SUBSCRIBED puzzle hashes from `min_height`, UNIONed
    /// (deduped by coin id) with coins HINTED by those same 32-byte values. ONE
    /// `max_subscribe_response_items` budget bounds the whole reply: the ph
    /// query runs under it, `max_items -= len(states)`, and the hint-id lookup runs under the
    /// remainder — so a dust-storm puzzle hash cannot materialize an unbounded set into one message.
    /// Truncation is SILENT to the wallet (logged, answered anyway). Empty on a node
    /// without the coin-index service tier (a non-wallet-serving node).
    #[cfg(feature = "coin-index")]
    async fn ph_initial_states(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        mut max_items: usize,
    ) -> Vec<CoinState> {
        let mut by_id: HashMap<Bytes32, CoinState> = HashMap::new();
        if let Ok(states) = self
            .store
            .get_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, true, max_items)
            .await
        {
            // The remaining budget after the ph query caps the hint side.
            max_items = max_items.saturating_sub(states.len());
            for s in states {
                by_id.insert(s.coin.name(), s);
            }
        }
        #[cfg(feature = "hint")]
        {
            // The hint-id lookup itself is budget-capped, decremented per hint.
            let mut hint_ids: Vec<Bytes32> = Vec::new();
            for ph in puzzle_hashes {
                let remaining = max_items.saturating_sub(hint_ids.len());
                if remaining == 0 {
                    break;
                }
                if let Ok(ids) = self.store.get_coins_for_hint(ph, remaining).await {
                    hint_ids.extend(ids);
                }
            }
            // The hinted states read with max_items = len(hint_coin_ids) —
            // the budget was already applied at the id lookup. No empty-guard needed:
            // get_coin_states_by_ids early-returns on an empty id list.
            if let Ok(states) = self
                .store
                .get_coin_states_by_ids(&hint_ids, min_height, true, hint_ids.len())
                .await
            {
                for s in states {
                    by_id.entry(s.coin.name()).or_insert(s);
                }
            }
        }
        if max_items == 0 {
            // Truncation posture: log it, answer anyway, signal nothing.
            info!(
                "RegisterForPhUpdates initial state truncated at max_subscribe_response_items states={} subscribed={}",
                by_id.len(),
                puzzle_hashes.len()
            );
        }
        by_id.into_values().collect()
    }

    #[cfg(not(feature = "coin-index"))]
    async fn ph_initial_states(
        &self,
        _puzzle_hashes: &[Bytes32],
        _min_height: u32,
        _max_items: usize,
    ) -> Vec<CoinState> {
        Vec::new()
    }
}
