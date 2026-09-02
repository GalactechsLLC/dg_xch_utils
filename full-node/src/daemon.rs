use crate::config::{Backend, Config};
use crate::metrics::{HealthState, MetricsSources, ProducerMetrics};
use crate::rpc::{NodeRpc, NodeRpcHandler};
use crate::trust::TrustPolicy;
use crate::tx_queue::TxQueue;
use crate::wallet::{LimitedSemaphore, WalletNotifier};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::blockchain::unfinished_header_block::UnfinishedHeaderBlock;
use dg_xch_core::blockchain::weight_proof::WeightProof;
use dg_xch_core::consensus::block_generator::GeneratorReference;
use dg_xch_core::consensus::block_header_validation::{
    ValidationState, validate_unfinished_header_block,
};
use dg_xch_core::consensus::constants::{ConsensusConstants, MAINNET, TESTNET_11};
use dg_xch_core::consensus::difficulty_adjustment::get_next_sub_slot_iters_and_difficulty;
use dg_xch_core::consensus::make_sub_epoch_summary::next_sub_epoch_summary;
use dg_xch_core::consensus::producer::{
    RewardBlockClaim, has_valid_pool_sig, splice_farmer_foliage_signatures,
    unfinished_block_to_full_block,
};
use dg_xch_core::errors::ChiaError;
use dg_xch_core::protocols::farmer::{
    DeclareProofOfSpace, NewSignagePoint, RequestSignedValues, SPSubSlotSourceData,
    SignagePointSourceData, SignedValues,
};
use dg_xch_core::protocols::full_node::{
    NewCompactVDF, NewPeak, NewSignagePointOrEndOfSubSlot, NewTransaction, NewUnfinishedBlock,
    NewUnfinishedBlock2, RequestCompactVDF, RequestProofOfWeight,
    RequestSignagePointOrEndOfSubSlot, RequestTransaction, RequestUnfinishedBlock,
    RequestUnfinishedBlock2, RespondCompactVDF, RespondEndOfSubSlot, RespondProofOfWeight,
    RespondSignagePoint,
};
use dg_xch_core::protocols::timelord::{
    NewEndOfSubSlotVDF, NewInfusionPointVDF, NewPeakTimelord, NewSignagePointVDF,
    NewUnfinishedBlockTimelord, RequestCompactProofOfTime, RespondCompactProofOfTime,
};
use dg_xch_core::protocols::wallet::{
    CoinState, FeeEstimate, FeeEstimateGroup, FeeRate, NewPeakWallet, PuzzleSolutionResponse,
    RegisterForCoinUpdates, RegisterForPhUpdates, RejectBlockHeaders, RejectHeaderBlocks,
    RejectStateReason, RequestCoinState, RequestFeeEstimates, RespondBlockHeaders,
    RespondCoinState, RespondHeaderBlocks, RespondToCoinUpdates, RespondToPhUpdates,
    SendTransaction, TransactionAck,
};
// The additions/removals/children served surface reads the coin-index secondary indexes; without the
// feature those store queries do not exist and the trait defaults (reject/empty) stand in.
use crate::peak_book::{ClaimGuard, PeakBook, PeakClaim};
#[cfg(feature = "coin-index")]
use dg_xch_core::blockchain::coin::Coin;
#[cfg(feature = "coin-index")]
use dg_xch_core::blockchain::coin_record::CoinRecord;
#[cfg(feature = "coin-index")]
use dg_xch_core::consensus::block_filter::chia_block_filter;
#[cfg(feature = "coin-index")]
use dg_xch_core::consensus::block_generator::hash_coin_ids;
#[cfg(feature = "coin-index")]
use dg_xch_core::consensus::merkle_set::MerkleSet;
#[cfg(feature = "coin-index")]
use dg_xch_core::protocols::wallet::{
    Additions, NamedCoin, RejectAdditionsRequest, RejectRemovalsRequest, RequestAdditions,
    RequestPuzzleState, RequestRemovals, RespondAdditions, RespondPuzzleState, RespondRemovals,
};
use dg_xch_core::protocols::{NodeType, PeerMap, SocketPeer};
use dg_xch_core::traits::SizedBytes;
use dg_xch_node::engine::BlockDelta;
use dg_xch_node::farmer::{
    AcceptedProof, CandidateBlockStore, CandidatePrev, DeclareVerdict, ProofCandidateStore,
    assemble_candidate, candidate_difficulty_and_ssi, new_signage_point_for_farmers,
    resolve_candidate_iters, validate_declared_proof,
};
use dg_xch_node::slots::{PeakSlotContext, SlotState};
use dg_xch_node::sync::queue::BlockQueue;
use dg_xch_node::sync::source::{
    BlockRangeSource, CapturingSource, OutboundPeerSource, request_weight_proof,
};
use dg_xch_node::sync::{WpForkPoint, wp_fork_point};
use dg_xch_node::unfinished::UnfinishedCache;
use dg_xch_node::{
    BlockRecordCache, Chaser, ConfirmedDelta, Engine, Mempool, NativePrimitives, NodeError,
    ReorgWalletDelta, SyncConfig, SyncError, SyncMetrics, validate_unfinished_block_body,
};
#[cfg(test)]
use dg_xch_p2p::P2pSettings;
#[cfg(feature = "coin-index")]
use dg_xch_p2p::{AdditionsReply, PuzzleStateReply, RemovalsReply};
use dg_xch_p2p::{
    BlockHeaderReply, BlockHeadersReply, CoinRegistration, CoinStateReply, FullNodeApi,
    HandlerFactory, HeaderBlocksReply, NetCounters, OutboundPeer, PhRegistration,
    SignagePointResponse, Supervisor, TransactionAnnounceAction, full_node_handlers_client_counted,
    full_node_handlers_counted,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_servers::rpc::{RpcServer, RpcServerConfig};
use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Error, ErrorKind};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// An in-flight gossip-transaction fetch (`full_node_store` `pending_tx_request`): WHEN we asked
// (for age-expiry) plus the ADVERTISED fee + cost from the peer's `NewTransaction`, carried to
// the untrusted tx-queue lane so it can order by fee-per-cost. The
// advertised values steer queue order only — admission always re-validates at the bundle's true fee.
#[derive(Clone, Copy)]
struct PendingTx {
    at: Instant,
    advertised_fee: u64,
    advertised_cost: u64,
}
// Tip-follow driver knobs: poll cadence + max blocks pulled per follow step (a hard bound on catch-up work).
const DRIVER_TICK: Duration = Duration::from_secs(2);
const FOLLOW_BATCH: u32 = 32;
// Fast-sync trigger: a claimed peak this many blocks ahead of a near-empty local store means tip-follow
// (FOLLOW_BATCH/step) would never converge — drive the weight-proof bulk sync instead. `local < GAP`
// gates the RECENT-CHAIN JUMP to a fresh/near-empty store (a mid-chain node long-syncs the gap through
// the batch pipeline instead — see `wants_long_sync`). The value doubles as the weight-proof anchor
// floor: WEIGHT_PROOF_RECENT_BLOCKS (1000, core/src/consensus/constants.rs) — a tip below it
// cannot be WP-anchored; from-zero batch sync covers that band.
const FAST_SYNC_GAP: u32 = 1000;
// `sync_blocks_behind_threshold` (300): a peer's claimed tip more than this many blocks ahead of
// the local peak enters the WP-anchored long-sync band, regardless of local height.
const SYNC_BLOCKS_BEHIND_THRESHOLD: u32 = 300;
// `short_sync_blocks_behind_threshold` (20): within this many blocks of a peer-announced peak the
// node follows block-by-block off the NewPeak event (the normal case of receiving the next
// block) rather than batch-syncing, so
// the confirmed peak tracks the tip within 0-1. The tip_follower owns this band; FOLLOW_BATCH catch-up
// and bulk_sync own the wider bands.
const SHORT_SYNC_BLOCKS_BEHIND_THRESHOLD: u32 = 20;
// Falling-edge hysteresis for the secondary-index shed (`update_synced`): shed only when the node
// is more than this many blocks behind the claimed network tip. The shed pays for itself only if
// the index-lean catch-up saves more than the one-time drop + CONCURRENTLY-rebuild round trip
// (hours over ~435M coin rows on slow storage), so the threshold must sit well past that
// crossover AND far outside every normal tip excursion: the raw `synced` bit decays on a 1-block
// dip or a restart wobble, and SYNC_BLOCKS_BEHIND_THRESHOLD-scale (300-block) long-syncs are
// routine — none of those may churn a multi-GB index set. 50,000 blocks (~10.8 days of mainnet
// tip advance at 4,608 blocks/day) is reachable only by a genuine outage or a leg whose confirm
// rate lost to the chain for days — exactly the deep re-catch-up phase the shed exists for, with
// the rebuild cost amortized over the tens of thousands of index-lean block applies that follow.
const SHED_TIP_LAG_BLOCKS: u32 = 50_000;
// Tip-follower safety re-check: a stored notify permit means an advance is never missed, so this is
// only a backstop against a lost wakeup.
const TIP_FOLLOW_IDLE: Duration = Duration::from_secs(2);
// Weight-proof fetch deadline: the proof is ~14 MB and the peer assembles it; generous vs the block timeout.
const WEIGHT_PROOF_TIMEOUT: Duration = Duration::from_secs(120);

/// Open the configured storage backend. Only the embedded SQLite backend is built today.
///
/// # Errors
/// Returns [`ErrorKind::Unsupported`] for a `postgres://` URL (the industrial backend is a dg_xch_stores
/// concern not yet landed), or an I/O error if the SQLite database cannot be opened/migrated.
pub async fn open_backend(backend: &Backend) -> Result<Arc<SqliteStore>, Error> {
    match backend {
        Backend::Sqlite(path) => {
            let store = SqliteStore::open(path)
                .await
                .map_err(|e| Error::other(format!("open sqlite {}: {e}", path.display())))?;
            Ok(Arc::new(store))
        }
        // The Postgres and mmap backends are constructed in main's dispatch (Node::boot_with_store);
        // this SQLite-typed helper only serves Node::boot's embedded path.
        Backend::Postgres(url) => Err(Error::new(
            ErrorKind::Unsupported,
            format!("postgres backend ({url}) boots via Node::boot_with_store, not open_backend"),
        )),
        Backend::Mmap(dir) => Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "mmap backend ({}) boots via Node::boot_with_store, not open_backend",
                dir.display()
            ),
        )),
    }
}

// Select consensus constants by network id. A fork's own constants would enter here (a later
// constants/genesis swap in core), touching no other crate boundary.
fn constants_for(network_id: &str) -> ConsensusConstants {
    match network_id {
        "testnet11" => TESTNET_11,
        _ => MAINNET,
    }
}

// The store-backed peer protocol surface: peers pull blocks from us (RequestBlock/RequestBlocks), fetch a
// mempool transaction, and announce their tip (NewPeak → recorded as a sync target). Blind to consensus.
struct StoreApi<S> {
    store: Arc<S>,
    mempool: Arc<Mutex<Mempool>>,
    constants: ConsensusConstants,
    claimed_peak: Arc<AtomicU32>,
    // The per-peer peak-claim book (`sync_store`): on_new_peak records the announcing peer's
    // (hash, height, weight) claim here; the sync bands select the heaviest verified-selectable claim.
    peak_book: Arc<PeakBook>,
    // OUTBOUND connections only: the minted per-connection claim key whose Drop retracts the claim
    // (`sync_store.peer_disconnected`). `None` on the shared inbound server api, which keys claims
    // by the real inbound peer id and reconciles them against the live inbound map each driver tick.
    claim_guard: Option<Arc<ClaimGuard>>,
    new_peak_signal: Arc<Notify>,
    // Live outbound peers as TimestampedPeerInfo — the RequestPeers gossip answer, refreshed by the driver.
    known_peers: Arc<RwLock<Vec<TimestampedPeerInfo>>>,
    tx_requested: Arc<Mutex<HashMap<Bytes32, PendingTx>>>,
    // The slot state machine — handlers read it to answer/filter SP gossip; only the
    // driver writes it (validation needs the record ancestry + next-SSI context).
    slot_state: Arc<Mutex<SlotState>>,
    // Received signage points / EOS bundles awaiting driver-side validation into the slot state.
    sp_inbox: Arc<Mutex<Vec<SpEvent>>>,
    // The unfinished-block cache (announce dedup + request tracking + serve path) and
    // the received-block inbox the driver validates through validate_unfinished_header_block.
    unfinished: Arc<Mutex<UnfinishedCache>>,
    ub_inbox: Arc<Mutex<Vec<UnfinishedBlock>>>,
    // Timelord infusion-return inbox (`new_infusion_point_vdf`): the infusion-point VDFs that finish
    // one of OUR cached unfinished blocks into a FullBlock. Queued off the read loop; the driver
    // (process_ip_inbox) assembles the FullBlock, runs it through the engine, and sets the new peak.
    ip_inbox: Arc<Mutex<Vec<NewInfusionPointVDF>>>,
    // The sync-status flag: slot/unfinished gossip is tip-context, so a deep-syncing node pulls
    // nothing it cannot validate (the ignore-while-syncing guard on these handlers).
    synced: Arc<AtomicBool>,
    // Simulator only: serve wallets a v1-shaped proof of space in headers (stock wallets cannot
    // deserialize a v2 proof). Off on a production node.
    wallet_compat: Arc<AtomicBool>,
    // Received bundles awaiting the validator worker (never validated on the read loop). A trusted
    // peer's bundle takes the high-priority lane (the high-priority lane).
    tx_inbox: Arc<Mutex<TxQueue>>,
    // Accepted transactions queued for NewTransaction re-broadcast — shared with the Node/rpc so a
    // wallet's p2p SendTransaction admission announces through the SAME seam as push_tx and the
    // gossip worker (the announce fires for every successful admission).
    tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    // txid -> (origin identity, when recorded): the peer a gossiped bundle arrived FROM, recorded at
    // receipt (`on_respond_transaction`) with its remote host so the announce drain can exclude an
    // OUTBOUND origin — whose dispatch id is our shared client-cert hash.
    // The SAME Arc the Node holds, so the drain sees receipt-time records. Bounded — record_tx_origin.
    tx_origin: Arc<Mutex<HashMap<Bytes32, (TxOrigin, Instant)>>>,
    // RequestProofOfWeight requests awaiting the weight-proof worker (built off the read path).
    wp_inbox: Arc<Mutex<Vec<WpRequest>>>,
    // Compact-VDF consume: pulled RespondCompactVDF proofs awaiting the driver's
    // validate + swap + re-gossip pass (a VDF verify never runs on the websocket read loop).
    compact_vdf_inbox: Arc<Mutex<Vec<RespondCompactVDF>>>,
    // Farmer interface: accepted DeclareProofOfSpace declarations, held as block candidates
    // until assembly consumes them. Bounded FIFO — evicting a stale candidate is harmless.
    proof_candidates: Arc<Mutex<ProofCandidateStore>>,
    // Candidate unfinished blocks awaiting the farmer's foliage signatures, keyed by quality
    // string. declare builds + stores here;
    // signed_values retrieves, splices the real signatures, and emits.
    candidates: Arc<Mutex<CandidateBlockStore>>,
    // Block-producer pipeline counters (the first-block funnel) — shared with Node + the
    // driver broadcasts + the /metrics scrape.
    producer: Arc<ProducerMetrics>,
    // Header hashes of unfinished blocks WE farmed (= FullBlock.header_hash = hash of the spliced
    // foliage), recorded at signed_values splice time so the follow driver can recognise our own block
    // when it confirms (S8). Bounded FIFO; a farmed block that never confirms just ages out.
    farmed_headers: Arc<Mutex<VecDeque<Bytes32>>>,
    // The wallet coin-state subscription registry (shared with the daemon's confirm path, which pushes
    // CoinStateUpdate deltas into it). RegisterForPh/CoinUpdates register here and get the delivery
    // receiver the dispatch layer bridges to the socket.
    wallet: Arc<WalletNotifier>,
    // The Node's consensus-walk record window + sync metrics, shared so the on-connect TIMELORD
    // greeting (`timelord_peak`) can run the same build as `broadcast_new_peak_timelord`.
    record_window: Arc<Mutex<BlockRecordCache>>,
    sync_metrics: Arc<SyncMetrics>,
    // The shared trusted-peer policy — resolves the register initial-state response budget
    // (`max_subscribe_response_items(peer)`) per-peer: the untrusted 100,000 by default, the
    // trusted 500,000 for a configured trusted peer. One budget is DECREMENTED across the puzzle-hash query and then the hint query of
    // a single RegisterForPhUpdates, and caps the RegisterForCoinUpdates initial read — so one
    // dust-storm puzzle hash cannot materialize an unbounded CoinState set into a single reply. Also
    // decides tx-queue priority (on_transaction) — the SAME `Arc` the WalletNotifier holds.
    trust: Arc<TrustPolicy>,
    // Bounds CONCURRENT heavy wallet-serve DB work (`wallet_sync_api_sem`):
    // shared node-wide across every peer's handler map; additions/removals acquire it and REJECT on
    // overflow. The read-loop rate limiter bounds message rate; this bounds the
    // concurrent block-delta scans those messages fan out into. Only the coin-index tier serves
    // additions/removals, hence unread (not unbounded) without that feature.
    #[cfg_attr(not(feature = "coin-index"), allow(dead_code))]
    wallet_sync_sem: Arc<LimitedSemaphore>,
}

// `max_duplicate_unfinished_blocks`: variants of one reward hash worth fetching.
const MAX_DUPLICATE_UNFINISHED_BLOCKS: usize = 3;

// Transaction-inbox bounds (per-peer queues under an aggregate cap).
const TX_INBOX_CAP: usize = 256;
const TX_INBOX_PER_PEER: usize = 32;

// How many of OUR farmed unfinished-block header hashes to remember for the S8 confirm match. A few
// slots of candidates is ample; a farmed block that never confirms just ages out of this FIFO.
const FARMED_HEADER_CAP: usize = 256;

// The wall-clock budget for assembling a block generator from the mempool on a winning declare —
// `block_creation_timeout` config default (2.0s).
const BLOCK_CREATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

// One received slot-gossip payload, queued for the driver's validation pass.
enum SpEvent {
    SignagePoint(Box<RespondSignagePoint>),
    EndOfSubSlot(Box<RespondEndOfSubSlot>),
}

// The received-gossip inbox cap: a peer cannot grow driver work without bound between ticks.
const SP_INBOX_CAP: usize = 256;

// The infusion-point inbox cap. One infusion-point VDF per farmed unfinished block per slot; a handful
// in flight between driver ticks is ample, and a full inbox drops the excess (the timelord re-sends on
// the next NewPeakTimelord). Bounds driver work a misbehaving timelord could otherwise grow without limit.
const IP_INBOX_CAP: usize = 64;

// One queued RequestProofOfWeight: who asked, over which link map, for which tip, under which
// request id (the requester's oneshot matches RespondProofOfWeight by type + this id).
struct WpRequest {
    peer: Bytes32,
    peers: PeerMap,
    tip: Bytes32,
    id: Option<u16>,
}

// The weight-proof request inbox cap. Building a proof walks sub-epochs of store history — a
// handful of queued requests is plenty, the rest drop (the peer retries or asks another node).
const WP_INBOX_CAP: usize = 8;

#[async_trait]
impl<S: BlockStore + CoinStore + Send + Sync + 'static> FullNodeApi for StoreApi<S> {
    async fn block_by_height(&self, height: u32) -> Option<Box<FullBlock>> {
        let rec = self
            .store
            .get_block_record_by_height(height)
            .await
            .ok()
            .flatten()?;
        self.store
            .get_block(&rec.header_hash)
            .await
            .ok()
            .flatten()
            .map(Box::new)
    }

    // — the RequestBlocks serving cap comes from the network's consensus
    // constants (MAX_BLOCK_COUNT_PER_REQUESTS = 32, override-tunable),
    // exactly like the request_header_blocks cap this daemon already enforces.
    fn max_block_count_per_requests(&self) -> u32 {
        self.constants.max_block_count_per_requests
    }

    // The decode-time list caps resolve from the trusted-peer policy — the same values the
    // handlers enforce post-parse, so decode truncation and handler truncation can never
    // disagree.
    fn max_subscriptions(&self, peer: &Bytes32, host: Option<IpAddr>) -> u32 {
        u32::try_from(self.wallet.max_subscriptions(peer, host)).unwrap_or(u32::MAX)
    }
    fn max_subscribe_response_items(&self, peer: &Bytes32, host: Option<IpAddr>) -> u32 {
        u32::try_from(self.trust.max_subscribe_response_items(peer, host)).unwrap_or(u32::MAX)
    }

    // An inbound TIMELORD is accepted from localhost or an exempt network only; the trusted-CIDR
    // list defaults empty, so localhost alone. Host-based only — node-id trust does not apply.
    fn accept_inbound_timelord(&self, host: Option<IpAddr>) -> bool {
        self.trust.host_trusted(host)
    }

    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        self.known_peers.read().await.clone()
    }

    async fn on_new_peak(&self, peer: Bytes32, peak: NewPeak) {
        // `full_node.new_peak`: record this peer's claim FIRST (sync_store.peer_has_block), weight
        // included — WEIGHT is the fork-choice ordering key, and the newest announcement REPLACES the
        // peer's previous claim (which is also how an over-claim is withdrawn). Outbound connections
        // key by the minted per-connection guard (the dispatch peer id there is our own cert hash);
        // the shared inbound api keys by the real inbound peer id.
        let (key, inbound) = match &self.claim_guard {
            Some(guard) => (guard.key(), false),
            None => (peer, true),
        };
        let changed = self.peak_book.record(
            key,
            inbound,
            PeakClaim {
                header_hash: peak.header_hash,
                height: peak.height,
                weight: peak.weight,
            },
        );
        // Event-driven near-tip follow: a change of the
        // heaviest claim wakes the tip_follower to close the gap block-by-block. notify_one stores a
        // permit if the follower is busy, so an advance is never missed.
        if changed {
            self.new_peak_signal.notify_one();
        }
    }

    async fn transaction(&self, id: Bytes32) -> Option<SpendBundle> {
        self.mempool.lock().await.spend_bundle(&id)
    }

    async fn on_new_transaction(
        &self,
        _peer: Bytes32,
        tx: NewTransaction,
    ) -> TransactionAnnounceAction {
        if !self.synced.load(Ordering::Relaxed) {
            return TransactionAnnounceAction::Ignore;
        }
        // Behavior (b) — "It's not reasonable to advertise a transaction with zero
        // cost." A zero-cost announcement is a protocol violation; ban the peer.
        if tx.cost == 0 {
            warn!(
                "banning peer: zero-cost transaction announcement id={}",
                tx.transaction_id
            );
            return TransactionAnnounceAction::Ban;
        }
        // Pre-filter: an announcement above the block cost ceiling can never be admitted, so it
        // is not worth a round trip. Ignore (not a ban — a mis-costed advert is not the
        // zero-cost violation).
        if tx.cost > self.constants.max_block_cost_clvm {
            return TransactionAnnounceAction::Ignore;
        }
        // Behavior (c) — if we already hold a VALIDATED mempool item for this id,
        // the announced cost/fee must match our own validation. One specific diff is tolerated:
        // pre-2.4.3 peers fold the quote's byte+execution cost into the advertised cost, so
        // `mempool_item.cost + (QUOTE_BYTES * COST_PER_BYTE + QUOTE_EXECUTION_COST)` is also
        // accepted. Any other cost, or any fee mismatch, is a ban.
        let seen = {
            let mempool = self.mempool.lock().await;
            mempool
                .get(&tx.transaction_id)
                .map(|item| (item.cost, item.fee))
        };
        if let Some((item_cost, item_fee)) = seen {
            const QUOTE_BYTES: u64 = 2;
            const QUOTE_EXECUTION_COST: u64 = 20;
            let tolerated = QUOTE_BYTES * self.constants.cost_per_byte + QUOTE_EXECUTION_COST;
            let cost_ok = tx.cost == item_cost || tx.cost == item_cost + tolerated;
            if !cost_ok || tx.fees != item_fee {
                warn!(
                    "banning peer: already-seen tx with mismatched cost/fee id={} advertised_cost={} validation_cost={} advertised_fee={} validation_fee={}",
                    tx.transaction_id, tx.cost, item_cost, tx.fees, item_fee
                );
                return TransactionAnnounceAction::Ban;
            }
            // Already seen and consistent (`return None` after the match check).
            return TransactionAnnounceAction::Ignore;
        }
        // Behavior (d) — `is_fee_enough`: the whole request path is
        // gated on the ADVERTISED fee being able to get in. With room in the pool anything
        // passes; at capacity the fee must clear the nonzero floor (5 fpc) and strictly beat the
        // pool's min fee rate — otherwise the bundle is never fetched (spam CLVM protection).
        if !self.mempool.lock().await.is_fee_enough(tx.fees, tx.cost) {
            return TransactionAnnounceAction::Ignore;
        }
        // New to us. A live (non-expired) entry means a fetch for this id is already in flight
        // from another peer: ignore the duplicate advert.
        // Otherwise record the request instant and pull.
        {
            let mut pending = self.tx_requested.lock().await;
            if pending
                .get(&tx.transaction_id)
                .is_some_and(|t| t.at.elapsed() < REQUEST_TIMEOUT)
            {
                return TransactionAnnounceAction::Ignore;
            }
            // Record the request instant AND this peer's advertised fee/cost — the untrusted
            // tx-queue lane orders on them. A later announcer for this id is deduped above, so the
            // first announcer's advertised values steer the order (not the max fpc across
            // announcers — a documented delta).
            pending.insert(
                tx.transaction_id,
                PendingTx {
                    at: Instant::now(),
                    advertised_fee: tx.fees,
                    advertised_cost: tx.cost,
                },
            );
        }
        TransactionAnnounceAction::Request(RequestTransaction {
            transaction_id: tx.transaction_id,
        })
    }

    async fn on_respond_transaction(&self, peer: Bytes32, host: Option<IpAddr>, tx: SpendBundle) {
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        // Behavior (d) — respond_transaction: a body is accepted only if it answers
        // a pull WE issued. `tx_requested` is our `pending_tx_request` set (a RequestTransaction
        // was sent for this id). An unsolicited body — no matching pending entry — is dropped
        // before it can reach the validator. `remove` consumes the entry, so a second copy of the
        // same id is then unsolicited too.
        let Ok(name) = tx.name() else {
            return;
        };
        let Some(pending) = self.tx_requested.lock().await.remove(&name) else {
            debug!(
                "dropping unsolicited transaction body id={} peer={}",
                name, peer
            );
            return;
        };
        // Record the origin (peer id + remote host) BEFORE queueing so the announce drain, which may
        // run concurrently with the validator worker's admission, can exclude the peer this bundle
        // arrived from. The host is what makes an OUTBOUND
        // origin excludable — its dispatch peer id is our shared client-cert hash. A bundle that
        // later fails admission produces no announcement, so its origin
        // is never consumed and simply ages out (bounded — see record_tx_origin).
        record_tx_origin(
            &self.tx_origin,
            name,
            TxOrigin {
                peer_id: peer,
                host,
            },
        )
        .await;
        // The CLVM run does NOT happen here — the websocket read loop must never carry
        // validation work. Bundles queue into the bounded inbox the
        // validator worker drains. A trusted peer's bundle takes the unbounded high-priority lane
        // (`TransactionQueue.put(high_priority=is_trusted(peer))`); an untrusted
        // bundle is admitted to the capped lane only within this peer's share, else dropped, and the
        // lane orders by the advertised fee-per-cost carried on the pending request.
        let high_priority = self.trust.is_trusted(&peer, host);
        self.tx_inbox.lock().await.push(
            peer,
            tx,
            high_priority,
            pending.advertised_fee,
            pending.advertised_cost,
        );
    }

    async fn on_new_signage_point_or_eos(
        &self,
        _peer: Bytes32,
        ann: NewSignagePointOrEndOfSubSlot,
    ) -> Option<RequestSignagePointOrEndOfSubSlot> {
        // Gated on sync mode: a syncing node's slot list anchors at its local
        // peak, so tip-context objects can never validate — skip the round trip.
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        // Pull only what the slot state does not hold and is not outdated. (A walk-up-to-30-EOS
        // backwards catch-up for a diverged slot list is a noted follow-up — a missed slot here
        // self-heals at the next confirmed peak.)
        let state = self.slot_state.lock().await;
        if ann.index_from_challenge == 0 {
            if state.get_sub_slot(&ann.challenge_hash).is_some() {
                return None;
            }
        } else if state
            .get_signage_point_by_index(
                &ann.challenge_hash,
                ann.index_from_challenge,
                &ann.last_rc_infusion,
            )
            .is_some()
        {
            return None;
        }
        if state.have_newer_signage_point(
            &ann.challenge_hash,
            ann.index_from_challenge,
            &ann.last_rc_infusion,
        ) {
            return None;
        }
        Some(RequestSignagePointOrEndOfSubSlot {
            challenge_hash: ann.challenge_hash,
            index_from_challenge: ann.index_from_challenge,
            last_rc_infusion: ann.last_rc_infusion,
        })
    }

    async fn signage_point_or_eos(
        &self,
        req: RequestSignagePointOrEndOfSubSlot,
    ) -> Option<SignagePointResponse> {
        // request_signage_point_or_end_of_sub_slot: index 0 serves the EOS bundle
        // itself, any other index the cached signage point built on the requested infusion.
        let state = self.slot_state.lock().await;
        if req.index_from_challenge == 0 {
            let (eos, _, _) = state.get_sub_slot(&req.challenge_hash)?;
            return Some(SignagePointResponse::EndOfSubSlot(Box::new(
                RespondEndOfSubSlot {
                    end_of_slot_bundle: eos.clone(),
                },
            )));
        }
        let sp = state.get_signage_point_by_index(
            &req.challenge_hash,
            req.index_from_challenge,
            &req.last_rc_infusion,
        )?;
        // index > 0 (guaranteed by the branch above) always resolves a stored SP with real VDFs;
        // RespondSignagePoint carries non-optional VDFs, so the
        // `?` here only guards a malformed all-None SP, which get_signage_point_by_index never returns.
        Some(SignagePointResponse::SignagePoint(Box::new(
            RespondSignagePoint {
                index_from_challenge: req.index_from_challenge,
                challenge_chain_vdf: sp.cc_vdf?,
                challenge_chain_proof: sp.cc_proof.clone()?,
                reward_chain_vdf: sp.rc_vdf?,
                reward_chain_proof: sp.rc_proof.clone()?,
            },
        )))
    }

    async fn on_respond_signage_point(&self, _peer: Bytes32, sp: RespondSignagePoint) {
        let mut inbox = self.sp_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(SpEvent::SignagePoint(Box::new(sp)));
        }
    }

    async fn on_respond_end_of_sub_slot(&self, _peer: Bytes32, eos: RespondEndOfSubSlot) {
        let mut inbox = self.sp_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(SpEvent::EndOfSubSlot(Box::new(eos)));
        }
    }

    async fn on_new_unfinished_block(
        &self,
        _peer: Bytes32,
        ann: NewUnfinishedBlock,
    ) -> Option<RequestUnfinishedBlock> {
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        // The v1 announce carries no foliage hash — pull only when we hold and request nothing
        // for this reward hash.
        let mut cache = self.unfinished.lock().await;
        if cache.get_block(&ann.unfinished_reward_hash).is_some() {
            return None;
        }
        let (requesting, count) = cache.is_requesting(&ann.unfinished_reward_hash, None);
        if requesting || count > 0 {
            return None;
        }
        cache.mark_requesting(ann.unfinished_reward_hash, None);
        Some(RequestUnfinishedBlock {
            unfinished_reward_hash: ann.unfinished_reward_hash,
        })
    }

    async fn on_new_unfinished_block2(
        &self,
        _peer: Bytes32,
        ann: NewUnfinishedBlock2,
    ) -> Option<RequestUnfinishedBlock2> {
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        // `new_unfinished_block2`'s admission ladder: already held, a better variant held,
        // too many variants held, already fetching, or too many fetches in flight — all ignore.
        let mut cache = self.unfinished.lock().await;
        let (entry, count, have_better) =
            cache.get_block2(&ann.unfinished_reward_hash, ann.foliage_hash.as_ref());
        if entry.is_some() || have_better || count > MAX_DUPLICATE_UNFINISHED_BLOCKS {
            return None;
        }
        let (requesting, count) =
            cache.is_requesting(&ann.unfinished_reward_hash, ann.foliage_hash.as_ref());
        if requesting || count >= MAX_DUPLICATE_UNFINISHED_BLOCKS {
            return None;
        }
        cache.mark_requesting(ann.unfinished_reward_hash, ann.foliage_hash);
        Some(RequestUnfinishedBlock2 {
            unfinished_reward_hash: ann.unfinished_reward_hash,
            foliage_hash: ann.foliage_hash,
        })
    }

    async fn unfinished_block(&self, reward_hash: Bytes32) -> Option<Box<UnfinishedBlock>> {
        self.unfinished
            .lock()
            .await
            .get_block(&reward_hash)
            .cloned()
            .map(Box::new)
    }

    async fn unfinished_block2(
        &self,
        reward_hash: Bytes32,
        foliage_hash: Option<Bytes32>,
    ) -> Option<Box<UnfinishedBlock>> {
        self.unfinished
            .lock()
            .await
            .get_block2(&reward_hash, foliage_hash.as_ref())
            .0
            .cloned()
            .map(Box::new)
    }

    async fn on_respond_unfinished_block(&self, block: Box<UnfinishedBlock>) {
        let mut inbox = self.ub_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(*block);
        }
    }

    async fn mempool_items(&self, filter: Vec<u8>) -> Vec<NewTransaction> {
        // Decode the peer's BIP158 filter and serve up to `limit` (100) highest-fee items NOT in
        // it, scanning at most `max_checked` (5000). A malformed filter decodes to None and we
        // serve unfiltered — over-announcing is the safe superset (the peer's own dedup absorbs it).
        let decoded = dg_xch_core::consensus::block_filter::decode_chia_block_filter(&filter);
        let mp = self.mempool.lock().await;
        let mut out = Vec::new();
        for (checked, item) in mp.items_by_fee().into_iter().enumerate() {
            if out.len() >= 100 || checked >= 5000 {
                break;
            }
            let name_bytes = SizedBytes::bytes(&item.name);
            if let Some(decoded) = &decoded
                && dg_xch_core::consensus::block_filter::chia_block_filter_match(
                    decoded,
                    &name_bytes,
                )
            {
                continue;
            }
            out.push(NewTransaction {
                transaction_id: item.name,
                cost: item.cost,
                fees: item.fee,
            });
        }
        out
    }

    async fn on_request_proof_of_weight(
        &self,
        peer: Bytes32,
        req: RequestProofOfWeight,
        id: Option<u16>,
        peers: PeerMap,
    ) {
        // Queue-only, off the read path: the wp worker builds (single-flight per tip inside the
        // WeightProofServer's lock) and responds. Bounded + deduped — a repeat
        // {peer, tip} would be one build + one response anyway.
        let mut inbox = self.wp_inbox.lock().await;
        if inbox.len() >= WP_INBOX_CAP {
            return;
        }
        if inbox.iter().any(|r| r.peer == peer && r.tip == req.tip) {
            return;
        }
        inbox.push(WpRequest {
            peer,
            peers,
            tip: req.tip,
            id,
        });
    }

    async fn compact_vdf(&self, req: RequestCompactVDF) -> Option<RespondCompactVDF> {
        // SERVE (`full_node.request_compact_vdf`): the height's main-chain block whose header
        // hash the requester named; answer only when OUR proof for that field is already compact.
        let block = self.block_by_height(req.height).await?;
        if block.header_hash().ok()? != req.header_hash {
            return None;
        }
        let proof = dg_xch_node::compact_vdf::serve_compact(&block, req.field_vdf, &req.vdf_info)?;
        Some(RespondCompactVDF {
            height: req.height,
            header_hash: req.header_hash,
            field_vdf: req.field_vdf,
            vdf_info: req.vdf_info,
            vdf_proof: proof,
        })
    }

    async fn on_new_compact_vdf(
        &self,
        _peer: Bytes32,
        ann: NewCompactVDF,
    ) -> Option<RequestCompactVDF> {
        // `new_compact_vdf`: ignore while syncing (tip-context), ignore blocks within 5 of our
        // peak ("will not compactify recent block"), and pull only when we still hold that exact
        // field/VdfInfo bulky (needs_compact_proof). Otherwise stay silent.
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        let (_, peak_height) = self.store.get_peak().await.ok().flatten()?;
        if peak_height.saturating_sub(ann.height) < 5 {
            return None;
        }
        let block = self.block_by_height(ann.height).await?;
        if block.header_hash().ok()? != ann.header_hash {
            return None;
        }
        if !dg_xch_node::compact_vdf::needs_compact_proof(&block, ann.field_vdf, &ann.vdf_info) {
            return None;
        }
        Some(RequestCompactVDF {
            height: ann.height,
            header_hash: ann.header_hash,
            field_vdf: ann.field_vdf,
            vdf_info: ann.vdf_info,
        })
    }

    async fn on_respond_compact_vdf(&self, _peer: Bytes32, resp: RespondCompactVDF) {
        // Queue for the driver: validation (a VDF verify) and the block re-write never run on the
        // websocket read loop. Bounded like every other received-gossip inbox.
        let mut inbox = self.compact_vdf_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(resp);
        }
    }

    async fn on_respond_compact_proof_of_time(
        &self,
        _peer: Bytes32,
        resp: RespondCompactProofOfTime,
    ) {
        // A bluebox timelord's answer to our solicitation. It carries the same five fields as a
        // RespondCompactVDF, so map it and feed the identical consume inbox — the driver's
        // process_compact_vdf_inbox validates + swaps under the same-header-hash guard + re-gossips
        // NewCompactVDF. No new validate/replace surface.
        let mapped = RespondCompactVDF {
            height: resp.height,
            header_hash: resp.header_hash,
            field_vdf: resp.field_vdf,
            vdf_info: resp.vdf_info,
            vdf_proof: resp.vdf_proof,
        };
        let mut inbox = self.compact_vdf_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(mapped);
        }
    }

    async fn on_declare_proof_of_space(
        &self,
        peer: Bytes32,
        declare: DeclareProofOfSpace,
    ) -> Option<RequestSignedValues> {
        // S1 — a declare arrived AT ALL (distinguishes never-received from received-then-dropped).
        self.producer.declare_received();
        info!(
            "declare_proof_of_space received event={} peer={} sp_index={} challenge={} cc_sp={}",
            "producer.declare.received",
            peer,
            declare.signage_point_index,
            declare.challenge_hash,
            declare.challenge_chain_sp
        );
        // `full_node_api.declare_proof_of_space` — declare validation is tip-context; a
        // syncing node has no consistent slot state to check against, so it drops the message.
        if !self.synced.load(Ordering::Relaxed) {
            // Promoted trace!->info! for bring-up: at default level the operator must
            // see this wall.
            self.producer.validated("not_synced");
            info!(
                "declare dropped: node not synced (tip-context validation impossible) event={} peer={}",
                "producer.declare.not_synced", peer
            );
            return None;
        }
        // The plot filter is height-dependent (hard-fork sizing). The farmer proved against the
        // network tip it sees; the highest peer-announced tip is our closest match to that view.
        let height = self.claimed_peak.load(Ordering::Relaxed);
        // The two slot-state lookups run synchronously under one guard — the proof verify itself is
        // CPU-only (no I/O), so holding the slot lock across it is bounded and avoids a TOCTOU on the
        // SP set. This is the read loop, but a single PoSpace verify is cheap (unlike a VDF verify,
        // which we always defer).
        let verdict = {
            let slot = self.slot_state.lock().await;
            validate_declared_proof(
                &self.constants,
                &declare,
                height,
                |cc_sp| slot.get_signage_point(cc_sp),
                |cc| slot.get_sub_slot(cc).is_some(),
            )
        };
        match verdict {
            DeclareVerdict::Accepted(quality_string) => {
                self.producer.validated("accepted");
                // An accepted proof is always held as a candidate proof, independent of
                // whether we can assemble a full block from it yet.
                self.proof_candidates.lock().await.insert(AcceptedProof {
                    declare: declare.clone(),
                    quality_string,
                });
                info!(
                    "accepted proof of space, held as candidate event={} peer={} qs={}",
                    "producer.declare.accepted", peer, quality_string
                );
                // Assemble the candidate unfinished block (placeholder foliage
                // signatures + the SP signatures from THIS declare message), store it keyed by the
                // quality string, and return the RequestSignedValues for the farmer to sign the two
                // foliage hashes. Returns None (no emit) when the candidate cannot yet be assembled
                // from the reachable slot/block state — see try_build_candidate's per-reason drops.
                let request = self.try_build_candidate(&declare, quality_string).await;
                // S4 — a candidate was built and we are about to send RequestSignedValues back.
                if request.is_some() {
                    self.producer.request_signed_values();
                }
                request
            }
            other => {
                let result = other.result_label();
                self.producer.validated(result);
                info!(
                    "declare rejected at validate_declared_proof event={} peer={} result={} sp_index={} challenge={}",
                    "producer.declare.rejected",
                    peer,
                    result,
                    declare.signage_point_index,
                    declare.challenge_hash
                );
                None
            }
        }
    }

    async fn on_signed_values(&self, peer: Bytes32, signed: SignedValues) {
        // FARMER→NODE (`full_node_api.signed_values`): the farmer's real foliage signatures for a
        // candidate we asked it to sign. Retrieve the candidate, verify the foliage_block_data
        // signature against the plot key (a mismatch means a plot
        // collision), splice both signatures into the foliage, and propagate the finished block.
        // S5 — the farmer signed back.
        self.producer.signed_values();
        let Some((height, mut candidate)) = self
            .candidates
            .lock()
            .await
            .get(&signed.quality_string)
            .cloned()
        else {
            self.producer
                .candidate_dropped("signed_values_no_candidate");
            warn!(
                "signed_values: no candidate for this quality string (evicted or unknown) event={} reason={} qs={} peer={}",
                "producer.signed.dropped",
                "signed_values_no_candidate",
                signed.quality_string,
                peer
            );
            return;
        };
        // Verify(plot_public_key, foliage_block_data.get_hash(), fbd_signature).
        let plot_pk = candidate.reward_chain_block.proof_of_space.plot_public_key;
        let Ok(fbd_hash) = candidate.foliage.foliage_block_data.hash() else {
            self.producer.candidate_dropped("foliage_hash_fail");
            warn!(
                "signed_values: candidate foliage_block_data failed to hash event={} reason={} qs={}",
                "producer.signed.dropped", "foliage_hash_fail", signed.quality_string
            );
            return;
        };
        if !dg_xch_core::consensus::producer::verify_plot_signature(
            &plot_pk,
            fbd_hash,
            &signed.foliage_block_data_signature,
        ) {
            // Stays warn! — an invalid foliage signature is a plot collision, genuinely alarming.
            self.producer.candidate_dropped("sig_verify_fail");
            warn!(
                "signed_values: foliage_block_data signature invalid (plot collision?); dropping event={} reason={} qs={} peer={}",
                "producer.signed.dropped", "sig_verify_fail", signed.quality_string, peer
            );
            return;
        }
        // Splice the foliage_block_data signature and, for a tx block, the
        // foliage_transaction_block signature.
        splice_farmer_foliage_signatures(
            &mut candidate,
            signed.foliage_block_data_signature,
            signed.foliage_transaction_block_signature,
        );
        // The add-unfinished-block latency guard: drop the block
        // if it would be infused before the current finished head (block.total_iters < peak.total_iters).
        if let Ok(Some((peak_hash, _))) = self.store.get_peak().await
            && let Ok(Some(peak_rec)) = self.store.get_block_record(&peak_hash).await
            && candidate.reward_chain_block.total_iters < peak_rec.total_iters
        {
            self.producer.candidate_dropped("latency_drop_signed");
            warn!(
                "dropping farmed unfinished block: would infuse before the current head (latency) event={} reason={} qs={} sp_index={} block_total_iters={} head_total_iters={}",
                "producer.signed.dropped",
                "latency_drop_signed",
                signed.quality_string,
                candidate.reward_chain_block.signage_point_index,
                candidate.reward_chain_block.total_iters,
                peak_rec.total_iters
            );
            return;
        }
        // S5 success — a finished unfinished block. `partial` is the S6/S7 join key; `header` (the hash
        // of the now-signed foliage) is the FullBlock header hash, recorded so the follow driver counts
        // this block when it confirms (S8). The foliage is fixed from here — infusion never touches it.
        self.producer.ub_assembled();
        let partial = candidate.reward_chain_block.hash().ok();
        if let Ok(bytes) = candidate
            .foliage
            .to_bytes(dg_xch_serialize::ChiaProtocolVersion::default())
        {
            let header = Bytes32::from(dg_xch_core::utils::hash_256(bytes));
            let mut farmed = self.farmed_headers.lock().await;
            farmed.push_back(header);
            while farmed.len() > FARMED_HEADER_CAP {
                farmed.pop_front();
            }
        }
        info!(
            "farmed unfinished block: signatures spliced, propagating event={} height={} qs={} partial={:?}",
            "producer.signed.spliced", height, signed.quality_string, partial
        );
        let mut inbox = self.ub_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(candidate);
        } else {
            // Was a silent drop on the floor: a spliced, ready-to-infuse block lost with no trace.
            self.producer.candidate_dropped("ub_inbox_full");
            warn!(
                "farmed unfinished block dropped: ub_inbox at cap event={} reason={} qs={}",
                "producer.signed.dropped", "ub_inbox_full", signed.quality_string
            );
        }
    }

    async fn on_new_infusion_point_vdf(&self, peer: Bytes32, req: NewInfusionPointVDF) {
        // `full_node_api.new_infusion_point_vdf` — `if sync_store.get_sync_mode(): return None`.
        // A syncing node has no consistent slot/unfinished state to finish a block against.
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        // Queue only — the assembly (unfinished-block lookup + reward-chain backtrack + finished-sub-slot
        // walk + engine add_block/set-peak) runs on the driver, never the websocket read loop.
        let mut inbox = self.ip_inbox.lock().await;
        if inbox.len() < IP_INBOX_CAP {
            inbox.push(req);
        } else {
            warn!("dropping infusion-point VDF: ip_inbox at cap peer={}", peer);
        }
    }

    async fn on_new_signage_point_vdf(&self, peer: Bytes32, req: NewSignagePointVDF) {
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        let sp = RespondSignagePoint {
            index_from_challenge: req.index_from_challenge,
            challenge_chain_vdf: req.challenge_chain_sp_vdf,
            challenge_chain_proof: req.challenge_chain_sp_proof,
            reward_chain_vdf: req.reward_chain_sp_vdf,
            reward_chain_proof: req.reward_chain_sp_proof,
        };
        self.on_respond_signage_point(peer, sp).await;
    }

    async fn on_new_end_of_sub_slot_vdf(&self, peer: Bytes32, req: NewEndOfSubSlotVDF) {
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        let Ok(cc_hash) = req.end_of_sub_slot_bundle.challenge_chain.hash() else {
            return;
        };
        if self
            .slot_state
            .lock()
            .await
            .get_sub_slot(&cc_hash)
            .is_some()
        {
            return;
        }
        self.on_respond_end_of_sub_slot(
            peer,
            RespondEndOfSubSlot {
                end_of_slot_bundle: req.end_of_sub_slot_bundle,
            },
        )
        .await;
    }

    // ---- light-wallet query surface (wallet handlers) ---------------------------

    // `request_puzzle_solution`: the (puzzle, solution) of a coin spent at
    // `height`, recovered by re-running that block's generator. Reuses the ONE tested extraction path
    // shared with the HTTP RPC (never a second VM); any miss maps to a RejectPuzzleSolution on the wire.
    async fn puzzle_solution(
        &self,
        coin_name: Bytes32,
        height: u32,
    ) -> Option<PuzzleSolutionResponse> {
        let spend = crate::rpc::puzzle_and_solution_coin_spend(
            self.store.as_ref(),
            &self.constants,
            &coin_name,
            height,
        )
        .await
        .ok()?;
        Some(PuzzleSolutionResponse {
            coin_name,
            height,
            puzzle: spend.puzzle_reveal,
            solution: spend.solution,
        })
    }

    async fn send_transaction(&self, _peer: Bytes32, tx: SendTransaction) -> TransactionAck {
        let txid = tx.transaction.name().unwrap_or_default();
        if !self.synced.load(Ordering::Relaxed) {
            return TransactionAck {
                txid,
                status: TXStatus::FAILED,
                error: Some("NO_TRANSACTIONS_WHILE_SYNCING".to_string()),
            };
        }
        match crate::tx_admission::admit_spend_bundle(
            self.store.as_ref(),
            &self.mempool,
            &self.constants,
            &self.tx_announce,
            tx.transaction,
        )
        .await
        {
            // SUCCESS acks with error=None.
            Ok(name) => TransactionAck {
                txid: name,
                status: TXStatus::SUCCESS,
                error: None,
            },
            Err(e) => {
                let (status, err_name) = e.ack();
                debug!("send_transaction rejected txid={} error={}", txid, e);
                TransactionAck {
                    txid,
                    status,
                    error: Some(err_name.to_string()),
                }
            }
        }
    }

    // `request_block_header`: the HeaderBlock at a height. An unknown height
    // rejects; a record with no stored body stays silent. The header carries the REAL BIP158
    // transactions_filter (G3 closed — see served_header_block).
    async fn block_header(&self, height: u32) -> BlockHeaderReply {
        let record = match self.store.get_block_record_by_height(height).await {
            Ok(Some(r)) => r,
            _ => return BlockHeaderReply::Reject(height),
        };
        match self.store.get_block(&record.header_hash).await {
            Ok(Some(block)) => match self.served_header_block(&block, true).await {
                Some(hb) => BlockHeaderReply::Respond(Box::new(hb)),
                None => BlockHeaderReply::Silent,
            },
            _ => BlockHeaderReply::Silent,
        }
    }

    // `request_header_blocks` (DEPRECATED, code 60): header blocks in
    // [start, end]. A bad/oversized range is silent; an unknown height in range rejects.
    // Headers carry the real filter, built from the coin store per block.
    async fn header_blocks(&self, start_height: u32, end_height: u32) -> HeaderBlocksReply {
        if end_height < start_height
            || end_height - start_height > self.constants.max_block_count_per_requests
        {
            return HeaderBlocksReply::Silent;
        }
        let reject = || {
            HeaderBlocksReply::Reject(RejectHeaderBlocks {
                start_height,
                end_height,
            })
        };
        let mut header_blocks = Vec::new();
        for h in start_height..=end_height {
            let Ok(Some(record)) = self.store.get_block_record_by_height(h).await else {
                return reject();
            };
            let Ok(Some(block)) = self.store.get_block(&record.header_hash).await else {
                return reject();
            };
            let Some(hb) = self.served_header_block(&block, true).await else {
                return reject();
            };
            header_blocks.push(hb);
        }
        HeaderBlocksReply::Respond(Box::new(RespondHeaderBlocks {
            start_height,
            end_height,
            header_blocks,
        }))
    }

    // `request_block_headers` (the streamed shape, code 86): header blocks in
    // [start, end], capped at 128. A bad range or a missing block rejects. return_filter is honored:
    // false serves the encoded-empty filter b"\x00", true serves the real per-block filter from
    // the coin store.
    async fn block_headers(
        &self,
        start_height: u32,
        end_height: u32,
        return_filter: bool,
    ) -> BlockHeadersReply {
        let reject = || {
            BlockHeadersReply::Reject(RejectBlockHeaders {
                start_height,
                end_height,
            })
        };
        if end_height < start_height || end_height - start_height > 128 {
            return reject();
        }
        let mut header_blocks = Vec::new();
        for h in start_height..=end_height {
            let Ok(Some(record)) = self.store.get_block_record_by_height(h).await else {
                return reject();
            };
            let Ok(Some(block)) = self.store.get_block(&record.header_hash).await else {
                return reject();
            };
            let Some(hb) = self.served_header_block(&block, return_filter).await else {
                return reject();
            };
            header_blocks.push(hb);
        }
        BlockHeadersReply::Respond(Box::new(RespondBlockHeaders {
            start_height,
            end_height,
            header_blocks,
        }))
    }

    // `request_additions`: coins created at a block, grouped by puzzle hash.
    // puzzle_hashes = None → all additions, proofs = None (the trusted-wallet path);
    // puzzle_hashes = Some → per-hash coins plus MerkleSet INCLUSION/EXCLUSION proofs against the
    // foliage additions_root (leaf pairs [puzzle_hash, hash_coin_ids(coin names)]), which an
    // untrusted wallet verifies against the block header.
    // coin-index tier only.
    #[cfg(feature = "coin-index")]
    async fn additions(&self, req: RequestAdditions) -> AdditionsReply {
        let reject = |header_hash: Bytes32| {
            AdditionsReply::Reject(RejectAdditionsRequest {
                height: req.height,
                header_hash,
            })
        };
        if req
            .puzzle_hashes
            .as_ref()
            .is_some_and(|p| p.len() > MAX_COIN_HASHES_PER_REQUEST)
        {
            return reject(req.header_hash.unwrap_or_default());
        }
        // Resolve + fork-check the header hash (height_to_hash(height) == header_hash).
        let Ok(Some(confirmed)) = self.store.get_block_record_by_height(req.height).await else {
            return reject(req.header_hash.unwrap_or_default());
        };
        let header_hash = req.header_hash.unwrap_or(confirmed.header_hash);
        if header_hash != confirmed.header_hash {
            return reject(header_hash);
        }
        // Empty proof request: no DB + Merkle work — answer coins=[] with proofs=[]
        // (Some-empty, NOT None).
        if req.puzzle_hashes.as_ref().is_some_and(Vec::is_empty) {
            return AdditionsReply::Respond(Box::new(RespondAdditions {
                height: req.height,
                header_hash,
                coins: Vec::new(),
                proofs: Some(Vec::new()),
            }));
        }
        // The block-delta scan is guarded by wallet_sync_sem (active=2, waiting=20) and REJECTS
        // on overflow — concurrent heavy wallet serves are bounded, never queued without limit.
        let Ok(_permit) = self.wallet_sync_sem.acquire().await else {
            return reject(header_hash);
        };
        let Ok(added) = self.store.get_coins_added_at_height(req.height).await else {
            return reject(header_hash);
        };
        // Reorg guard: the DB read may straddle a reorg — re-check height→hash.
        match self.store.get_block_record_by_height(req.height).await {
            Ok(Some(r)) if r.header_hash == header_hash => {}
            _ => return reject(header_hash),
        }
        // puzzle hash → coins, in additions insertion order (the trusted-path response + the
        // proof leaf pairs both iterate it).
        let mut order: Vec<Bytes32> = Vec::new();
        let mut map: HashMap<Bytes32, Vec<Coin>> = HashMap::new();
        for cr in added {
            let entry = map.entry(cr.coin.puzzle_hash).or_default();
            if entry.is_empty() {
                order.push(cr.coin.puzzle_hash);
            }
            entry.push(cr.coin);
        }
        match req.puzzle_hashes {
            None => {
                // Only the serve-everything map is bounded.
                if map.len() > MAX_COINS_MAP_SIZE {
                    return reject(header_hash);
                }
                let coins: Additions = order
                    .iter()
                    .map(|ph| (*ph, map.remove(ph).unwrap_or_default()))
                    .collect();
                AdditionsReply::Respond(Box::new(RespondAdditions {
                    height: req.height,
                    header_hash,
                    coins,
                    proofs: None,
                }))
            }
            Some(ref puzzle_hashes) => {
                // The addition merkle set: [puzzle_hash, hash_coin_ids(coin names)] leaf pairs —
                // its root IS the foliage additions_root.
                let mut leafs: Vec<[u8; 32]> = Vec::with_capacity(2 * order.len());
                for ph in &order {
                    leafs.push(ph.bytes());
                    let names: Vec<[u8; 32]> = map[ph].iter().map(|c| c.name().bytes()).collect();
                    leafs.push(hash_coin_ids(&names));
                }
                let addition_merkle_set = MerkleSet::from_leafs(&mut leafs);
                let mut coins_map: Additions = Vec::with_capacity(puzzle_hashes.len());
                let mut proofs_map: Vec<(Bytes32, Vec<u8>, Option<Vec<u8>>)> =
                    Vec::with_capacity(puzzle_hashes.len());
                for ph in puzzle_hashes {
                    // INCLUSION if the hash is in the set, EXCLUSION otherwise (
                    // its asserts hold structurally here — the set is built from the same map —
                    // so a mismatch is a corrupt read: reject, never a panic).
                    let Ok((included, proof)) = addition_merkle_set.generate_proof(&ph.bytes())
                    else {
                        return reject(header_hash);
                    };
                    if let Some(coins) = map.get(ph) {
                        let names: Vec<[u8; 32]> = coins.iter().map(|c| c.name().bytes()).collect();
                        let coin_ids_hash = hash_coin_ids(&names);
                        let Ok((included_2, proof_2)) =
                            addition_merkle_set.generate_proof(&coin_ids_hash)
                        else {
                            return reject(header_hash);
                        };
                        if !included || !included_2 {
                            return reject(header_hash);
                        }
                        coins_map.push((*ph, coins.clone()));
                        proofs_map.push((*ph, proof, Some(proof_2)));
                    } else {
                        if included {
                            return reject(header_hash);
                        }
                        coins_map.push((*ph, Vec::new()));
                        proofs_map.push((*ph, proof, None));
                    }
                }
                AdditionsReply::Respond(Box::new(RespondAdditions {
                    height: req.height,
                    header_hash,
                    coins: coins_map,
                    proofs: Some(proofs_map),
                }))
            }
        }
    }

    // `request_removals`: coins spent at a block. coin_names = None (or
    // Some-empty, ) → all removals, proofs = None; coin_names = Some → per-name coins plus
    // MerkleSet INCLUSION/EXCLUSION proofs over the removal names, whose root is asserted equal to
    // the foliage removals_root before serving. coin-index tier only.
    #[cfg(feature = "coin-index")]
    async fn removals(&self, req: RequestRemovals) -> RemovalsReply {
        let reject = || {
            RemovalsReply::Reject(RejectRemovalsRequest {
                height: req.height,
                header_hash: req.header_hash,
            })
        };
        if req
            .coin_names
            .as_ref()
            .is_some_and(|n| n.len() > MAX_COIN_HASHES_PER_REQUEST)
        {
            return reject();
        }
        // The whole block-fetch + removal scan is guarded by wallet_sync_sem and REJECTS on
        // overflow.
        let Ok(_permit) = self.wallet_sync_sem.acquire().await else {
            return reject();
        };
        let Ok(Some(block)) = self.store.get_block(&req.header_hash).await else {
            return reject();
        };
        let peak_height = self.store.get_peak().await.ok().flatten().map(|(_, h)| h);
        let confirmed = self
            .store
            .get_block_record_by_height(req.height)
            .await
            .ok()
            .flatten()
            .map(|r| r.header_hash);
        // The four reject conditions: not a tx block, height mismatch, above peak, or a fork.
        if !block.is_transaction_block()
            || block.height() != req.height
            || peak_height.is_some_and(|ph| block.height() > ph)
            || confirmed != Some(req.header_hash)
        {
            return reject();
        }
        // No generator = reward-only tx block: empty removals (proofs None for a None request,
        // Some-empty when coin names were asked).
        if block.transactions_generator.is_none() {
            let proofs = if req.coin_names.is_none() {
                None
            } else {
                Some(Vec::new())
            };
            return RemovalsReply::Respond(Box::new(RespondRemovals {
                height: block.height(),
                header_hash: req.header_hash,
                coins: Vec::new(),
                proofs,
            }));
        }
        let Ok(removed) = self.store.get_coins_removed_at_height(block.height()).await else {
            return reject();
        };
        // Reorg guard: the DB read may straddle a reorg — re-check height→hash.
        match self.store.get_block_record_by_height(block.height()).await {
            Ok(Some(r)) if r.header_hash == req.header_hash => {}
            _ => return reject(),
        }
        match req.coin_names.as_deref() {
            // Trusted path — Some-empty behaves exactly like None.
            None | Some([]) => {
                let coins: Vec<NamedCoin> = removed
                    .into_iter()
                    .map(|cr| (cr.coin.name(), Some(cr.coin)))
                    .collect();
                RemovalsReply::Respond(Box::new(RespondRemovals {
                    height: block.height(),
                    header_hash: req.header_hash,
                    coins,
                    proofs: None,
                }))
            }
            Some(coin_names) => {
                // name → coin in removals order.
                let mut order: Vec<Bytes32> = Vec::with_capacity(removed.len());
                let mut by_name: HashMap<Bytes32, Coin> = HashMap::with_capacity(removed.len());
                for cr in removed {
                    let name = cr.coin.name();
                    if by_name.insert(name, cr.coin).is_none() {
                        order.push(name);
                    }
                }
                // The removal merkle set is the removal names; its root must BE the foliage
                // removals_root — a mismatch means the served delta
                // would not verify against the header: reject, never serve unprovable data.
                let mut leafs: Vec<[u8; 32]> = order.iter().map(|n| n.bytes()).collect();
                let removal_merkle_set = MerkleSet::from_leafs(&mut leafs);
                let removals_root = block
                    .foliage_transaction_block
                    .as_ref()
                    .map(|ftb| ftb.removals_root);
                if removals_root != Some(Bytes32::new(removal_merkle_set.get_root())) {
                    warn!(
                        "request_removals: stored removals do not hash to the foliage removals_root height={}",
                        req.height
                    );
                    return reject();
                }
                let mut coins_map: Vec<NamedCoin> = Vec::with_capacity(coin_names.len());
                let mut proofs_map: Vec<(Bytes32, Vec<u8>)> = Vec::with_capacity(coin_names.len());
                for coin_name in coin_names {
                    let Ok((included, proof)) =
                        removal_merkle_set.generate_proof(&coin_name.bytes())
                    else {
                        return reject();
                    };
                    proofs_map.push((*coin_name, proof));
                    if let Some(coin) = by_name.get(coin_name) {
                        if !included {
                            return reject();
                        }
                        coins_map.push((*coin_name, Some(*coin)));
                    } else {
                        if included {
                            return reject();
                        }
                        coins_map.push((*coin_name, None));
                    }
                }
                RemovalsReply::Respond(Box::new(RespondRemovals {
                    height: block.height(),
                    header_hash: req.header_hash,
                    coins: coins_map,
                    proofs: Some(proofs_map),
                }))
            }
        }
    }

    // `request_children`: coin states of every child (spent + unspent) of a
    // coin, read from the parent secondary index. coin-index tier only.
    #[cfg(feature = "coin-index")]
    async fn children(&self, coin_name: Bytes32) -> Vec<CoinState> {
        match self.store.get_coins_by_parent(&coin_name).await {
            Ok(records) => records.iter().map(coin_state_of).collect(),
            Err(_) => Vec::new(),
        }
    }

    // `register_for_ph_updates`: subscribe the peer to puzzle-hash coin
    // updates in the shared WalletNotifier AND return the initial matching CoinState set. The receiver is
    // handed back only on the peer's FIRST registration; the dispatch layer bridges it to the socket.
    async fn register_for_ph_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RegisterForPhUpdates,
    ) -> PhRegistration {
        // Subscribe FIRST — the returned set is the hashes ACTUALLY subscribed (in-request duplicates,
        // already-subscribed hashes, and the per-peer-cap overflow all filtered out) — and feed ONLY
        // that set to the initial-state read (the query runs on
        // add_puzzle_subscriptions' return, never the raw request list). A registry-capacity failure
        // subscribes nothing, so it also reads nothing.
        let (receiver, subscribed) = self
            .wallet
            .register_for_ph_updates(peer, host, &req.puzzle_hashes)
            .await
            .unwrap_or_else(|_| (None, Vec::new()));
        // The response budget resolves from trust (`max_subscribe_response_items(peer)`).
        let max_items = self.trust.max_subscribe_response_items(&peer, host);
        let coin_states = self
            .ph_initial_states(&subscribed, req.min_height, max_items)
            .await;
        PhRegistration {
            response: RespondToPhUpdates {
                // The reply echoes the REQUESTED hashes, not the subscribed subset — and
                // signals nothing on truncation (log-only).
                puzzle_hashes: req.puzzle_hashes,
                min_height: req.min_height,
                coin_states,
            },
            receiver,
        }
    }

    // `register_for_coin_updates`: subscribe the peer to coin-id updates AND
    // return the initial matching CoinState set. The initial read uses get_coin_states_by_ids — a
    // provided default over point-gets, so it works on every backend without the service tier.
    async fn register_for_coin_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RegisterForCoinUpdates,
    ) -> CoinRegistration {
        // The REQUEST list truncates to max_subscriptions; the SLICED list is subscribed + queried,
        // and echoes the sliced list back. Unlike the ph path, in-request
        // duplicates stay queryable — the query runs on the deduped set, so dedup
        // happens at the query, not the echo.
        let mut coin_ids = req.coin_ids;
        coin_ids.truncate(self.wallet.max_subscriptions(&peer, host));
        let receiver = self
            .wallet
            .register_for_coin_updates(peer, host, &coin_ids)
            .await
            .ok()
            .and_then(|(rx, _added)| rx);
        let mut seen = HashSet::new();
        let query_ids: Vec<Bytes32> = coin_ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect();
        // Bounded by the response budget (get_coin_states_by_ids with
        // max_items = max_subscribe_response_items), resolved per-peer from trust.
        let coin_states = self
            .store
            .get_coin_states_by_ids(
                &query_ids,
                req.min_height,
                true,
                self.trust.max_subscribe_response_items(&peer, host),
            )
            .await
            .unwrap_or_default();
        CoinRegistration {
            response: RespondToCoinUpdates {
                coin_ids,
                min_height: req.min_height,
                coin_states,
            },
            receiver,
        }
    }

    // `request_puzzle_state` (code 98) — the Sage sync loop's paged
    // puzzle-hash read. Truncate + dedup the request list, check the requester's previous peak
    // against our chain (REORG reject on mismatch), check the subscription cap before AND after
    // the store read (the await-race double check), run the paged batch query, resolve the
    // page's (height, header_hash), and subscribe-on-finish. coin-index tier only (the batch
    // query reads the puzzle-hash secondary index); the validator tier keeps the store-blind
    // REORG-reject default.
    #[cfg(feature = "coin-index")]
    async fn puzzle_state(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RequestPuzzleState,
    ) -> PuzzleStateReply {
        // — the list_limits truncation + order-preserving dedup.
        let mut puzzle_hashes = req.puzzle_hashes;
        puzzle_hashes.truncate(dg_xch_stores::traits::MAX_PUZZLE_HASH_BATCH_SIZE);
        let mut seen = HashSet::new();
        puzzle_hashes.retain(|ph| seen.insert(*ph));
        // — previous_height=None compares against the GENESIS_CHALLENGE; an
        // unknown height or a mismatched hash means the requester's chain forked from ours.
        let previous_hash = match req.previous_height {
            Some(h) => self.height_to_hash(h).await,
            None => Some(self.constants.genesis_challenge),
        };
        if previous_hash != Some(req.header_hash) {
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        }
        // — would this subscribe blow the per-peer cap? (trust-resolved)
        let max_subscriptions = self.wallet.max_subscriptions(&peer, host);
        if req.subscribe_when_finished
            && puzzle_hashes.len() + self.wallet.peer_subscription_count(&peer).await
                > max_subscriptions
        {
            return PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        // — the page floor and the paged store read (batch_coin_states_by_
        // puzzle_hashes), bounded by the trust-resolved response budget.
        let max_items = self.trust.max_subscribe_response_items(&peer, host);
        let min_height = req.previous_height.map_or(0, |h| h.saturating_add(1));
        let Ok((coin_states, next_min_height)) = self
            .store
            .batch_coin_states_by_puzzle_hashes(&puzzle_hashes, min_height, &req.filters, max_items)
            .await
        else {
            // A store failure cannot produce a consistent page — the REORG reject is the
            // always-answer posture (an exception would leave the wallet timing out).
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        };
        let is_finished = next_min_height.is_none();
        // — the page's (height, header_hash): the block BEFORE the next page's
        // floor, or the peak when finished; no peak / unresolvable height rejects REORG.
        let Ok(Some((_, peak_height))) = self.store.get_peak().await else {
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        };
        let height = next_min_height.map_or(peak_height, |h| h.saturating_sub(1));
        let Some(header_hash) = self.height_to_hash(height).await else {
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        };
        // — re-check the cap across the await point.
        if req.subscribe_when_finished
            && puzzle_hashes.len() + self.wallet.peer_subscription_count(&peer).await
                > max_subscriptions
        {
            return PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        let mut receiver = None;
        if is_finished && req.subscribe_when_finished {
            match self
                .wallet
                .register_for_ph_updates(peer, host, &puzzle_hashes)
                .await
            {
                Ok((rx, _added)) => receiver = rx,
                // The registry itself is at capacity (a structural bound): the honest
                // answer is the subscription-limit reject, not a silently unsubscribed respond.
                Err(_) => {
                    return PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
                }
            }
        }
        PuzzleStateReply::Respond(
            Box::new(RespondPuzzleState {
                puzzle_hashes,
                height,
                header_hash,
                is_finished,
                coin_states,
            }),
            receiver,
        )
    }

    async fn coin_state(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RequestCoinState,
    ) -> CoinStateReply {
        // — truncate to max_subscribe_response_items (trust-resolved), then dedup.
        let max_items = self.trust.max_subscribe_response_items(&peer, host);
        let mut coin_ids = req.coin_ids;
        coin_ids.truncate(max_items);
        let mut seen = HashSet::new();
        coin_ids.retain(|id| seen.insert(*id));
        // — the previous-peak consistency check.
        let previous_hash = match req.previous_height {
            Some(h) => self.height_to_hash(h).await,
            None => Some(self.constants.genesis_challenge),
        };
        if previous_hash != Some(req.header_hash) {
            return CoinStateReply::Reject(RejectStateReason::REORG);
        }
        // — the pre-read cap check (trust-resolved).
        let max_subscriptions = self.wallet.max_subscriptions(&peer, host);
        if req.subscribe
            && coin_ids.len() + self.wallet.peer_subscription_count(&peer).await > max_subscriptions
        {
            return CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        // — include_spent=True, min_height = previous_height + 1.
        let min_height = req.previous_height.map_or(0, |h| h.saturating_add(1));
        let Ok(coin_states) = self
            .store
            .get_coin_states_by_ids(&coin_ids, min_height, true, max_items)
            .await
        else {
            return CoinStateReply::Reject(RejectStateReason::REORG);
        };
        // — the await-race re-check.
        if req.subscribe
            && coin_ids.len() + self.wallet.peer_subscription_count(&peer).await > max_subscriptions
        {
            return CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        let mut receiver = None;
        if req.subscribe {
            match self
                .wallet
                .register_for_coin_updates(peer, host, &coin_ids)
                .await
            {
                Ok((rx, _added)) => receiver = rx,
                Err(_) => {
                    return CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
                }
            }
        }
        CoinStateReply::Respond(
            Box::new(RespondCoinState {
                coin_ids,
                coin_states,
            }),
            receiver,
        )
    }

    // `request_fee_estimates`: for each requested epoch time,
    // estimate the fee-rate to be confirmed within `max(0, target - now)` seconds, reading the
    // mempool's fee estimator. V2→V1 rounds with `ceil` and always
    // answers one FeeEstimate per requested time (error=None; rate 0 when there is no history).
    async fn fee_estimates(&self, req: RequestFeeEstimates) -> FeeEstimateGroup {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let estimates = {
            let mp = self.mempool.lock().await;
            let est = mp.fee_estimator();
            req.time_targets
                .iter()
                .map(|&target| {
                    // deltas = [max(0, req_ts - utc_now)]
                    let delta = target.saturating_sub(now);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let mojos = est.estimate_fee_rate(delta).ceil() as u64;
                    FeeEstimate {
                        error: None,
                        time_target: target,
                        estimated_fee_rate: FeeRate {
                            mojos_per_clvm_cost: mojos,
                        },
                    }
                })
                .collect()
        };
        FeeEstimateGroup {
            error: None,
            estimates,
        }
    }

    // `request_remove_puzzle_subscriptions`: None = clear all
    // (returning the prior set), Some = remove the listed subset (returning what was removed).
    async fn remove_puzzle_subscriptions(
        &self,
        peer: Bytes32,
        puzzle_hashes: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        self.wallet
            .remove_ph_subscriptions(&peer, puzzle_hashes.as_deref())
            .await
    }

    // `request_remove_coin_subscriptions`.
    async fn remove_coin_subscriptions(
        &self,
        peer: Bytes32,
        coin_ids: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        self.wallet
            .remove_coin_subscriptions(&peer, coin_ids.as_deref())
            .await
    }

    // on_connect: the current peak as the wallet greeting —
    // fork_point_with_previous_peak is the peak HEIGHT on connect. None while the store has no
    // peak (`peak_full is None`).
    async fn wallet_peak(&self) -> Option<NewPeakWallet> {
        let (hash, height) = self.store.get_peak().await.ok().flatten()?;
        let rec = self.store.get_block_record(&hash).await.ok().flatten()?;
        Some(NewPeakWallet {
            header_hash: hash,
            height,
            weight: rec.weight,
            fork_point_with_previous_peak: height,
        })
    }

    // NODE→FULL_NODE greeting (on_connect): the confirmed peak as
    // `NewPeak(header_hash, height, weight, peak.height, unfinished_reward_block_hash)` — fork
    // point = the peak height itself on connect, and the unfinished hash committed exactly as
    // the commitment is `reward_chain_block.get_unfinished().get_hash()`.
    async fn full_node_peak(&self) -> Option<NewPeak> {
        on_connect_new_peak(self.store.as_ref()).await
    }

    // NODE→TIMELORD greeting (on_connect send_peak_to_timelords):
    // the same construction the peak broadcast runs, over the shared record window. Fails closed
    // (None → nothing sent) when the peak's ancestry cannot ground the difficulty/challenge
    // walks — the timelord then syncs on the next peak advance instead of receiving an
    // approximate message.
    async fn timelord_peak(&self) -> Option<Box<NewPeakTimelord>> {
        let (hash, _) = self.store.get_peak().await.ok().flatten()?;
        build_new_peak_timelord(
            self.store.as_ref(),
            &self.constants,
            &self.record_window,
            &self.sync_metrics,
            hash,
        )
        .await
    }

    async fn mempool_sync_filter(&self) -> Option<Vec<u8>> {
        on_connect_mempool_filter(&self.synced, &self.mempool).await
    }
}

// The on-connect NewPeak greeting construction (on_connect), shared by
// the inbound Handshake greeting ([`FullNodeApi::full_node_peak`]) and the outbound dial hook
// ([`outbound_on_connect`]): fork point = the peak height itself, unfinished hash =
// `reward_chain_block.get_unfinished().get_hash()`.
async fn on_connect_new_peak<S: BlockStore + Send + Sync>(store: &S) -> Option<NewPeak> {
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

async fn on_connect_mempool_filter(
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
    node: &Arc<Node<S>>,
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
const MAX_COIN_HASHES_PER_REQUEST: usize = 50;
#[cfg(feature = "coin-index")]
const MAX_COINS_MAP_SIZE: usize = 100;

// `max_subscribe_response_items` for an UNTRUSTED peer (initial-config.yaml:441,
// default): the untrusted response budget. Production resolves it per-peer from
// `TrustPolicy`; this named constant is the untrusted-tier value the coin-index wallet-query tests
// inject via `api_tuned` (its only consumers live in the coin-index-gated `wallet_queries` module).
#[cfg(all(test, feature = "coin-index"))]
const MAX_SUBSCRIBE_RESPONSE_ITEMS: usize = 100_000;

// `wallet_sync_api_sem = LimitedSemaphore.create(active_limit=2, waiting_limit=20)`
//: the node-wide concurrency bound on the heavy wallet-serve handlers.
const WALLET_SYNC_ACTIVE_LIMIT: usize = 2;
const WALLET_SYNC_WAITING_LIMIT: usize = 20;

// A CoinRecord rendered as the wallet-protocol CoinState: both heights are
// carried, with an unspent coin's spent_height left None.
#[cfg(feature = "coin-index")]
fn coin_state_of(cr: &CoinRecord) -> CoinState {
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
    async fn served_header_block(
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

    async fn try_build_candidate(
        &self,
        declare: &DeclareProofOfSpace,
        quality_string: Bytes32,
    ) -> Option<RequestSignedValues> {
        debug!(
            "producer.build start qs={} sp_index={}",
            quality_string, declare.signage_point_index
        );
        self.try_build_candidate_inner(declare, quality_string)
            .await
    }

    async fn try_build_candidate_inner(
        &self,
        declare: &DeclareProofOfSpace,
        // Keys the candidate in `self.candidates` and rides in the `RequestSignedValues`
        // (`add_candidate_block(quality_string, ...)`).
        quality_string: Bytes32,
    ) -> Option<RequestSignedValues> {
        let is_genesis_challenge = declare.challenge_hash == self.constants.genesis_challenge;

        // ---- Phase A: reachable slot inputs (one slot lock; a single PoSpace/hash pass, no I/O) ----
        let (sp, cc_challenge_hash, total_iters_pos_slot, rc_challenge, pos_eos) = {
            let slot = self.slot_state.lock().await;
            // the SP we accepted. At index 0 it is the all-None sub-slot-start
            // form; index > 0 carries real VDFs. Was a bare `?` (the #1 silent hole): an accepted proof
            // that no longer resolves here vanished with no trace. Now categorical.
            let Some(sp) = slot.get_signage_point(&declare.challenge_chain_sp) else {
                self.producer.candidate_dropped("sp_not_found_in_slotstate");
                info!(
                    "candidate: accepted SP no longer resolvable in slot state; dropping event={} reason={} cc_sp={}",
                    "producer.build.dropped",
                    "sp_not_found_in_slotstate",
                    declare.challenge_chain_sp
                );
                return None;
            };
            // At index 0 the SP is the sub-slot start (all-None); the cc challenge
            // is the SP hash itself. At index > 0 the SP MUST carry a cc VDF (guaranteed by admission); a
            // missing one is a malformed SP that slipped the accept ladder — was a bare `?`.
            let cc_challenge_hash = if declare.signage_point_index == 0 {
                declare.challenge_chain_sp
            } else {
                match sp.cc_vdf.as_ref() {
                    Some(v) => v.challenge,
                    None => {
                        self.producer.candidate_dropped("sp_cc_vdf_missing");
                        info!(
                            "candidate: index>0 SP has no challenge-chain VDF (malformed); dropping event={} reason={}",
                            "producer.build.dropped", "sp_cc_vdf_missing"
                        );
                        return None;
                    }
                }
            };
            // the pos sub-slot (start total-iters + its reward chain for the
            // backtrack); genesis has no stored sub-slot (total_iters_pos_slot = 0). Was a bare `?`
            // (the #2 silent hole).
            let (total_iters_pos_slot, pos_rc_end_challenge, pos_eos) = if is_genesis_challenge {
                (0u128, None, None)
            } else {
                let Some((eos, _, start)) = slot.get_sub_slot(&cc_challenge_hash) else {
                    self.producer.candidate_dropped("pos_sub_slot_not_found");
                    info!(
                        "candidate: pos sub-slot absent in slot state; dropping event={} reason={} cc_challenge_hash={}",
                        "producer.build.dropped", "pos_sub_slot_not_found", cc_challenge_hash
                    );
                    return None;
                };
                (
                    start,
                    Some(eos.reward_chain.end_of_slot_vdf.challenge),
                    Some(eos.clone()),
                )
            };
            // the reward-chain challenge the prev block must carry: index 0 uses
            // the pos sub-slot's reward chain, index > 0 the SP's rc-vdf challenge; then backtrack it
            // through the empty sub-slots we hold.
            let rc_challenge = if declare.signage_point_index == 0 {
                pos_rc_end_challenge
            } else {
                sp.rc_vdf.as_ref().map(|v| v.challenge)
            }
            .map(|rc| slot.backtrack_rc_challenge(rc));
            (
                sp,
                cc_challenge_hash,
                total_iters_pos_slot,
                rc_challenge,
                pos_eos,
            )
        };

        // the resolved cc challenge must equal the farmer's declared challenge.
        if cc_challenge_hash != declare.challenge_hash {
            self.producer.candidate_dropped("cc_challenge_mismatch");
            warn!(
                "candidate: resolved cc-challenge != declared challenge_hash; dropping event={} reason={} cc_challenge_hash={} declared={}",
                "producer.build.dropped",
                "cc_challenge_mismatch",
                cc_challenge_hash,
                declare.challenge_hash
            );
            return None;
        }

        // ---- Phase B: peak + prev-block linkage (store) ----
        let peak_rec = match self.store.get_peak().await {
            Ok(Some((hash, _))) => self.store.get_block_record(&hash).await.ok().flatten(),
            _ => None,
        };
        // prev_b starts at the peak; the reward-chain backtrack finds the true
        // previous block. No peak ⇒ genesis: prev_b = None, height 0.
        let prev_b: Option<BlockRecord> = if let Some(peak) = peak_rec.clone() {
            let Some(rc) = rc_challenge else {
                self.producer.candidate_dropped("no_rc_challenge");
                warn!(
                    "candidate: non-genesis declare with no reward-chain challenge resolved; dropping event={} reason={}",
                    "producer.build.dropped", "no_rc_challenge"
                );
                return None;
            };
            match backtrack_prev_block(self.store.as_ref(), peak, rc).await {
                Some(pb) => pb,
                None => {
                    self.producer.candidate_dropped("prev_block_not_found");
                    warn!(
                        "candidate: no previous block with the correct reward chain hash; dropping event={} reason={}",
                        "producer.build.dropped", "prev_block_not_found"
                    );
                    return None;
                }
            }
        } else {
            None
        };
        let height = match &prev_b {
            Some(pb) => pb.height + 1,
            None => 0,
        };

        // ---- Finished sub-slots (slot lock) + the pos-sub-slot guard ----
        // challenge_in_chain is block-store-derived (GENESIS if no prev block,
        // else prev_b's first-in-sub-slot ancestor's last finished challenge slot hash).
        let chain_challenge = match &prev_b {
            None => self.constants.genesis_challenge,
            Some(pb) => match challenge_in_chain(self.store.as_ref(), pb).await {
                Some(c) => c,
                None => {
                    self.producer
                        .candidate_dropped("challenge_in_chain_unresolved");
                    warn!(
                        "candidate: could not resolve challenge_in_chain from prev block; dropping event={} reason={}",
                        "producer.build.dropped", "challenge_in_chain_unresolved"
                    );
                    return None;
                }
            },
        };
        let finished_sub_slots = {
            let slot = self.slot_state.lock().await;
            slot.get_finished_sub_slots(chain_challenge, cc_challenge_hash)
        };
        let Some(finished_sub_slots) = finished_sub_slots else {
            self.producer
                .candidate_dropped("finished_sub_slots_disconnected");
            warn!(
                "candidate: finished sub-slots not connected; dropping event={} reason={} challenge_in_chain={} cc_challenge_hash={}",
                "producer.build.dropped",
                "finished_sub_slots_disconnected",
                chain_challenge,
                cc_challenge_hash
            );
            return None;
        };
        // the last finished sub-slot we would farm on must be the pos sub-slot.
        if let (Some(pos_eos), Some(last)) = (pos_eos.as_ref(), finished_sub_slots.last())
            && last != pos_eos
        {
            self.producer.candidate_dropped("wrong_sub_slots_to_farm");
            warn!(
                "candidate: have different sub-slots than required to farm this block; dropping event={} reason={}",
                "producer.build.dropped", "wrong_sub_slots_to_farm"
            );
            return None;
        }

        // ---- Phase D: pool/farmer targets, difficulty/ssi ----
        let (pool_target, farmer_ph) = match &prev_b {
            // Genesis pays the pre-farm puzzle hashes.
            None => (
                PoolTarget {
                    puzzle_hash: self.constants.genesis_pre_farm_pool_puzzle_hash,
                    max_height: 0,
                },
                self.constants.genesis_pre_farm_farmer_puzzle_hash,
            ),
            Some(_) => {
                // pool-contract plots pin the pool puzzle hash; OG plots carry
                // the farmer's pool_target.
                let pt = if let Some(ph) = declare.proof_of_space.pool_contract_puzzle_hash {
                    PoolTarget {
                        puzzle_hash: ph,
                        max_height: 0,
                    }
                } else if let Some(pt) = declare.pool_target {
                    pt
                } else {
                    self.producer.candidate_dropped("missing_pool_target");
                    warn!(
                        "candidate: OG-plot declare missing pool_target; dropping event={} reason={}",
                        "producer.build.dropped", "missing_pool_target"
                    );
                    return None;
                };
                (pt, declare.farmer_puzzle_hash)
            }
        };
        let peak_pair = match &peak_rec {
            Some(peak) => {
                let prev_weight = self
                    .store
                    .get_block_record(&peak.prev_hash)
                    .await
                    .ok()
                    .flatten()
                    .map_or(0, |r| r.weight);
                Some((peak, prev_weight))
            }
            None => None,
        };
        let (difficulty, sub_slot_iters) =
            candidate_difficulty_and_ssi(&self.constants, peak_pair, &finished_sub_slots);

        // ---- Phase E: iters + latency/empty-block guards ----
        let Some(iters) = resolve_candidate_iters(
            &self.constants,
            quality_string,
            &declare.proof_of_space,
            difficulty,
            sub_slot_iters,
            declare.signage_point_index,
            declare.challenge_chain_sp,
            total_iters_pos_slot,
        ) else {
            self.producer
                .candidate_dropped("required_iters_out_of_range");
            warn!(
                "candidate: proof failed the iters filter (required_iters out of range); dropping event={} reason={} sp_index={}",
                "producer.build.dropped",
                "required_iters_out_of_range",
                declare.signage_point_index
            );
            return None;
        };
        // a candidate that would infuse before the head is too late (latency).
        if let Some(peak) = &peak_rec
            && iters.infusion_point_total_iters < peak.total_iters
        {
            self.producer.candidate_dropped("latency_drop_candidate");
            warn!(
                "candidate: infusion point behind the current head (latency); dropping event={} reason={} sp_index={} infusion_point_total_iters={} head_total_iters={}",
                "producer.build.dropped",
                "latency_drop_candidate",
                declare.signage_point_index,
                iters.infusion_point_total_iters,
                peak.total_iters
            );
            return None;
        }
        // Empty-block coercion: if the candidate's signage point
        // sits at/before the transaction peak's window, the last transaction block prevents a new
        // one — coerce the block generator to None. tx-peak resolves via the O(1)
        // prev_transaction_block_hash link (the tx-peak link — NOT a peak backwalk).
        let mut coerce_empty = false;
        if let Some(peak) = &peak_rec {
            let tx_peak = if peak.is_transaction_block() {
                Some(peak.clone())
            } else if let Some(pth) = peak.prev_transaction_block_hash {
                self.store.get_block_record(&pth).await.ok().flatten()
            } else {
                None
            };
            if let Some(tx_peak) = tx_peak
                && iters.candidate_sp_total_iters <= tx_peak.total_iters
            {
                debug!("candidate: sp at/before the tx-peak window -> empty block");
                coerce_empty = true;
            }
        }

        // ---- Phase F: prev linkage (is_tx + reward claims), timestamp, assemble, store ----
        let total_iters_sp = total_iters_pos_slot + u128::from(iters.sp_iters);
        let prev = match &prev_b {
            // Genesis is a transaction block with no reward claims.
            None => CandidatePrev {
                is_transaction_block: true,
                prev_block_hash: self.constants.genesis_challenge,
                prev_transaction_block_hash: self.constants.genesis_challenge,
                prev_transaction_block_height: 0,
                reward_claims: Vec::new(),
            },
            Some(pb) => {
                match resolve_prev_linkage(self.store.as_ref(), &self.constants, pb, total_iters_sp)
                    .await
                {
                    Some(p) => p,
                    None => {
                        self.producer.candidate_dropped("prev_linkage_store_gap");
                        warn!(
                            "candidate: prev-block linkage/reward-claim walk failed (store gap); dropping event={} reason={}",
                            "producer.build.dropped", "prev_linkage_store_gap"
                        );
                        return None;
                    }
                }
            }
        };
        // timestamp strictly after the previous transaction block.
        let timestamp = match &prev_b {
            None => now_secs(),
            Some(pb) => candidate_timestamp(self.store.as_ref(), pb).await,
        };

        let transactions = {
            let mp = self.mempool.lock().await;
            if may_build_transactions(
                prev.is_transaction_block,
                coerce_empty,
                mp.peak().map(|(h, _)| h),
                prev.prev_transaction_block_height,
            ) {
                mp.create_block_generator(&self.constants, height, BLOCK_CREATION_TIMEOUT)
            } else {
                debug!(
                    "candidate: no mempool payload (non-tx, coerced, or frame mismatch); empty block is_tx={} coerce_empty={} mempool_peak={:?} prev_tx_height={}",
                    prev.is_transaction_block,
                    coerce_empty,
                    mp.peak(),
                    prev.prev_transaction_block_height
                );
                None
            }
        };

        // index 0 passes SignagePoint(None, ...) (the sub-slot start has no
        // signage VDFs); index > 0 passes the real SP. `sp` at index 0 is already the all-None form, so
        // either the explicit None or Some(&sp) would null the VDFs; None keeps the contract unambiguous.
        let sp_for_block = if declare.signage_point_index == 0 {
            None
        } else {
            Some(&sp)
        };
        let Some((candidate, request)) = assemble_candidate(
            &self.constants,
            declare,
            quality_string,
            sp_for_block,
            finished_sub_slots,
            &iters,
            height,
            &prev,
            transactions.as_ref(),
            pool_target,
            farmer_ph,
            timestamp,
            cc_challenge_hash,
        ) else {
            self.producer.candidate_dropped("assembly_hash_fail");
            warn!(
                "candidate: assembly failed to hash foliage/reward block; dropping event={} reason={}",
                "producer.build.dropped", "assembly_hash_fail"
            );
            return None;
        };

        // S3 success — the candidate exists. `partial` (the reward-chain-block hash) is the S6/S7 join
        // key; logging it here bridges the qs-keyed build events to the partial-keyed driver events.
        let partial = candidate.reward_chain_block.hash().ok();
        // `full_node_store.add_candidate_block`(quality_string, height, unfinished_block).
        self.candidates
            .lock()
            .await
            .insert(quality_string, height, candidate);
        self.producer.candidate_built();
        info!(
            "assembled candidate unfinished block; requesting farmer signatures event={} height={} sp_index={} qs={} partial={:?} tx_generator={}",
            "producer.build.assembled",
            height,
            declare.signage_point_index,
            quality_string,
            partial,
            transactions.is_some()
        );
        Some(request)
    }
}

pub(crate) fn may_build_transactions(
    is_transaction_block: bool,
    coerce_empty: bool,
    mempool_peak_height: Option<u32>,
    prev_transaction_block_height: u32,
) -> bool {
    is_transaction_block
        && !coerce_empty
        && mempool_peak_height == Some(prev_transaction_block_height)
}

// Wall-clock seconds since the Unix epoch (`uint64(time.time())`), 0 on a pre-epoch clock.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// A hard cap on any single candidate-assembly block-store walk. A well-formed chain reaches a
// sub-slot/transaction boundary within `MAX_SUB_SLOT_BLOCKS`; the cap turns a store gap or a corrupt
// ancestry into a bounded bail instead of an unbounded loop on the produce path.
const CANDIDATE_WALK_CAP: usize = 512;

/// The reward-chain backtrack that finds the candidate's true previous block —
/// `declare_proof_of_space:1005-1022`. `rc_challenge` is already backtracked through empty sub-slots
/// (`SlotState::backtrack_rc_challenge`). Returns `None` to BAIL (the did-not-find return), or
/// `Some(prev_b)` where `prev_b` may itself be `None` when the finished-reward-slot match steps back to
/// the genesis boundary. Bounded to 10 attempts.
pub(crate) async fn backtrack_prev_block<S: BlockStore + Send + Sync>(
    store: &S,
    peak: BlockRecord,
    rc_challenge: Bytes32,
) -> Option<Option<BlockRecord>> {
    let mut prev_b = Some(peak);
    for _ in 0..10 {
        let Some(cur) = prev_b.clone() else { break };
        if cur.reward_infusion_new_challenge == rc_challenge {
            return Some(Some(cur));
        }
        if let Some(hashes) = &cur.finished_reward_slot_hashes
            && hashes.last() == Some(&rc_challenge)
        {
            // This block includes the sub-slot our SP vdf starts in; go back one more for the prev block.
            return Some(store.get_block_record(&cur.prev_hash).await.ok().flatten());
        }
        prev_b = store.get_block_record(&cur.prev_hash).await.ok().flatten();
    }
    None
}

/// The `challenge_in_chain` for `get_finished_sub_slots` —
/// `curr = prev_b; while not curr.first_in_sub_slot: curr = block_record(curr.prev_hash);
/// curr.finished_challenge_slot_hashes[-1]` (`full_node_store.get_finished_sub_slots`). Bounded.
pub(crate) async fn challenge_in_chain<S: BlockStore + Send + Sync>(
    store: &S,
    prev_b: &BlockRecord,
) -> Option<Bytes32> {
    let mut cur = prev_b.clone();
    for _ in 0..CANDIDATE_WALK_CAP {
        if cur.first_in_sub_slot() {
            return cur.finished_challenge_slot_hashes?.last().copied();
        }
        cur = store
            .get_block_record(&cur.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    None
}

/// `is_transaction_block` + the reward-claim walk + `prev_transaction_block_hash` for a non-genesis
/// candidate — `get_prev_transaction_block` +
/// `create_foliage`'s reward-claim walk. `total_iters_sp`
/// is the candidate's signage-point total iters (`total_iters_pos_slot + sp_iters`). Returns `None` on a
/// store gap. Walks are bounded by [`CANDIDATE_WALK_CAP`].
pub(crate) async fn resolve_prev_linkage<S: BlockStore + Send + Sync>(
    store: &S,
    constants: &ConsensusConstants,
    prev_b: &BlockRecord,
    total_iters_sp: u128,
) -> Option<CandidatePrev> {
    // get_prev_transaction_block: walk prev_b back to the first transaction block.
    let mut cur = prev_b.clone();
    for _ in 0..CANDIDATE_WALK_CAP {
        if cur.is_transaction_block() {
            break;
        }
        cur = store
            .get_block_record(&cur.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    if !cur.is_transaction_block() {
        return None; // walk cap hit without a transaction block — store gap.
    }
    let prev_transaction_block = cur;
    // is_transaction_block = total_iters_sp > prev_transaction_block.total_iters.
    let is_transaction_block = total_iters_sp > prev_transaction_block.total_iters;
    // height > 0 here (prev_b exists), so prev_block_hash is prev_b.header_hash .
    let prev_block_hash = prev_b.header_hash;
    if !is_transaction_block {
        // Non-tx candidate: create_foliage builds no foliage_transaction_block; the tx-hash/claims are
        // unused (prev_transaction_block_hash is a harmless placeholder) and the daemon never builds
        // a mempool payload for it (the height still records the true prev tx block).
        return Some(CandidatePrev {
            is_transaction_block: false,
            prev_block_hash,
            prev_transaction_block_hash: constants.genesis_challenge,
            prev_transaction_block_height: prev_transaction_block.height,
            reward_claims: Vec::new(),
        });
    }
    // The reward-claim walk (height > 0): the prev transaction block WITH its
    // fees, then every non-transaction block between it and the transaction block before it (fees = 0).
    let prev_transaction_block_hash = prev_transaction_block.header_hash;
    let mut reward_claims = vec![RewardBlockClaim {
        height: prev_transaction_block.height,
        pool_puzzle_hash: prev_transaction_block.pool_puzzle_hash,
        farmer_puzzle_hash: prev_transaction_block.farmer_puzzle_hash,
        fees: prev_transaction_block.fees.unwrap_or(0),
    }];
    if prev_transaction_block.height > 0 {
        let mut curr = store
            .get_block_record(&prev_transaction_block.prev_hash)
            .await
            .ok()
            .flatten()?;
        for _ in 0..CANDIDATE_WALK_CAP {
            if curr.is_transaction_block() {
                break;
            }
            reward_claims.push(RewardBlockClaim {
                height: curr.height,
                pool_puzzle_hash: curr.pool_puzzle_hash,
                farmer_puzzle_hash: curr.farmer_puzzle_hash,
                fees: 0,
            });
            curr = store
                .get_block_record(&curr.prev_hash)
                .await
                .ok()
                .flatten()?;
        }
    }
    Some(CandidatePrev {
        is_transaction_block: true,
        prev_block_hash,
        prev_transaction_block_hash,
        prev_transaction_block_height: prev_transaction_block.height,
        reward_claims,
    })
}

/// The candidate timestamp — `declare_proof_of_space:1113-1121`: `max(now, prev_tx_block.timestamp
/// + 1)`, walking `prev_b` back to the first transaction block (or genesis). Falls back to `now` on a
/// store gap. Bounded by [`CANDIDATE_WALK_CAP`].
pub(crate) async fn candidate_timestamp<S: BlockStore + Send + Sync>(
    store: &S,
    prev_b: &BlockRecord,
) -> u64 {
    let now = now_secs();
    let mut curr = prev_b.clone();
    for _ in 0..CANDIDATE_WALK_CAP {
        if curr.is_transaction_block() || curr.height == 0 {
            break;
        }
        match store.get_block_record(&curr.prev_hash).await.ok().flatten() {
            Some(next) => curr = next,
            None => return now,
        }
    }
    match curr.timestamp {
        Some(ts) if now <= ts => ts + 1,
        _ => now,
    }
}

// The running node (server layer): one shared store fanned out by Arc to the engine (via the
// chaser), the RPC surface, and the peer server; plus the mempool + wallet-notifier + sync-status flag the
// new-peak path updates. Thin — it wires the four layers, it does not re-implement them.
pub struct Node<S = SqliteStore> {
    pub config: Config,
    pub store: Arc<S>,
    pub mempool: Arc<Mutex<Mempool>>,
    pub wallet: Arc<WalletNotifier>,
    // The shared trusted-peer policy (`trusted_peers`): resolves per-peer subscription + response
    // caps and tx-queue priority. Built once from `config.trusted_peers` at boot; the SAME `Arc` is
    // held by `wallet` and every StoreApi. Empty config → every peer untrusted.
    trust: Arc<TrustPolicy>,
    // The node-wide wallet-serve concurrency bound (`wallet_sync_sem`) — one instance shared by
    // the inbound server api and every outbound connection's api. See the StoreApi field.
    wallet_sync_sem: Arc<LimitedSemaphore>,
    pub rpc: Arc<NodeRpc<S>>,
    pub synced: Arc<AtomicBool>,
    /// Simulator only: serve wallets a v1-shaped proof of space in block headers (a stock wallet
    /// cannot deserialize a v2 proof). Off on a production node.
    pub wallet_compat: Arc<AtomicBool>,
    pub run: Arc<AtomicBool>,
    // One-shot latch for the deferred secondary-index build fired on the not-synced -> synced
    // edge in `update_synced`; reset on a failed build so a later edge retries, and by the
    // falling-edge shed so the next tip edge rebuilds what the shed dropped.
    deferred_indexes_started: Arc<AtomicBool>,
    // One-shot latch for the falling-edge index shed fired when the node is deeper behind than
    // SHED_TIP_LAG_BLOCKS in `update_synced`; reset by a successful rising-edge build (arming
    // the next fall) or on a failed shed so the phase re-fires it.
    service_indexes_shed: Arc<AtomicBool>,
    constants: ConsensusConstants,
    claimed_peak: Arc<AtomicU32>,
    // The per-peer peak-claim book (`sync_store`): per-connection claims, heaviest-claim selection,
    // disconnect retraction, and the bad-peak quarantine. Publishes the heaviest claim's height into
    // `claimed_peak` (the metrics gauge + declare plot-filter height), rolling it BACK on retraction.
    peak_book: Arc<PeakBook>,
    // Wakes the tip_follower on a NewPeak announcement (shared with the StoreApi handler).
    new_peak_signal: Arc<Notify>,
    // The last weight proof we verified, keyed by the tip it attests. A body-download retry reuses this
    // instead of re-fetching + re-running the multi-minute proof verify every driver tick.
    validated_tip: Arc<RwLock<Option<ValidatedTip>>>,
    // The validated WP tip whose fork point has been resolved against our chain this landing
    // (the fork point is computed once per sync). The producer's
    // FOLLOW fill is gated on it while the mid-chain long-sync band is active, so the batch
    // download never outruns the trust anchor (the proof validates before
    // sync_from_fork_point ever runs).
    long_sync_anchor: Arc<RwLock<Option<Bytes32>>>,
    // Whether the `--sync-from` mid-chain anchor span has been staged (headers candidate pass +
    // epoch-depth backfill). The producer's FOLLOW fill is gated on it while no peak exists yet:
    // the anchor stages ancestry but sets no peak — the first confirmed body does — so without
    // this flag the producer waits on a peak that only the producer's own fill can create.
    sync_from_anchor: Arc<RwLock<Option<u32>>>,
    known_peers: Arc<RwLock<Vec<TimestampedPeerInfo>>>,
    // The INBOUND peer sessions map, owned by the Node so the confirm path can reach wallet-type
    // peers directly: `notify_new_peak` broadcasts `NewPeakWallet` to every peer that handshook as
    // NodeType::Wallet (`full_node.update_wallets`). spawn_peer_server
    // hands this same map to the WebsocketServer (which inserts/removes sessions) and returns it.
    inbound_peers: PeerMap,
    // Transactions admitted from gossip, push_tx, or a wallet's p2p SendTransaction, awaiting
    // NewTransaction re-broadcast by the driver. Public like `mempool`: the integration suite
    // asserts every admission path queues its announce through this one seam.
    pub tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    tx_requested: Arc<Mutex<HashMap<Bytes32, PendingTx>>>,
    // The header hash of the LAST confirmed delta notify_new_peak processed. A delta whose
    // prev_hash breaks this chain is a reorg landing (the engine may emit only the new tip's
    // delta for a deep reorg) — the mempool then takes the slow path
    // (`Mempool::revalidate_for_reorg`) so items whose removals were rolled back
    // (UNKNOWN_UNSPENT) or spent on the winning branch are dropped, before the per-delta
    // fast path runs.
    last_delta_hash: Mutex<Option<Bytes32>>,
    // txid -> (origin identity, when recorded): the peer a gossiped bundle arrived FROM, so the
    // NewTransaction re-broadcast can exclude it. The identity carries BOTH the dispatch peer
    // id AND the remote host:
    // an inbound link's peer id is the peer's true cert hash (exact), but every outbound DIAL shares
    // OUR client cert hash (clients websocket peer_id = hash of our own cert), so an outbound origin
    // is only distinguishable by its remote host. Entries are consumed by
    // the announce drain; unconsumed ones (failed admissions) age out. Bounded — see `note_tx_origin`.
    tx_origin: Arc<Mutex<HashMap<Bytes32, (TxOrigin, Instant)>>>,
    // Slot state + its driver-drained queues (received gossip in, relay announces out).
    slot_state: Arc<Mutex<SlotState>>,
    sp_inbox: Arc<Mutex<Vec<SpEvent>>>,
    sp_announce: Arc<Mutex<Vec<NewSignagePointOrEndOfSubSlot>>>,
    // The farmer-form signage points queued at each accept site alongside sp_announce, and
    // drained by the driver to inbound farmer peers (new_signage_point → farmer_protocol).
    sp_farmer_announce: Arc<Mutex<Vec<NewSignagePoint>>>,
    // Peer-link traffic counters shared with every handler map and the broadcast paths.
    net: Arc<NetCounters>,
    // Unfinished-block cache + received-block inbox + relay announce queue.
    unfinished: Arc<Mutex<UnfinishedCache>>,
    ub_inbox: Arc<Mutex<Vec<UnfinishedBlock>>>,
    // Timelord infusion-return inbox (`new_infusion_point_vdf`): drained by process_ip_inbox, which
    // finishes our cached unfinished block into a FullBlock and sets it as the new peak.
    ip_inbox: Arc<Mutex<Vec<NewInfusionPointVDF>>>,
    ub_announce: Arc<Mutex<Vec<NewUnfinishedBlock2>>>,
    // add_unfinished_block also sends NewUnfinishedBlockTimelord to the timelord
    // peers so a timelord can infuse the partial into a FullBlock. Queued here alongside ub_announce and
    // drained to inbound timelord peers by the driver.
    ub_timelord_announce: Arc<Mutex<Vec<NewUnfinishedBlockTimelord>>>,
    // The gossip-transaction inbox drained by the validator worker (trusted-priority lane inside).
    tx_inbox: Arc<Mutex<TxQueue>>,
    // The RequestProofOfWeight inbox drained by the weight-proof worker.
    wp_inbox: Arc<Mutex<Vec<WpRequest>>>,
    // Compact-VDF consume: the pulled-proof inbox the driver validates + swaps, and the
    // NewCompactVDF re-gossip queue it feeds (drained to peers like ub_announce).
    compact_vdf_inbox: Arc<Mutex<Vec<RespondCompactVDF>>>,
    compact_vdf_announce: Arc<Mutex<Vec<NewCompactVDF>>>,
    // Farmer interface: accepted proof-of-space declarations awaiting block assembly.
    proof_candidates: Arc<Mutex<ProofCandidateStore>>,
    // Candidate unfinished blocks awaiting the farmer's SignedValues reply.
    candidates: Arc<Mutex<CandidateBlockStore>>,
    // Block-producer pipeline counters — the
    // first-block funnel, shared with the read-loop StoreApi and rendered on /metrics.
    producer: Arc<ProducerMetrics>,
    // Header hashes of unfinished blocks WE farmed, recorded at splice time so the follow driver can
    // count our own block when it confirms (S8). Bounded FIFO shared with the read-loop StoreApi.
    farmed_headers: Arc<Mutex<VecDeque<Bytes32>>>,
    // Signage-point telemetry: latest accepted SP index + running total.
    sp_current_index: Arc<AtomicU32>,
    signage_points_total: Arc<std::sync::atomic::AtomicU64>,
    // Unix second the current follow/backtrack fetch+confirm went in flight (0 = idle) — read by the
    // /health stall dump so a wedged request is named with its age instead of inferred from silence.
    follow_inflight_since: Arc<std::sync::atomic::AtomicU64>,
    sync_metrics: Arc<SyncMetrics>,
    // Bounded LRU of peer-fetched OUT-OF-SPAN generator refs (`--sync-from` compression refs
    // below the anchor), keyed by height. The engine's per-window seed clear is a correctness
    // invariant (an in-engine capacity cap could evict a ref mid-window) — so the daemon caches
    // the FETCH instead: the dust era references the same template heights (e.g. mainnet
    // 4,413,681) window after window, and without this every window re-pulled them from the
    // peer. Bounded at SEED_REF_CACHE_CAP entries × the 1 MiB generator ceiling.
    seed_ref_cache: Mutex<VecDeque<(u32, dg_xch_core::clvm::program::SerializedProgram)>>,
    chaser: Mutex<Chaser<Arc<S>, NativePrimitives>>,
    // The consensus-walk record window (record_window.rs): the in-memory record cache
    // equivalent, serving difficulty_records_map without per-call store walks. Arc-shared with
    // the inbound StoreApi so the on-connect TIMELORD greeting can build a NewPeakTimelord.
    record_window: Arc<Mutex<BlockRecordCache>>,
}

// Cross-window cache bound for peer-fetched out-of-span generator refs: 64 × ~1 MiB worst case.
const SEED_REF_CACHE_CAP: usize = 64;

// A verified weight proof plus the summaries its verification produced, cached against re-validation.
#[derive(Clone)]
struct ValidatedTip {
    tip: Bytes32,
    wp: Arc<WeightProof>,
    summaries: Arc<Vec<SubEpochSummary>>,
}

impl Node<SqliteStore> {
    /// Boot the node on the embedded SQLite backend: open the `--db` path, then wire everything via
    /// [`Node::boot_with_store`]. Does not start any network listener (see [`Node::run`] or the granular
    /// `spawn_*` helpers).
    ///
    /// # Errors
    /// Returns an I/O error if the backend cannot be opened.
    pub async fn boot(config: Config) -> Result<Self, Error> {
        let store = open_backend(&config.backend).await?;
        Self::boot_with_store(config, store)
    }
}

impl<S> Node<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// Wire the node around an already-opened store: engine/chaser + mempool + wallet + RPC surface.
    /// Backend selection lives in the caller (`main` dispatches `--db` to SQLite or Postgres).
    ///
    /// # Errors
    /// Infallible today; kept fallible so store-dependent wiring can fail cleanly later.
    pub fn boot_with_store(config: Config, store: Arc<S>) -> Result<Self, Error> {
        let constants = constants_for(&config.network_id);
        Self::boot_with_store_constants(config, store, constants)
    }

    /// As [`boot_with_store`], but with the consensus constants supplied directly rather than
    /// resolved from the network id — the seam a simulator uses to serve under its own constants.
    pub fn boot_with_store_constants(
        config: Config,
        store: Arc<S>,
        constants: ConsensusConstants,
    ) -> Result<Self, Error> {
        let engine = Engine::new(store.clone(), NativePrimitives, constants);
        let chaser = Chaser::new(engine, SyncConfig::default());
        // Clone the chaser's metrics handle up front so the /metrics server can read the same atomics the
        // sync pipeline writes (the chaser then lives behind a Mutex).
        let sync_metrics = chaser.metrics().clone();
        // The claimed-peak gauge: written only by the peak book's republish (the heaviest live claim's
        // height); shared with /metrics and the declare handler's plot-filter height read.
        let claimed_peak = Arc::new(AtomicU32::new(0));
        let mempool = Arc::new(Mutex::new(Mempool::new(&constants)));
        // Resolve the trusted-peer policy from config ONCE (node ids + CIDRs, parsed here); the same
        // policy Arc backs the wallet (subscription caps) and every StoreApi (response-item cap + tx
        // priority). Empty config → only localhost is trusted (`is_trusted_peer` default).
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
        let rpc = Arc::new(NodeRpc::new(
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
            rpc,
            synced,
            wallet_compat,
            run: Arc::new(AtomicBool::new(true)),
            deferred_indexes_started: Arc::new(AtomicBool::new(false)),
            service_indexes_shed: Arc::new(AtomicBool::new(false)),
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
        // The gossip-transaction validator worker lives for the node's lifetime (requires an
        // ambient tokio runtime — every boot path is async).
        spawn_tx_validator(
            node.store.clone(),
            node.mempool.clone(),
            node.constants,
            node.tx_inbox.clone(),
            node.tx_announce.clone(),
            node.synced.clone(),
            node.run.clone(),
        );
        // The weight-proof serving worker: drains queued RequestProofOfWeight, builds off the
        // websocket read path (single-flight per tip), and responds to the requesting peer.
        spawn_wp_worker(
            node.store.clone(),
            node.constants,
            node.wp_inbox.clone(),
            node.net.clone(),
            node.run.clone(),
        );
        // Compact-VDF solicitation scan — OFF unless `--uncompact`.
        if node.config.uncompact {
            spawn_uncompact_scanner(
                node.store.clone(),
                node.inbound_peers.clone(),
                node.net.clone(),
                node.run.clone(),
            );
        }
        Ok(node)
    }

    /// Start the P2P peer server (`--listen`): peers complete the handshake and pull blocks from our store.
    /// Returns the server's run flag (clear it to stop the listener).
    ///
    /// # Errors
    /// Returns an I/O error if the TLS config or socket cannot be initialized.
    pub fn spawn_peer_server(&self) -> Result<(Arc<AtomicBool>, PeerMap), Error> {
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
        // The Node-owned inbound map (see the field note): the server inserts/removes sessions in
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
        let run_c = run.clone();
        tokio::spawn(async move {
            let _ = server.run(run_c).await;
        });
        // Hand the shared inbound map back so /metrics can gauge its live length — the
        // retention bisect instrument for the inbound peer sessions (the collection whose
        // unbounded growth was the second live-only retainer; the PeerRegistry-derived
        // `fullnode_inbound_peers` never observed it because the server path bypasses admit_inbound).
        Ok((run, peers))
    }

    /// Start the RPC server (`--rpc`): block/coin queries + push_tx over mutual TLS
    /// (server cert from the private CA chain, client cert required — `rpc::build_rpc_tls_context`).
    /// Attaches the daemon's live state (unfinished cache, slot state, inbound
    /// peers, cert-hash node id) so the live-state endpoints answer. Returns the server's run flag.
    ///
    /// # Errors
    /// Returns an I/O error if the TLS config or socket cannot be initialized.
    pub fn spawn_rpc_server(&self) -> Result<Arc<AtomicBool>, Error> {
        // `--rpc-tls local` is unauthenticated, so a routable --rpc bind is downgraded to
        // loopback with a loud warning rather than exposing an unauthenticated RPC to the
        // network. `--rpc-tls private-ca` enables authenticated network RPC.
        let (rpc_bind, downgraded) = self.config.rpc_tls.resolve_bind(self.config.rpc);
        if downgraded {
            log::warn!(
                "--rpc-tls local is unauthenticated; refusing to bind the RPC to a routable address \
                 — serving on loopback instead. Use --rpc-tls private-ca for an authenticated network RPC. configured={} effective={}",
                self.config.rpc,
                rpc_bind
            );
        }
        let tls = crate::rpc::build_rpc_tls_context(&self.config.rpc_tls, rpc_bind)?;
        self.rpc.attach_live(crate::rpc::NodeRpcLive {
            node_id: tls.node_id,
            network_id: self.config.network_id.clone(),
            local_port: self.config.listen.port(),
            claimed_peak: self.claimed_peak.clone(),
            slot_state: self.slot_state.clone(),
            unfinished: self.unfinished.clone(),
            inbound_peers: self.inbound_peers.clone(),
        });
        let handler = Arc::new(NodeRpcHandler::new(self.rpc.clone()));
        let server = RpcServer::new_with_server_config(
            &RpcServerConfig {
                host: rpc_bind.ip().to_string(),
                port: rpc_bind.port(),
                ssl_info: None,
            },
            tls.server_config,
            handler,
        )?;
        let run = Arc::new(AtomicBool::new(true));
        let run_c = run.clone();
        tokio::spawn(async move {
            let _ = server.run(run_c).await;
        });
        Ok(run)
    }

    /// Follow a peer's tip: pull `from..=to`, confirm each block through the engine, and drive the per-peak
    /// side effects (mempool revalidation + wallet coin-state updates). Returns the confirmed peak.
    ///
    /// # Errors
    /// Returns an I/O error if the peer cannot serve the range, a block fails validation, or a store/notify
    /// step fails.
    pub async fn sync_follow(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, Error> {
        self.follow_step(source, from, to)
            .await
            .map_err(|e| Error::other(e.to_string()))
    }

    /// [`Node::sync_follow`] over pre-fetched, height-sorted blocks — the driver's prefetch
    /// overlap feeds this so the next window's download runs during this window's validation.
    ///
    /// # Errors
    /// Returns an error if a block fails validation or the store errors.
    pub async fn sync_follow_blocks(
        &self,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
    ) -> Result<Option<(Bytes32, u32)>, Error> {
        self.follow_step_blocks(blocks)
            .await
            .map_err(|e| Error::other(e.to_string()))
    }

    async fn follow_step(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let (peak, deltas) = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_to_reporting(source, from, to).await?
        };
        self.finish_follow_step(peak, &deltas).await
    }

    // Typed core of [`Node::sync_follow_blocks`].
    async fn follow_step_blocks(
        &self,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        self.follow_step_blocks_pre(blocks, None).await
    }

    // [`Node::follow_step_blocks`] with driver-precomputed window bodies (the cross-window body
    // pipeline: window N+1's CLVM/BLS precompute ran while window N validated).
    async fn follow_step_blocks_pre(
        &self,
        blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
        pre: Option<std::collections::HashMap<u32, dg_xch_node::engine::PrecomputedBody>>,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let (peak, deltas) = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_blocks_reporting_pre(blocks, pre).await?
        };
        self.finish_follow_step(peak, &deltas).await
    }

    // Stage half of the stage-ahead pipeline: window into the overlay, no writer, no drains —
    // callable while the PREVIOUS window's spawned drain still owns the CPU. An `Err` here has
    // NOT cleared the overlay (see `Chaser::stage_window_pre`); the caller confirms its pending
    // window first, then clears.
    async fn stage_step_window(
        &self,
        blocks: Vec<dg_xch_core::blockchain::full_block::FullBlock>,
        pre: Option<std::collections::HashMap<u32, dg_xch_node::engine::PrecomputedBody>>,
    ) -> Result<dg_xch_node::sync::StagedWindow, SyncError> {
        let mut chaser = self.chaser.lock().await;
        chaser.stage_window_pre(blocks, pre).await
    }

    // Confirm half: the drain's verdict lands the window (archive + coins + peak, one
    // transaction) and the per-peak side effects fire exactly as in the serial step.
    async fn confirm_step_window(
        &self,
        window: dg_xch_node::sync::StagedWindow,
        verdict: dg_xch_node::sync::WindowVerdict,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        let (peak, deltas) = {
            let mut chaser = self.chaser.lock().await;
            chaser.confirm_window_pre(window, verdict).await?
        };
        self.finish_follow_step(peak, &deltas).await
    }

    /// The mirrored `short_sync_backtrack` step, driven when a follow
    /// window fails with the unknown-parent orphan: the chain reorged at/below our stored tip, so
    /// the fork point is fetched backward from the same peer and the collected branch resubmitted
    /// through the ordinary follow pipeline (the engine's existing fork choice performs the reorg).
    /// The per-peak side effects (wallet coin-state + mempool revalidation) fire for every newly
    /// confirmed block exactly as in [`Node::sync_follow`].
    ///
    /// # Errors
    /// [`SyncError::DeepFork`] when the fork is deeper than the backtrack cap — the caller must fall
    /// back to the weight-proof long sync instead of retrying; any
    /// fetch/validation/store error otherwise.
    pub async fn sync_backtrack(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        // Mark the fetch+confirm in flight for the /health stall dump (cleared on EVERY exit path —
        // a wedged request is exactly when the dump needs its age).
        self.follow_inflight_since.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        let stepped = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_backtrack_reporting(source, from, to).await
        };
        self.follow_inflight_since.store(0, Ordering::Relaxed);
        let (peak, deltas) = stepped?;
        self.finish_follow_step(peak, &deltas).await
    }

    /// One near-tip follow step via `new_peak` ladder: forward-extend first, backtrack only on
    /// the unknown-parent orphan ([`Chaser::follow_tip_step_reporting`]). This is the near-tip band's
    /// entry — a direct child of the peak confirms with a single forward `[from, to]` fetch, so the
    /// confirmed peak pins the network tip at lag 0-1 instead of paying a backward peak-refetch per
    /// block. A real reorg at/below the tip still resolves through the same backtrack recovery arm.
    pub async fn sync_tip_step(
        &self,
        source: &Arc<dyn BlockRangeSource>,
        from: u32,
        to: u32,
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        self.follow_inflight_since.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        let stepped = {
            let mut chaser = self.chaser.lock().await;
            chaser.follow_tip_step_reporting(source, from, to).await
        };
        self.follow_inflight_since.store(0, Ordering::Relaxed);
        let (peak, deltas) = stepped?;
        self.finish_follow_step(peak, &deltas).await
    }

    // Shared tail of every follow-shaped step: per-peak side effects + the synced flag.
    async fn finish_follow_step(
        &self,
        peak: Option<(Bytes32, u32)>,
        deltas: &[ConfirmedDelta],
    ) -> Result<Option<(Bytes32, u32)>, SyncError> {
        for cd in deltas {
            let d = &cd.delta;
            // S8 — terminal PASS: a confirmed block whose header hash matches one WE farmed. The farmed
            // foliage hash recorded at splice time (S5) IS the FullBlock header hash, so a match means
            // our unfinished block was infused and completed. Take it out of the FIFO so we count once.
            {
                let mut farmed = self.farmed_headers.lock().await;
                if let Some(pos) = farmed.iter().position(|h| *h == d.header_hash) {
                    farmed.remove(pos);
                    drop(farmed);
                    self.producer.full_block();
                    info!(
                        "full block confirmed from OUR farmed unfinished block event={} height={} header={}",
                        "producer.full_block.added", d.height, d.header_hash
                    );
                }
            }
            self.notify_new_peak(d, cd.reorg.as_ref())
                .await
                .map_err(SyncError::Io)?;
        }
        if peak.is_some() {
            self.update_synced().await;
        }
        Ok(peak)
    }

    pub async fn update_synced(&self) {
        let peak = self.store.get_peak().await.ok().flatten();
        let synced = match peak {
            Some((hash, _)) => self.chain_is_current(&hash).await,
            None => false,
        };
        let was = self.synced.swap(synced, Ordering::Relaxed);
        // The not-synced -> synced rising edge is the sync->tip transition: fire the deferred
        // secondary-index build exactly once (during bulk sync the coin_record secondary
        // indexes are pure write-amplification; a
        // tip-serving node needs them). Off the follow path: Postgres builds CONCURRENTLY and
        // SQLite yields the writer between statements, so confirms continue underneath. CREATE
        // INDEX IF NOT EXISTS makes a restart-at-tip re-run a cheap no-op; on failure the latch
        // resets so the next rising edge (or a restart) retries. A successful build re-arms the
        // falling-edge shed below — the two latches alternate, so the drop/rebuild cycle can
        // only run once per genuine fall-behind/re-catch-up excursion.
        if synced && !was && !self.deferred_indexes_started.swap(true, Ordering::Relaxed) {
            let store = self.store.clone();
            let latch = self.deferred_indexes_started.clone();
            let shed_latch = self.service_indexes_shed.clone();
            tokio::spawn(async move {
                let started = std::time::Instant::now();
                match store.build_indexes().await {
                    Ok(()) => {
                        shed_latch.store(false, Ordering::Relaxed);
                        info!(
                            "deferred secondary indexes built at tip elapsed_ms={}",
                            started.elapsed().as_millis() as u64
                        );
                    }
                    Err(e) => {
                        latch.store(false, Ordering::Relaxed);
                        warn!(
                            "deferred index build failed; retrying on the next sync edge error={}",
                            e
                        );
                    }
                }
            });
        }
        // The FALLING edge: a node that reached tip (full index set built) and then fell DEEP
        // behind re-applies settled history while maintaining every secondary index, which turns
        // the confirm window into coin_record index/heap random reads with no HOT spend-updates.
        // Shed the secondary indexes once, off the follow path, and re-arm the build latch so the
        // rising edge rebuilds them at the next sync->tip transition, BEFORE the node re-enters
        // the reorg-exposed zone. Keyed on tip_lag depth, not the raw synced bit: `synced` flips
        // on a 1-block dip and would churn a multi-GB drop/rebuild. Arming by depth alone means a
        // restart mid-deep-catch-up re-derives the shed from the live phase and re-enters it with
        // a cheap idempotent re-drop instead of carrying stale state.
        if !synced {
            let local = peak.map_or(0, |(_, h)| h);
            let tip_lag = self
                .claimed_peak
                .load(Ordering::Relaxed)
                .saturating_sub(local);
            if local > 0
                && tip_lag > SHED_TIP_LAG_BLOCKS
                && !self.service_indexes_shed.swap(true, Ordering::Relaxed)
            {
                let store = self.store.clone();
                let build_latch = self.deferred_indexes_started.clone();
                let latch = self.service_indexes_shed.clone();
                tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    match store.shed_service_indexes().await {
                        Ok(()) => {
                            build_latch.store(false, Ordering::Relaxed);
                            info!(
                                "secondary indexes shed for deep re-catch-up elapsed_ms={} tip_lag={}",
                                started.elapsed().as_millis() as u64,
                                tip_lag
                            );
                        }
                        Err(e) => {
                            latch.store(false, Ordering::Relaxed);
                            warn!(
                                "index shed failed; retrying while still deep behind error={}",
                                e
                            );
                        }
                    }
                });
            }
        }
    }

    // — walk from the peak to the last transaction block and compare its
    // timestamp against now - 7 minutes. The walk is bounded generously: mainnet guarantees a
    // transaction block within far fewer records, and a missing/older-than-window record is simply
    // "not synced" — fail-closed on a stale chain.
    async fn chain_is_current(&self, peak_hash: &Bytes32) -> bool {
        let mut curr = self.store.get_block_record(peak_hash).await.ok().flatten();
        for _ in 0..512 {
            match &curr {
                None => return false,
                Some(rec) if rec.timestamp.is_some() => break,
                Some(rec) => {
                    curr = self
                        .store
                        .get_block_record(&rec.prev_hash)
                        .await
                        .ok()
                        .flatten();
                }
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        curr.and_then(|r| r.timestamp)
            .is_some_and(|ts| ts >= now.saturating_sub(60 * 7))
    }

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

    /// From-zero bulk sync: fetch the weight proof to the highest peer-announced tip, verify it, epoch-anchor
    /// the recent-chain headers, and download + confirm the recent bodies through the reservation window
    /// across all live outbound peers (the W==P contract). Returns the confirmed peak, or `None` if no tip or
    /// no live peer is available yet (retry next tick). After this lands, the cheap tip-follow driver takes
    /// over — this does not backfill deep history.
    ///
    /// # Errors
    /// Returns an I/O error if the weight-proof fetch, its validation, or the body download/confirm fails.
    pub async fn bulk_sync(
        &self,
        registry: &Arc<dyn OutboundPeers>,
    ) -> Result<Option<(Bytes32, u32)>, Error> {
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(None);
        };
        let sources: Vec<Arc<dyn BlockRangeSource>> = peers
            .iter()
            .map(|p| {
                let base = Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                    as Arc<dyn BlockRangeSource>;
                // When capturing, wrap each source so every downloaded block range is written to disk for
                // offline replay (capture happens at fetch, before confirm, so it records even ranges that
                // later fail to confirm).
                match &self.config.capture_dir {
                    Some(dir) => Arc::new(CapturingSource::new(base, dir.clone()))
                        as Arc<dyn BlockRangeSource>,
                    None => base,
                }
            })
            .collect();
        let peak = {
            let mut chaser = self.chaser.lock().await;
            let peak = chaser
                .fast_sync_with_summaries(&validated.wp, &validated.summaries, &sources)
                .await
                .map_err(|e| Error::other(e.to_string()))?;
            // Epoch-depth backfill (records only) so the next LIVE epoch-boundary retarget can walk
            // the previous epoch locally, then load the full ancestry into the engine's walk cache.
            // A backfill miss is retryable and must not fail the sync — the boundary is far ahead;
            // the fail-closed walk simply stays light until records exist.
            let anchor = validated
                .wp
                .recent_chain_data
                .first()
                .map(dg_xch_core::blockchain::header_block::HeaderBlock::height)
                .unwrap_or(0);
            match chaser
                .backfill_epoch_depth(&sources, &validated.summaries, anchor)
                .await
            {
                Ok(n) => log::info!("epoch-depth backfill complete records={}", n),
                Err(e) => {
                    log::warn!(
                        "epoch-depth backfill incomplete; will stay light near the boundary error={}",
                        e
                    )
                }
            }
            match chaser.warm_engine_cache().await {
                Ok(n) => {
                    log::info!("engine walk cache warmed from store records={}", n);
                    // mm-OOM visibility: a pod that dies seconds after start still leaves its
                    // post-warm memory shape in the log (the OOMed node left zero allocation evidence).
                    crate::metrics::log_startup_memory("fast_sync", n);
                }
                Err(e) => log::warn!("engine cache warm failed error={}", e),
            }
            peak
        };
        if peak.is_some() {
            self.update_synced().await;
        }
        Ok(peak)
    }

    pub(crate) async fn long_sync_anchored(&self) -> bool {
        self.long_sync_anchor.read().await.is_some()
    }

    pub(crate) async fn sync_from_anchored(&self) -> bool {
        self.sync_from_anchor.read().await.is_some()
    }

    /// Establish — once per landing — the `_sync` trust anchor for a MID-CHAIN deep gap
    ///: a validated weight proof for the heaviest claim (cached
    /// across ticks by [`Node::validated_proof`]), the fork point of its summaries against our
    /// chain (`get_fork_point`), the `check_fork_next_block` peer
    /// probe, and — when the fork point is below our
    /// peak (the offline period saw a reorg deeper than our tip) — the reland through the
    /// engine's atomic reorg. Returns `true` once anchored (the detached fetch/confirm pipeline
    /// then batch-syncs the gap), `false` to retry next tick (no claim, peers, proof, or peak
    /// yet).
    ///
    /// # Errors
    /// Returns an I/O error on a failed proof fetch/validation, an unresolvable fork point, or a
    /// reland that could not move the peak.
    async fn ensure_long_sync_anchor(
        &self,
        registry: &Arc<dyn OutboundPeers>,
    ) -> Result<bool, Error> {
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(false);
        };
        if self.long_sync_anchor.read().await.as_ref() == Some(&validated.tip) {
            return Ok(true);
        }
        let Some((peak_hash, peak_height)) = self
            .store
            .get_peak()
            .await
            .map_err(|e| Error::other(e.to_string()))?
        else {
            // No confirmed peak: the from-zero fast-sync arm owns the band, never this anchor.
            return Ok(false);
        };
        let fork = wp_fork_point(
            self.store.as_ref(),
            &validated.summaries,
            self.constants.sub_epoch_blocks,
        )
        .await
        .map_err(|e| Error::other(e.to_string()))?;
        // check_fork_next_block probes ONLY the no-fork case (fork point == the
        // no-divergence conservative value); a detected divergence keeps its fork point.
        let connects = match &fork {
            WpForkPoint::NoForkDetected { .. } => {
                next_block_connects(&peers, peak_hash, peak_height).await
            }
            _ => false,
        };
        match long_sync_plan(&fork, peak_height, connects) {
            LongSyncPlan::Extend => {}
            LongSyncPlan::Rewind { fork_point } => {
                info!(
                    "long sync: WP fork point below the local peak; relanding through the engine reorg fork_point={} peak_height={}",
                    fork_point, peak_height
                );
                self.long_sync_rewind(&peers, fork_point).await?;
            }
            LongSyncPlan::Stall => {
                return Err(Error::other(format!(
                    "long sync: no WP fork point within the walk window (peak {peak_height}); \
                     refusing to batch-sync toward a chain the proof does not attest"
                )));
            }
        }
        *self.long_sync_anchor.write().await = Some(validated.tip);
        info!(
            "long-sync landing anchored (weight proof validated, fork point resolved) peak_height={} tip={}",
            peak_height, validated.tip
        );
        Ok(true)
    }

    // The reorg-across-the-gap reland (the fork point below the peak): drive the
    // chaser's window re-follow from the fork point with the Node's per-peak side effects
    // (wallet coin-state + mempool revalidation fire for every reorg delta, exactly as in
    // [`Node::sync_backtrack`]).
    async fn long_sync_rewind(
        &self,
        peers: &[Arc<OutboundPeer>],
        fork_point: u32,
    ) -> Result<(), Error> {
        let Some(peer) = peers.first() else {
            return Err(Error::other("long-sync reland: no live peer"));
        };
        let source: Arc<dyn BlockRangeSource> =
            Arc::new(OutboundPeerSource::new(peer.clone(), REQUEST_TIMEOUT));
        self.follow_inflight_since.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        let stepped = {
            let mut chaser = self.chaser.lock().await;
            chaser.long_sync_reland_reporting(&source, fork_point).await
        };
        self.follow_inflight_since.store(0, Ordering::Relaxed);
        let (peak, deltas) = stepped.map_err(|e| Error::other(e.to_string()))?;
        self.finish_follow_step(peak, &deltas)
            .await
            .map_err(|e| Error::other(e.to_string()))?;
        Ok(())
    }

    /// One-time mid-chain anchor for `--sync-from H`: validate the weight proof for the
    /// claimed tip (its sub-epoch summaries give the WHOLE chain's epoch schedule), download the
    /// span just below H from a live peer, run the existing headers-first candidate pass over it
    /// against that schedule, and warm the engine cache. The follow driver then body-syncs from
    /// the span's start; the first bodies anchor on the candidate ancestry exactly like a
    /// weight-proof checkpoint. Returns `false` (retry next tick) until a peer + proof exist.
    ///
    /// # Errors
    /// Returns an I/O error if the header pass rejects the fetched span.
    async fn anchor_at(&self, registry: &Arc<dyn OutboundPeers>, h: u32) -> Result<bool, Error> {
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(false);
        };
        let start = h.saturating_sub(64);
        let end = h.saturating_add(31);
        // Peers reject RequestBlocks spans wider than 32, so the anchor span is fetched in
        // 32-block chunks; a peer that fails any chunk is abandoned for the next peer.
        let mut fetched = None;
        'peers: for peer in &peers {
            let source = OutboundPeerSource::new(peer.clone(), REQUEST_TIMEOUT);
            let mut span = Vec::new();
            let mut lo = start;
            while lo <= end {
                let hi = end.min(lo + 31);
                match source.fetch_range(lo, hi).await {
                    Ok(blocks) if !blocks.is_empty() => span.extend(blocks),
                    Ok(_) | Err(_) => continue 'peers,
                }
                lo = hi + 1;
            }
            fetched = Some(span);
            break;
        }
        let Some(mut blocks) = fetched else {
            warn!(
                "sync-from anchor: no peer served the anchor span; retrying start={} end={} peers={}",
                start,
                end,
                peers.len()
            );
            return Ok(false);
        };
        blocks.sort_by_key(dg_xch_core::blockchain::full_block::FullBlock::height);
        let headers: Vec<_> = blocks
            .iter()
            .map(dg_xch_node::header_block_from_full_block)
            .collect();
        let mut chaser = self.chaser.lock().await;
        let schedule = chaser.epoch_schedule(&validated.summaries);
        chaser
            .sync_headers(&headers, &schedule, &validated.summaries)
            .await
            .map_err(|e| Error::other(e.to_string()))?;
        // The proof's summary chain outlives the anchor span: the first included-SES block ABOVE
        // the span has neither local ancestry nor a headers-first candidate to serve its summary
        // — the engine falls back to this chain, hash-gated as ever.
        chaser.seed_summary_chain(validated.summaries.to_vec());
        // The anchor span alone cannot serve the FIRST epoch retarget the follow hits: its
        // `get_second_to_last_transaction_block_in_previous_epoch` walk reads records back past
        // the previous epoch surpass — up to a full epoch below the span (the 4,575,744-boundary
        // wall: --sync-from=4575000 seeded [4574936, 4575031], staging 4,575,758 walked to
        // 4,571,135 and died on "block record not found"). Backfill those records headers-first
        // now, exactly as the from-zero bulk sync does after its weight-proof landing. Fail
        // closed: without them the follow WILL wall at the boundary, so retry the anchor next
        // tick rather than establish a known-incomplete one.
        let sources: Vec<Arc<dyn BlockRangeSource>> = peers
            .iter()
            .map(|p| {
                Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                    as Arc<dyn BlockRangeSource>
            })
            .collect();
        match chaser
            .backfill_epoch_depth(&sources, &validated.summaries, start)
            .await
        {
            Ok(n) => info!("sync-from epoch-depth backfill complete records={}", n),
            Err(e) => {
                warn!(
                    "sync-from epoch-depth backfill failed; retrying anchor next tick error={}",
                    e
                );
                return Ok(false);
            }
        }
        if let Err(e) = chaser.warm_engine_cache().await {
            warn!("sync-from cache warm failed error={}", e);
        }
        *self.sync_from_anchor.write().await = Some(start);
        info!("sync-from anchor established anchor={} target={}", start, h);
        Ok(true)
    }

    async fn local_peak_weight(&self) -> Option<u128> {
        let (hash, _) = self.store.get_peak().await.ok().flatten()?;
        self.store
            .get_block_record(&hash)
            .await
            .ok()
            .flatten()
            .map(|rec| rec.weight)
    }

    /// The sync target: the HEAVIEST live peer claim (`sync_store.get_heaviest_peak`, already
    /// net of quarantined peaks and retracted/stale claims), gated to claims strictly heavier than
    /// our confirmed peak — lighter announcements drop at `new_peak` ("Not interested in less
    /// heavy peaks") and `request_validate_wp` refuses a target not heavier than the local peak
    /// ("already caught up"). `None` means caught up or nothing (heavier) claimed: a longer-but-
    /// LIGHTER fork is not a target, and every consumer band (tip_follower / FOLLOW fill / bulk)
    /// idles instead of grinding toward it.
    pub(crate) async fn sync_target(&self) -> Option<PeakClaim> {
        let heaviest = self.peak_book.heaviest()?;
        match self.local_peak_weight().await {
            Some(local_weight) if heaviest.weight <= local_weight => None,
            _ => Some(heaviest),
        }
    }

    /// A validated weight proof for the current claimed tip: the cached one when present (validate AT
    /// MOST ONCE per landing — reuse ANY already-validated proof, NOT only an exact-tip match; mainnet's
    /// tip advances every block, so an exact match never holds and the node would re-verify the
    /// multi-minute proof every tick), else fetch racing every live peer and run the full six-phase
    /// verification off the async runtime. `None` when no claimed tip or no live peer exists yet.
    ///
    /// # Errors
    /// Returns an I/O error if every peer fails the fetch or the proof fails validation.
    async fn validated_proof(
        &self,
        peers: &[Arc<OutboundPeer>],
    ) -> Result<Option<ValidatedTip>, Error> {
        let Some(target) = self.sync_target().await else {
            return Ok(None);
        };
        let (tip, tip_height, tip_weight) = (target.header_hash, target.height, target.weight);
        if peers.is_empty() {
            return Ok(None);
        }
        if let Some(v) = self.validated_tip.read().await.clone() {
            info!("reusing validated weight proof cached_tip={}", v.tip);
            return Ok(Some(v));
        }
        // Race the weight-proof request across EVERY live peer and take the first that answers. A single
        // peer that is slow, swamped, or unwilling to serve a multi-MB proof must not stall the sync (one
        // laggy peer would otherwise eat the whole timeout every tick). The first valid proof wins; the
        // rest are aborted. The serving peer travels with the proof so a proof that fails the claim
        // cross-check or validation can evict exactly that peer (the failed-proof eviction).
        info!(
            "fast-sync: fetching weight proof (racing all peers) tip_height={} peers={}",
            tip_height,
            peers.len()
        );
        let mut fetches = tokio::task::JoinSet::new();
        for peer in peers {
            let peer = peer.clone();
            fetches.spawn(async move {
                let fetched =
                    request_weight_proof(&peer, tip, tip_height, WEIGHT_PROOF_TIMEOUT).await;
                (peer, fetched)
            });
        }
        let mut fetched = None;
        let mut failures = 0usize;
        while let Some(joined) = fetches.join_next().await {
            match joined {
                Ok((peer, Ok(proof))) => {
                    fetched = Some((peer, proof));
                    break;
                }
                Ok((_, Err(e))) => {
                    failures += 1;
                    warn!(
                        "weight-proof fetch from a peer failed, awaiting others error={}",
                        e
                    );
                }
                Err(e) => {
                    failures += 1;
                    warn!("weight-proof fetch task join error error={}", e);
                }
            }
        }
        fetches.abort_all();
        let Some((wp_peer, proof)) = fetched else {
            // NO peer will serve a proof for this claimed tip: retract the claim (every claimant) so a
            // phantom peak cannot be re-selected tick after tick. Honest claimants re-announce within a
            // block cadence and repopulate the book — the soft analog of closing the peer that
            // failed to serve the proof (request_validate_wp → peer.close).
            self.peak_book.retract_hash(&tip);
            return Err(Error::other(format!(
                "weight-proof fetch failed from all {failures} peers; retracted claims on tip {tip}"
            )));
        };
        // `request_validate_wp`: the proof must attest EXACTLY the claimed tip — its recent chain's
        // last block carries the claimed height AND weight. A mismatch quarantines the claimed
        // peak (never re-selected) and evicts the
        // serving peer.
        let attested = proof
            .recent_chain_data
            .last()
            .map(|h| (h.height(), h.weight()));
        if attested != Some((tip_height, tip_weight)) {
            self.peak_book.quarantine(tip, tip_height);
            wp_peer.stop();
            return Err(Error::other(format!(
                "weight proof attests {attested:?}, claim was ({tip_height}, {tip_weight}); peak {tip} quarantined"
            )));
        }
        // Refuse a proof whose recent chain rides through ANY quarantined peak
        // (an extension of a poisoned chain re-offered under a fresh tip hash).
        for header in &proof.recent_chain_data {
            if let Ok(hash) = header.header_hash()
                && self.peak_book.is_quarantined(&hash)
            {
                return Err(Error::other(format!(
                    "weight proof rides through quarantined peak {hash}"
                )));
            }
        }
        // Debug: dump the raw proof bytes so a real mainnet weight proof can be captured as an offline
        // validation fixture (validating a fetched-fresh proof live is minutes; a fixture makes it
        // deterministic and profilable). Off unless --dump-weight-proof-dir is set.
        if let Some(dir) = &self.config.capture_dir {
            match proof.to_bytes(ChiaProtocolVersion::default()) {
                Ok(bytes) => {
                    let path = dir.join(format!("weight_proof_{tip_height}.bin"));
                    match std::fs::write(&path, &bytes) {
                        Ok(()) => {
                            info!(
                                "dumped weight-proof fixture path={} bytes={}",
                                path.display(),
                                bytes.len()
                            )
                        }
                        Err(e) => warn!("failed to write weight-proof dump error={}", e),
                    }
                }
                Err(e) => warn!("failed to serialize weight proof for dump error={:?}", e),
            }
        }
        let wp = Arc::new(proof);
        // Verify off the async runtime (blocking pool) AND off the chaser lock: the verify is CPU-bound
        // for minutes and must not stall the tip-follow driver or hold the chaser mutex meanwhile.
        let constants = self.constants;
        let wp_for_verify = wp.clone();
        info!(
            "fast-sync: validating weight proof (spawn_blocking) tip_height={}",
            tip_height
        );
        let verified = tokio::task::spawn_blocking(move || {
            dg_xch_weight_proof::validate_weight_proof(&wp_for_verify, &constants)
        })
        .await
        .map_err(|e| Error::other(format!("weight-proof verify task: {e}")))?;
        let summaries = match verified {
            Ok((true, summaries)) => summaries,
            // The proof does NOT prove the claimed peak: quarantine it (
            // a poisoned peak is never re-selected) and evict the peer that served the bad proof
            // (a peer eviction would be the harder posture).
            Ok((false, _)) => {
                self.peak_book.quarantine(tip, tip_height);
                wp_peer.stop();
                return Err(Error::other(format!(
                    "weight proof did not validate; peak {tip} quarantined"
                )));
            }
            Err(e) => {
                self.peak_book.quarantine(tip, tip_height);
                wp_peer.stop();
                return Err(Error::other(format!(
                    "weight proof: {e:?}; peak {tip} quarantined"
                )));
            }
        };
        let v = ValidatedTip {
            tip,
            wp,
            summaries: Arc::new(summaries),
        };
        *self.validated_tip.write().await = Some(v.clone());
        Ok(Some(v))
    }

    /// One-shot resume repair for a process restart over an existing chain (the live 9,143,851 and
    /// 9,143,94x walls): warm the engine's walk cache from the store, and — when the store's record
    /// floor is too shallow for the next epoch-boundary retarget — fetch+validate a weight proof and
    /// backfill records to epoch depth. Returns `true` when repair is complete (or nothing needed),
    /// `false` to retry next tick (no tip/peers yet, or the backfill missed).
    ///
    /// `deepen` is set by the consumer's MissingRecord recovery: a stage walk still missed a
    /// record even though the floor walk reads clean, meaning the walk pierced BELOW
    /// `epoch_backfill_low` (a two-transaction-block scan crossing a non-transaction run longer
    /// than `EPOCH_BACKFILL_SLACK` under the previous epoch surpass). In that case the backfill
    /// anchors at the walk's reach point — one full epoch below the standard floor — instead of
    /// concluding "nothing to do" and re-warming the identical span forever (a livelock).
    ///
    /// # Errors
    /// Returns an I/O error on a store failure or a failed proof fetch/validation.
    async fn resume_repair(
        &self,
        registry: &Arc<dyn OutboundPeers>,
        deepen: bool,
    ) -> Result<bool, Error> {
        let Some((peak_hash, local)) = self
            .store
            .get_peak()
            .await
            .map_err(|e| Error::other(e.to_string()))?
        else {
            return Ok(true); // Empty store: fast-sync owns the from-zero path.
        };
        {
            let mut chaser = self.chaser.lock().await;
            match chaser.warm_engine_cache().await {
                Ok(n) => {
                    info!("engine walk cache warmed (resume) records={}", n);
                    // mm-OOM visibility: the resume path is the one the OOMing node takes on every
                    // restart — this self-report is the allocation evidence its 8-second life lacked.
                    crate::metrics::log_startup_memory("resume", n);
                }
                Err(e) => warn!("engine cache warm failed (resume) error={}", e),
            }
        }
        // The deepest record the next possible epoch retarget can read from this peak — the
        // PENDING boundary's previous-surpass depth (`epoch_backfill_low`), NOT the boundary
        // rounded up from the peak: that rounding concluded "nothing to backfill" on every
        // restart while the peak sat 13 blocks past boundary 4,575,744 with its retarget still
        // pending, leaving the follow loop walled on "block record not found" forever.
        let needed_low = dg_xch_node::sync::epoch_backfill_low(
            local,
            self.constants.epoch_blocks,
            self.constants.sub_epoch_blocks,
        );
        // The record floor is measured by PREV-HASH WALK from the peak (crate::resume_floor), not
        // by height. By-height lookups are main-chain-only on every backend, so epoch-backfill
        // CANDIDATE records are invisible to them, and a by-height binary search assumes hole-free
        // monotone presence, so a mid-span record hole above the floor reads as "nothing to
        // repair". The hash walk sees candidates and breaks exactly at a hole.
        let outcome = crate::resume_floor::measure_record_floor(
            self.store.as_ref(),
            peak_hash,
            local,
            needed_low,
        )
        .await
        .map_err(|e| Error::other(e.to_string()))?;
        let anchor = match outcome {
            crate::resume_floor::RecordFloor::Reached { floor } if !deepen || floor == 0 => {
                return Ok(true);
            }
            // Deepened repair (see the doc comment): backfill anchored at the reach point, one
            // epoch below the standard floor.
            crate::resume_floor::RecordFloor::Reached { floor } => floor,
            crate::resume_floor::RecordFloor::Broken { stop } => stop,
        };
        let peers = registry.live_peers().await;
        let Some(validated) = self.validated_proof(&peers).await? else {
            return Ok(false);
        };
        let sources: Vec<Arc<dyn BlockRangeSource>> = peers
            .iter()
            .map(|p| {
                Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                    as Arc<dyn BlockRangeSource>
            })
            .collect();
        let mut chaser = self.chaser.lock().await;
        match chaser
            .backfill_epoch_depth(&sources, &validated.summaries, anchor)
            .await
        {
            Ok(n) => info!(
                "resume epoch-depth backfill complete records={} anchor={}",
                n, anchor
            ),
            Err(e) => {
                warn!("resume backfill incomplete, retrying next tick error={}", e);
                return Ok(false);
            }
        }
        match chaser.warm_engine_cache().await {
            Ok(n) => info!("engine walk cache re-warmed after backfill records={}", n),
            Err(e) => warn!("engine cache warm failed after backfill error={}", e),
        }
        Ok(true)
    }

    /// Record the peer a gossiped transaction arrived FROM — its dispatch peer id AND its remote
    /// host — so the `NewTransaction` re-broadcast excludes it (`broadcast_added_tx`'s
    /// `current_peer`). The host is what excludes an OUTBOUND origin, whose
    /// dispatch id is our own shared client-cert hash. Bounded: entries
    /// older than 60s are pruned on insert and the map is capped — an unconsumed entry (failed
    /// admission) cannot accumulate.
    pub async fn note_tx_origin(&self, txid: Bytes32, peer: Bytes32, host: Option<IpAddr>) {
        record_tx_origin(
            &self.tx_origin,
            txid,
            TxOrigin {
                peer_id: peer,
                host,
            },
        )
        .await;
    }

    /// Drain the queued `NewTransaction` announcements to every connected FULL_NODE peer —
    /// inbound and outbound — excluding each transaction's origin peer. Public so
    /// the integration suite can drive the drain the driver loop normally runs.
    pub async fn drain_tx_announcements(self: &Arc<Self>, registry: &Arc<dyn OutboundPeers>) {
        broadcast_transactions(self, registry).await;
    }

    /// The sync-end transition — `_finish_sync`. After a bulk/
    /// fast-sync band lands its recent-chain peak and exits to the follow driver, fire peak-post-
    /// processing ONCE against the final peak, with EMPTY coin deltas:
    ///   - mempool revalidation against the new (transaction) peak — the empty spent set
    ///     expires time-locked items and re-admits parked ones without dropping for spends (the
    ///     batch path already advanced the coin set);
    ///   - NewPeak to full-node peers + slot-state advance + NewPeakTimelord to timelords;
    ///   - NewPeakWallet to wallet peers.
    ///
    /// There is NO per-coin `CoinStateUpdate`: `lookup_coin_ids` is EMPTY at `_finish_sync`
    /// (the empty StateChangeSummary), so subscribers get only the peak announcement — the bounded
    /// push the sync-end owes, NEVER an unbounded per-block replay of the synced span. A wallet
    /// re-pages the coin state it missed via RequestPuzzleState/RequestCoinState anchored on the
    /// announced peak. Called by the driver on the fast-sync landing (the band-exit seam that the
    /// per-block follow side effects never covered).
    pub async fn finish_sync_transition(
        self: &Arc<Self>,
        registry: &Arc<dyn OutboundPeers>,
        inbound_peers: &PeerMap,
    ) {
        let Ok(Some((hash, height))) = self.store.get_peak().await else {
            return;
        };
        // The next follow block chains onto this landed peak as a plain extension — seed the
        // reorg-chain tracker so it is not mis-read as a hash-break reorg.
        *self.last_delta_hash.lock().await = Some(hash);
        // Mempool revalidation against the transaction peak framing the new tip.
        if let Some((tx_height, tx_ts)) = self.tx_peak_frame(hash).await
            && let Err(e) = self
                .mempool
                .lock()
                .await
                .new_peak(self.store.as_ref(), tx_height, tx_ts, &[])
                .await
        {
            warn!("sync-end mempool revalidation failed error={}", e);
        }
        // NewPeak to full-node peers + slot-state advance + NewPeakTimelord to timelords.
        broadcast_new_peak(self, registry, hash, height).await;
        update_slot_state_on_peak(self, hash).await;
        broadcast_new_peak_timelord(self, inbound_peers, hash).await;
        // NewPeakWallet to wallet peers (no per-coin CoinStateUpdate — an empty summary).
        // fork_point = max(height - 1, 0): `_finish_sync` builds `StateChangeSummary(peak,
        // fork_point, …)` with `fork_point = max(peak.height - 1, 0)` and update_wallets carries
        // that same fork height into NewPeakWallet — the height-1 default the
        // NewPeak broadcast above also uses, NOT the on-connect greeting's peak-height convention.
        let wallets = wallet_peers(inbound_peers).await;
        if !wallets.is_empty()
            && let Ok(Some(rec)) = self.store.get_block_record(&hash).await
        {
            let announce = NewPeakWallet {
                header_hash: hash,
                height,
                weight: rec.weight,
                fork_point_with_previous_peak: height.saturating_sub(1),
            };
            broadcast_new_peak_wallet(&self.net, &wallets, &announce).await;
        }
        info!("sync-end transition fired height={}", height);
    }

    // The transaction block framing a peak: walk from the peak to the nearest record carrying a
    // timestamp (`get_tx_peak`). Bounded like `chain_is_current`; `None` if none within the
    // window (a from-genesis peak with no transaction block yet).
    async fn tx_peak_frame(&self, peak_hash: Bytes32) -> Option<(u32, u64)> {
        let mut curr = self
            .store
            .get_block_record(&peak_hash)
            .await
            .ok()
            .flatten()?;
        for _ in 0..512 {
            if let Some(ts) = curr.timestamp {
                return Some((curr.height, ts));
            }
            curr = self
                .store
                .get_block_record(&curr.prev_hash)
                .await
                .ok()
                .flatten()?;
        }
        None
    }

    // One confirmed block's side effects: drop mempool items the block spent, then emit wallet coin-state
    // updates for the coins it created/spent. Bounded work — proportional to the block's coin delta.
    // `reorg` is Some on the first re-applied block of a landed reorg (the chaser's
    // [`ConfirmedDelta`] feed): the rolled-back coin states are pushed to subscribers and the true
    // fork height replaces the height-1 simplification.
    /// Apply a locally-produced peak's wallet-facing effects: revalidate the mempool at the new
    /// peak, roll wallet subscriptions forward (or back on a reorg), and push `CoinStateUpdate` +
    /// `NewPeakWallet` to every subscribed wallet peer. A simulator that produces blocks out of band
    /// drives this directly, since it does not run the follow loop that normally calls it.
    pub async fn notify_new_peak(
        &self,
        d: &BlockDelta,
        reorg: Option<&ReorgWalletDelta>,
    ) -> Result<(), Error> {
        // Reorg detection by hash-chain break: the engine may surface a deep reorg as just the
        // new tip's delta, so height monotonicity can't be trusted — a delta whose prev_hash
        // isn't the last delta we processed means blocks were rolled back. The mempool takes the
        // slow path there (the full pool rebuild):
        // `Mempool::revalidate_for_reorg` — drop items whose removals ceased to exist
        // (UNKNOWN_UNSPENT) or were spent on the winning branch, rebase surviving FF spends.
        // The threaded reorg delta forces the same path even when the branch's first block
        // happens to chain onto the last processed delta's hash.
        let reorged = {
            let mut last = self.last_delta_hash.lock().await;
            let broke_chain = last.is_some_and(|h| h != d.prev_hash);
            *last = Some(d.header_hash);
            broke_chain || reorg.is_some()
        };
        if reorged {
            let dropped = self
                .mempool
                .lock()
                .await
                .revalidate_for_reorg(self.store.as_ref())
                .await
                .map_err(|e| Error::other(e.to_string()))?;
            info!(
                "reorg landing: mempool revalidated on the slow path height={} dropped={}",
                d.height, dropped
            );
        }
        // `mempool_manager.new_peak`: "we're only interested in transaction blocks" — the
        // mempool peak must always be the most recent TRANSACTION block, whose height + timestamp
        // are the reference frame every time-lock admission checks against. A non-transaction
        // delta (timestamp 0, no foliage_transaction_block, no coin activity) leaves the pool
        // untouched.
        if d.timestamp != 0 {
            let peak_result = self
                .mempool
                .lock()
                .await
                .new_peak(self.store.as_ref(), d.height, d.timestamp, &d.removals)
                .await
                .map_err(|e| Error::other(e.to_string()))?;
            // Parked bundles that became admissible at this peak re-gossip like fresh admissions.
            if !peak_result.admitted.is_empty() {
                let mut announces = self.tx_announce.lock().await;
                for (name, cost, fees) in peak_result.admitted {
                    announces.push(NewTransaction {
                        transaction_id: name,
                        cost,
                        fees,
                    });
                }
            }
        }
        // The true fork height when this delta lands a reorg (threaded into every wallet
        // push); height-1 IS the fork point for every plain extension.
        let fork_height = reorg.map_or_else(|| d.height.saturating_sub(1), |r| r.fork_height);
        // Rolled-back states FIRST, then the branch's own delta — the same final state as one
        // combined `rolled_back_records + new_states` push.
        if let Some(r) = reorg
            && !r.rolled_back.is_empty()
        {
            self.wallet
                .notify_coin_states(d.header_hash, d.height, fork_height, &r.rolled_back)
                .await;
        }
        self.wallet
            .on_new_peak(
                self.store.as_ref(),
                crate::wallet::WalletUpdate {
                    peak_hash: d.header_hash,
                    height: d.height,
                    fork_height,
                    created: &d.additions,
                    spent_ids: &d.removals,
                    // The block's create-coin (hint, coin_id) pairs: a hint equal to a subscribed
                    // puzzle hash matches like the puzzle hash itself.
                    hints: &d.hints,
                },
            )
            .await
            .map_err(|e| Error::other(e.to_string()))?;
        // AFTER the per-subscriber CoinStateUpdate deltas, EVERY wallet-type peer gets the peak
        // as NewPeakWallet — subscribed or not (Sage
        // tracks the network peak from this push, and its delta sync anchors on it). Snapshot the
        // wallet peers first: with none connected (the common case, and every bulk-sync block)
        // this is one cheap read-lock and no store read. fork_point is the true fork height on a
        // threaded reorg delta and height-1 for every plain extension.
        let wallets = wallet_peers(&self.inbound_peers).await;
        if !wallets.is_empty()
            && let Ok(Some(rec)) = self.store.get_block_record(&d.header_hash).await
        {
            let announce = NewPeakWallet {
                header_hash: d.header_hash,
                height: d.height,
                weight: rec.weight,
                fork_point_with_previous_peak: fork_height,
            };
            broadcast_new_peak_wallet(&self.net, &wallets, &announce).await;
        }
        Ok(())
    }

    /// Run the node to completion: start the peer + RPC servers, bootstrap peer connectivity from the
    /// introducer, hold the outbound peer set, drive tip-follow sync toward the highest peer-announced peak,
    /// and block until a shutdown signal drains everything.
    ///
    /// # Errors
    /// Returns an I/O error if a server fails to start.
    /// Boot the protocol servers and the outbound supervisor, returning the shared
    /// service state the portfu tasks operate through. The sync driver, tip
    /// follower, and reaper no longer spawn here — they are portfu tasks.
    pub async fn start_services(self: &Arc<Self>) -> Result<crate::service::NodeServices, Error> {
        install_crypto_provider();
        let (peer_run, inbound_peers) = self.spawn_peer_server()?;
        let rpc_run = self.spawn_rpc_server()?;

        let mut supervisor = Supervisor::new(self.config.p2p);
        // Register the full_node handler set on every outbound dial BEFORE the slots start.
        supervisor.set_handlers(self.outbound_handler_factory());
        // On-connect greetings for OUTGOING connections (start_client →
        // on_connect): NewPeak + mempool-sync request to every peer we dial.
        {
            let hook_node = self.clone();
            supervisor.set_on_connect(Arc::new(move |peer| {
                let node = hook_node.clone();
                Box::pin(async move { outbound_on_connect(&node, peer.as_ref()).await })
            }));
        }
        // Manual peers are seeded directly into the address book, independent of the introducer: the outbound
        // slots dial them and reclaim-on-drop re-dials them, so a node pointed only at trusted full nodes
        // syncs with no introducer at all.
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
        // The RETRYING introducer session: a one-shot seed dies on boot-time DNS-not-ready and
        // the node stays peer-poor forever. The supervisor
        // session re-queries on a doubling backoff whenever the node is below its outbound target
        // with an empty address book.
        if let Some((host, port)) = &self.config.introducer {
            supervisor.start_introducer(host, *port);
        }
        supervisor.start_outbound();

        let peer_registry = supervisor.registry.clone();
        let registry: Arc<dyn OutboundPeers> = peer_registry.clone();
        Ok(crate::service::NodeServices {
            registry,
            peer_registry,
            inbound_peers,
            peer_run,
            rpc_run,
            supervisor: tokio::sync::Mutex::new(Some(supervisor)),
        })
    }

    /// Start the Prometheus `/metrics` server (`--metrics`, default on): exports the sync-pipeline counters,
    /// the confirmed + claimed peak heights, process RSS, and peer counts in Prometheus text format. Returns
    /// the server's run flag, or `None` when `--metrics off` disabled it. A bind failure is logged, not fatal.
    /// The `/metrics` + `/health` read handles for this node, wired to the live
    /// peer registries. Health is boot-anchored here, so the grace window opens
    /// when the process's servers come up.
    pub fn metrics_sources(&self, services: &crate::service::NodeServices) -> MetricsSources<S> {
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

    #[must_use]
    pub fn constants(&self) -> &ConsensusConstants {
        &self.constants
    }
}

pub(crate) async fn reap_wallet_subscriptions_once<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<Node<S>>,
    inbound_peers: &PeerMap,
) {
    let live: std::collections::HashSet<Bytes32> =
        inbound_peers.read().await.keys().copied().collect();
    node.wallet.retain_live(&live).await;
}

pub(crate) async fn tip_follower<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<Node<S>>,
    registry: Arc<dyn OutboundPeers>,
    inbound_peers: PeerMap,
) {
    let mut rotation = 0usize;
    while node.run.load(Ordering::Relaxed) {
        // Wake on a NewPeak advance; the idle timeout is only a backstop against a lost wakeup.
        tokio::select! {
            () = node.new_peak_signal.notified() => {}
            () = tokio::time::sleep(TIP_FOLLOW_IDLE) => {}
        }
        if !node.run.load(Ordering::Relaxed) {
            break;
        }
        let Some((_, local)) = node.store.get_peak().await.ok().flatten() else {
            // No confirmed peak yet: from-zero catch-up is the driver's bulk/batch job.
            continue;
        };
        // The weight-gated heaviest claim (lighter-than-local announcements are
        // dropped, so a longer-but-lighter fork never becomes the near-tip pull target).
        let Some(target) = node.sync_target().await else {
            continue;
        };
        let claimed = target.height;
        if !in_near_tip_band(local, claimed, true) {
            continue;
        }
        // Entering the near-tip band: per-block commits + the active WAL checkpointer keep the WAL tiny.
        node.store.set_near_tip(true);
        let peers = registry.live_peers().await;
        if peers.is_empty() {
            continue;
        }
        let peer = peers[rotation % peers.len()].clone();
        rotation = rotation.wrapping_add(1);
        let source: Arc<dyn BlockRangeSource> =
            Arc::new(OutboundPeerSource::new(peer, REQUEST_TIMEOUT));
        // `new_peak` ladder: forward-extend [local+1, claimed] first (a direct child of the peak
        // is the common case and needs one forward fetch, no backward peak-refetch), and fall to
        // short_sync_backtrack only on the unknown-parent orphan — so the follower pins tip at lag 0-1.
        match node
            .sync_tip_step(&source, local.saturating_add(1), claimed)
            .await
        {
            Ok(Some((hash, height))) => {
                broadcast_new_peak(&node, &registry, hash, height).await;
                update_slot_state_on_peak(&node, hash).await;
                broadcast_new_peak_timelord(&node, &inbound_peers, hash).await;
            }
            Ok(None) => {}
            // A deeper reorg or a peer that cannot serve the tip: defer to the driver's batch/bulk bands.
            Err(e) => {
                debug!(
                    "tip-follow step deferred to the driver local={} claimed={} error={}",
                    local, claimed, e
                );
            }
        }
    }
}

// Consumer idle backstop: the block processor is event-driven on the queue's `ready` signal; this tick
// only exists so a shutdown (run flag cleared) is observed even when no block is arriving.
const CONSUMER_IDLE: Duration = Duration::from_secs(2);
// Bound on the ConfirmedPeak announcer channel: sized well past the queue window depth so
// the height-monotone SPSC feed never backpressures the consumer under a normal peak cadence.
const PEAK_CHANNEL_CAP: usize = 256;
// Bound on the consumer→driver recovery channel: recovery is rare (reorg / --sync-from ref miss / epoch
// wall), one outstanding request at a time in practice; a small cap is ample and keeps it bounded.
const RECOVERY_CHANNEL_CAP: usize = 8;
// Stall-reclaim bound for the DECOUPLED fetch/confirm pipeline (a whole-pipeline liveness backstop). The
// bulk `download_worker` reclaims a per-reservation stall to the pool; the decoupled genesis/follow
// pipeline (WindowReadahead + BlockQueue) has no such reclaim — every individual fetch is timeout-bounded
// but nothing detects the pipeline AS A WHOLE ceasing to advance. This is that whole-pipeline watchdog
// bound: if the confirmed frontier (`queue.low_water`) does not advance for this long WHILE work remains,
// peers are live, and no confirm is legitimately in flight, the driver force-rebases to break the wedge.
// 60s = 2× REQUEST_TIMEOUT (the longest a single window fetch can legitimately stall outside a confirm),
// matching the node's existing 60s liveness convention; confirm time is excluded via `follow_inflight_since`
// so healthy — even slow — validation never trips it.
const RECLAIM_TIMEOUT: Duration = Duration::from_secs(60);
// Bound on how long the peer-free consumer parks on a recovery reply before giving up and retrying the
// window, so a stuck driver loop can never hang the confirm consumer forever. Set above the worst
// legitimate `handle_recovery` (MissingRecord's 8xDRIVER_TICK re-arm, a bounded backtrack's fetches)
// so it never abandons an in-progress recovery.
const RESET_REPLY_TIMEOUT: Duration = Duration::from_secs(120);

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The consumer→driver recovery channel. The detached, peer-free
/// [`block_processor`] emits one of these when a confirmed window needs a peer at confirm time and parks
/// on the reply **holding no engine/`Chaser` lock**. The thin driver services each by running the
/// EXISTING, unmoved recovery routines with its rotation peer, then
/// rebases the queue to restore the head invariant (`queue.low_water == confirmed_peak + 1`).
/// Recovery orchestration itself never moves.
enum RecoveryRequest {
    /// The window `[from, to]` returned the unknown-parent orphan — mainnet reorged at/below our tip. The
    /// driver runs the recovery ladder (`sync_backtrack` → deep-fork `bulk_sync`), then rebases to the
    /// (possibly rewound) peak + 1.
    Orphan {
        from: u32,
        to: u32,
        reply: oneshot::Sender<()>,
    },
    /// `--sync-from` only: the window references out-of-span generator heights the engine lacks. The
    /// driver fetches those generators with its peer and returns them; the consumer seeds them into the
    /// engine overlay and retries the confirm — all while holding no lock.
    SeedRefs {
        heights: Vec<u32>,
        reply: oneshot::Sender<Vec<(u32, dg_xch_core::clvm::program::SerializedProgram)>>,
    },
    /// A stage walk needed a block record below the store floor (the epoch-boundary wall). The driver
    /// re-arms `resume_repair` (header backfill + cache re-warm), then rebases so the window re-stages.
    MissingRecord { reply: oneshot::Sender<()> },
    /// A transient confirm failure (a peer served a bad body, a store hiccup): the window was drained but
    /// not confirmed, so the driver rebases to the unchanged peak + 1 and the producer re-fetches it.
    Reset { reply: oneshot::Sender<()> },
}

/// The consumer→announcer post-confirm signal. SPSC, height-monotone. The three
/// peer-facing broadcasts do NOT run in the peer-free consumer (that would re-couple it to the registry);
/// the announcer runs them one step later, which is behavior-preserving because NewPeak is
/// idempotent/monotone. The non-peer wallet/mempool delta application already ran inside the consumer's
/// `finish_follow_step`, so only the `(hash, height)` the broadcasts need travels the channel.
struct ConfirmedPeak {
    hash: Bytes32,
    height: u32,
}

// The confirmed head the queue should rebase to = engine peak + 1, or the follow base when no peak yet.
async fn follow_head<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
) -> u32 {
    match node.store.get_peak().await.ok().flatten() {
        Some((_, h)) => h.saturating_add(1),
        None if node.config.sync_from > 0 => node.config.sync_from.saturating_sub(63),
        None => 0,
    }
}

// One rotation peer as a block-range source for driver-side recovery fetches.
async fn recovery_source(
    registry: &Arc<dyn OutboundPeers>,
    rotation: &mut usize,
) -> Option<Arc<dyn BlockRangeSource>> {
    let peers = registry.live_peers().await;
    if peers.is_empty() {
        return None;
    }
    let idx = *rotation % peers.len();
    *rotation = rotation.wrapping_add(1);
    Some(
        Arc::new(OutboundPeerSource::new(peers[idx].clone(), REQUEST_TIMEOUT))
            as Arc<dyn BlockRangeSource>,
    )
}

async fn fetch_seed_refs<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    source: &Arc<dyn BlockRangeSource>,
    heights: &[u32],
) -> Vec<(u32, dg_xch_core::clvm::program::SerializedProgram)> {
    let mut out = Vec::with_capacity(heights.len());
    for &h in heights {
        let cached = {
            let mut cache = node.seed_ref_cache.lock().await;
            let hit = cache.iter().position(|(height, _)| *height == h);
            hit.and_then(|i| cache.remove(i)).map(|entry| {
                let generator = entry.1.clone();
                cache.push_back(entry);
                generator
            })
        };
        let generator = match cached {
            Some(generator) => generator,
            None => {
                let Ok(fetched) = source.fetch_range(h, h).await else {
                    warn!("recovery peer failed to serve ref block height={}", h);
                    continue;
                };
                let Some(generator) = fetched
                    .into_iter()
                    .find(|b| b.height() == h)
                    .and_then(|b| b.transactions_generator)
                else {
                    warn!(
                        "recovery peer served no generator for ref block height={}",
                        h
                    );
                    continue;
                };
                info!(
                    "fetched out-of-span generator ref for the consumer height={}",
                    h
                );
                let mut cache = node.seed_ref_cache.lock().await;
                cache.push_back((h, generator.clone()));
                if cache.len() > SEED_REF_CACHE_CAP {
                    cache.pop_front();
                }
                generator
            }
        };
        out.push((h, generator));
    }
    out
}

// Rebase the queue to the current engine peak — the driver's head-invariant restore after any recovery that changed,
// or left unchanged, the peak. The generation bump wakes the fetch_scheduler to abort in-flight windows
// and replan on the (possibly rewound) branch, so no readahead handle crosses the task boundary.
async fn rebase_to_peak<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    queue: &Arc<BlockQueue>,
) {
    let head = follow_head(node).await;
    queue.rebase(head);
}

// Service one consumer recovery request with the driver's peer (the peer-mediated half of recovery). Runs the
// EXISTING sync_backtrack / bulk_sync / resume_repair unchanged; the consumer is parked holding nothing.
async fn handle_recovery<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
    queue: &Arc<BlockQueue>,
    rotation: &mut usize,
    req: RecoveryRequest,
) {
    match req {
        RecoveryRequest::SeedRefs { heights, reply } => {
            let generators = match recovery_source(registry, rotation).await {
                Some(source) => fetch_seed_refs(node, &source, &heights).await,
                None => Vec::new(),
            };
            let _ = reply.send(generators);
        }
        RecoveryRequest::Orphan { from, to, reply } => {
            warn!(
                "driver servicing orphan backtrack for the consumer from={} to={}",
                from, to
            );
            if let Some(source) = recovery_source(registry, rotation).await {
                match node.sync_backtrack(&source, from, to).await {
                    Ok(Some((_, h))) => info!("backtrack converged past the fork height={}", h),
                    Ok(None) => {}
                    Err(SyncError::DeepFork { base, floor }) => {
                        warn!(
                            "fork deeper than the backtrack cap; driving long sync base={} floor={}",
                            base, floor
                        );
                        match node.bulk_sync(registry).await {
                            Ok(Some((_, h))) => {
                                info!("long sync landed after deep fork height={}", h)
                            }
                            Ok(None) => {}
                            Err(e) => {
                                warn!("deep-fork long sync failed, retry next window error={}", e)
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "orphan backtrack failed, retry next window from={} to={} error={}",
                            from, to, e
                        )
                    }
                }
            }
            rebase_to_peak(node, queue).await;
            let _ = reply.send(());
        }
        RecoveryRequest::MissingRecord { reply } => {
            // Re-arm resume repair until it completes (bounded): floor re-measure + epoch-depth backfill
            // + cache re-warm.
            // `deepen = true`: the consumer PROVED a stage walk missed a record, so a clean floor
            // walk means the miss is below the standard backfill floor — anchor the backfill at
            // the walk's reach point instead of replying "nothing to do" forever.
            for _ in 0..8 {
                match node.resume_repair(registry, true).await {
                    Ok(true) => break,
                    Ok(false) => tokio::time::sleep(DRIVER_TICK).await,
                    Err(e) => {
                        warn!(
                            "resume repair failed during recovery, retry next window error={}",
                            e
                        );
                        break;
                    }
                }
            }
            rebase_to_peak(node, queue).await;
            let _ = reply.send(());
        }
        RecoveryRequest::Reset { reply } => {
            rebase_to_peak(node, queue).await;
            let _ = reply.send(());
        }
    }
}

// Emit a confirmed peak to the announcer WITHOUT ever blocking the confirm consumer. NewPeak is
// best-effort gossip — a newer confirmed peak supersedes an older one — and, the load-bearing reason,
// the peer-free consumer must NEVER park on the announcer: a blocking `peak_tx.send().await` on a full
// buffer back-pressures the consumer off the BlockQueue, which in turn parks the producer on a full
// buffer — a permanent sync stall. `try_send` drops the announcement iff the 256-deep buffer is full,
// and the next confirmed peak carries a fresher tip. Returns false only when the announcer is GONE
// (receiver dropped) so the consumer can exit cleanly.
fn emit_confirmed_peak(peak_tx: &mpsc::Sender<ConfirmedPeak>, peak: ConfirmedPeak) -> bool {
    match peak_tx.try_send(peak) {
        Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => true,
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

// The peer-facing peak announcer: drains the height-monotone ConfirmedPeak feed
// and runs the three broadcasts the peer-free consumer cannot. Ends when the consumer drops its sender.
async fn peak_announcer<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<Node<S>>,
    registry: Arc<dyn OutboundPeers>,
    inbound_peers: PeerMap,
    mut peak_rx: mpsc::Receiver<ConfirmedPeak>,
) {
    while let Some(ConfirmedPeak { hash, height }) = peak_rx.recv().await {
        broadcast_new_peak(&node, &registry, hash, height).await;
        update_slot_state_on_peak(&node, hash).await;
        broadcast_new_peak_timelord(&node, &inbound_peers, hash).await;
    }
}

/// The detached, peer-free block processor. Drains the landed
/// [`BlockQueue`] in strict height order and runs the FROZEN validation/confirm core
/// (`follow_step_blocks` → `follow_blocks_reporting`), never acquiring a peer, lease, or registry.
/// Everything it needs from the network arrives through the queue or is delegated to the thin driver over
/// the [`RecoveryRequest`] channel; confirmed peaks leave via the [`ConfirmedPeak`] channel to the
/// announcer. No `Chaser` lock is held across a recovery send, by construction: `follow_step_blocks`
/// scopes the `chaser.lock()` inside itself and has fully returned — guard dropped — before this loop
/// inspects the `Err` and sends. The producer never locks the `Chaser` either; it only fetches and
/// pushes to the queue.
async fn block_processor<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<Node<S>>,
    queue: Arc<BlockQueue>,
    recovery_tx: mpsc::Sender<RecoveryRequest>,
    peak_tx: mpsc::Sender<ConfirmedPeak>,
) {
    // Cross-window body pipeline: while window N runs its stage/vdf/sig/confirm phases, window
    // N+1's body precompute (pure CPU over blocks already resident in the queue) runs here in a
    // blocking task. Keyed by the window's first height so a rebase/reorg between spawn and use
    // discards it (worst case: wasted compute, never a stale verdict — the engine's flag-key
    // guard re-verifies every precompute at stage time).
    let (pipe_constants, pipe_assume_valid, pipe_metrics) = {
        let chaser = node.chaser.lock().await;
        (
            chaser.constants(),
            chaser.assume_valid(),
            chaser.metrics().clone(),
        )
    };
    let mut pre_task: Option<(
        u32,
        tokio::task::JoinHandle<
            std::collections::HashMap<u32, dg_xch_node::engine::PrecomputedBody>,
        >,
    )> = None;
    // Stage-ahead pipeline (depth 1): the previous window, staged with its vdf/sig drain running
    // on a blocking thread. Confirmed at the top of the NEXT iteration, after this iteration's
    // stage has overlapped the drain. Depth 1 is enough — the drain dominates the serial residue.
    let mut pipeline: Option<(
        dg_xch_node::sync::StagedWindow,
        tokio::task::JoinHandle<dg_xch_node::sync::WindowVerdict>,
    )> = None;
    'consumer: while node.run.load(Ordering::Relaxed) {
        // Park until the head height is present; the idle tick is only a shutdown backstop.
        tokio::select! {
            () = queue.wait_ready() => {}
            () = tokio::time::sleep(CONSUMER_IDLE) => {}
        }
        if !node.run.load(Ordering::Relaxed) {
            break;
        }
        // Mark the whole drain+seed+confirm window in flight: the /health stall dump names a wedged
        // confirm with its age, AND it is the flag the driver's peak-reconcile reads to know a confirm is
        // in progress (so it never rebases mid-window). Set BEFORE the drain so the drain→confirm gap is
        // covered; cleared on every exit path below.
        node.follow_inflight_since
            .store(unix_secs(), Ordering::Relaxed);
        let window = queue.drain_ready_window(FOLLOW_BATCH);
        if window.is_empty() && pipeline.is_none() {
            node.follow_inflight_since.store(0, Ordering::Relaxed);
            continue;
        }
        let from = window.first().map_or(0, FullBlock::height);
        let to = window.last().map_or(0, FullBlock::height);
        // --sync-from out-of-span ref pre-seed (peer-free; the driver fetches). The Chaser lock is
        // dropped before the SeedRefs send and re-taken only to apply the returned generators.
        if node.config.sync_from > 0 {
            let missing = {
                let chaser = node.chaser.lock().await;
                chaser.missing_ref_heights(&window).await
            }; // guard dropped here — no lock held across the send below
            if !missing.is_empty() {
                let (tx, rx) = oneshot::channel();
                if recovery_tx
                    .send(RecoveryRequest::SeedRefs {
                        heights: missing,
                        reply: tx,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                match rx.await {
                    Ok(generators) => {
                        let mut chaser = node.chaser.lock().await;
                        chaser.clear_seed_generators();
                        for (h, g) in generators {
                            chaser.seed_ref_generator(h, g);
                        }
                    }
                    Err(_) => break, // driver gone
                }
            }
        }
        // Join the precompute spawned while the PREVIOUS window validated; a height mismatch
        // (rebase, reorg, partial advance) discards it.
        pipe_metrics
            .window_pre_wait_micros
            .store(0, Ordering::Relaxed);
        let pre = match pre_task.take() {
            Some((h, handle)) if h == from => {
                let join_started = std::time::Instant::now();
                let joined = handle.await.ok();
                pipe_metrics
                    .window_pre_wait_micros
                    .store(join_started.elapsed().as_micros() as u64, Ordering::Relaxed);
                joined
            }
            Some((_, handle)) => {
                handle.abort();
                None
            }
            None => None,
        };
        // Spawn the NEXT window's precompute before validating this one, so the two overlap.
        // Out-of-window compression refs resolve from the confirmed store up front (final in a
        // forward sync); the ones the store cannot serve yet fall to the engine's inline path.
        let next = queue.peek_ready_window(FOLLOW_BATCH);
        if let Some(next_from) = next.first().map(FullBlock::height)
            && next
                .iter()
                .any(|b| b.is_transaction_block() && b.transactions_generator.is_some())
        {
            let extra = {
                let chaser = node.chaser.lock().await;
                chaser.confirmed_ref_generators(&next).await
            };
            let constants = pipe_constants;
            pre_task = Some((
                next_from,
                tokio::task::spawn_blocking(move || {
                    dg_xch_node::sync::precompute_window_bodies_standalone(
                        &NativePrimitives,
                        &constants,
                        pipe_assume_valid,
                        &next,
                        &extra,
                    )
                }),
            ));
        }
        // Confirm results to consume this iteration, each with the bounds of the window it
        // belongs to (the pipeline lags announcement by one window).
        let mut steps: Vec<(u32, u32, StepOutcome)> = Vec::new();
        let near_tip = { node.chaser.lock().await.near_tip() };
        if near_tip || window.is_empty() {
            // Near the tip (or on a drain-only tick) the pipeline empties first: the per-block
            // path must own the writer and the overlay alone.
            if let Some((prev, handle)) = pipeline.take() {
                let (pfrom, pto) = prev.bounds();
                let verdict = join_drain(handle, &pipe_metrics).await;
                steps.push((pfrom, pto, node.confirm_step_window(prev, verdict).await));
            }
            if !window.is_empty() && steps.iter().all(|(_, _, s)| s.is_ok()) {
                steps.push((from, to, node.follow_step_blocks_pre(&window, pre).await));
            }
        } else {
            // Stage THIS window first — it overlaps the previous window's in-flight drain —
            // then confirm the predecessor, then hand this window's drain to a blocking thread.
            let staged = node.stage_step_window(window, pre).await;
            // Spawn THIS window's drain before confirming the predecessor: the drain (pure CPU
            // on already-built queues) then also overlaps that confirm and the next iteration's
            // driver work — otherwise that residue runs with the verification cores idle.
            let mut stage_failed: Option<SyncError> = None;
            let mut spawned: Option<(
                dg_xch_node::sync::StagedWindow,
                tokio::task::JoinHandle<dg_xch_node::sync::WindowVerdict>,
            )> = None;
            match staged {
                Ok(mut staged_window) => {
                    let input = staged_window.take_drain_input();
                    let constants = pipe_constants;
                    spawned = Some((
                        staged_window,
                        tokio::task::spawn_blocking(move || {
                            dg_xch_node::sync::drain_staged_window(
                                &NativePrimitives,
                                &constants,
                                input,
                            )
                        }),
                    ));
                }
                Err(e) => stage_failed = Some(e),
            }
            if let Some((prev, handle)) = pipeline.take() {
                let (pfrom, pto) = prev.bounds();
                let verdict = join_drain(handle, &pipe_metrics).await;
                steps.push((pfrom, pto, node.confirm_step_window(prev, verdict).await));
            }
            if let Some(e) = stage_failed {
                // Stage failure leaves the overlay for us (the predecessor's confirm had to
                // land first); clear it now that it has.
                node.chaser.lock().await.clear_staged_overlay();
                steps.push((from, to, Err(e)));
            }
            if steps.iter().all(|(_, _, s)| s.is_ok()) {
                pipeline = spawned;
            } else if let Some((_, handle)) = spawned.take() {
                // A failed confirm (or stage) retracted the overlay this window staged
                // against: abort its drain and drop it — the queue reset re-fetches both
                // spans. The extra clear is idempotent and covers the confirm-failure path.
                handle.abort();
                node.chaser.lock().await.clear_staged_overlay();
            }
        }
        if pipeline.is_none() {
            node.follow_inflight_since.store(0, Ordering::Relaxed);
        }
        for (sfrom, sto, step) in steps {
            match step {
                Ok(Some((hash, height))) => {
                    // Height-monotone SPSC feed to the announcer. NON-BLOCKING: a stalled announcer must
                    // never stall the confirm consumer, which would back-pressure it off the BlockQueue
                    // and stall the whole pipeline. `emit_confirmed_peak` drops a best-effort
                    // announcement under a full buffer and only reports failure when the announcer is gone.
                    if !emit_confirmed_peak(&peak_tx, ConfirmedPeak { hash, height }) {
                        break 'consumer;
                    }
                    // The window fully advanced the peak iff the confirmed height reached the drained top;
                    // in that case `low_water == height + 1` already holds. A partial advance (a tail
                    // that staged as a side-branch candidate without outweighing) left `low_water` ahead of
                    // the peak, so realign by rebasing to `peak + 1` — the driver drops the drained-but-
                    // unconfirmed tail and the producer re-fetches it.
                    if height < sto
                        && !await_reset(&recovery_tx, |reply| RecoveryRequest::Reset { reply })
                            .await
                    {
                        break 'consumer;
                    }
                }
                // No peak advance: the whole window staged as candidates below the peak (a known-parent side
                // branch). `low_water` advanced on drain but the peak did not, so realign to `peak + 1`. The
                // engine keeps the staged candidates, so weight still accumulates toward an eventual reorg.
                Ok(None) => {
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::Reset { reply }).await {
                        break 'consumer;
                    }
                }
                Err(e) if e.is_orphan() => {
                    warn!(
                        "consumer window orphaned; delegating backtrack to the driver from={} to={} error={}",
                        sfrom, sto, e
                    );
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::Orphan {
                        from: sfrom,
                        to: sto,
                        reply,
                    })
                    .await
                    {
                        break 'consumer;
                    }
                }
                Err(e) if e.is_missing_record() => {
                    warn!(
                        "consumer needs records below the floor; delegating repair from={} to={} error={}",
                        sfrom, sto, e
                    );
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::MissingRecord {
                        reply,
                    })
                    .await
                    {
                        break 'consumer;
                    }
                }
                Err(e) => {
                    warn!(
                        "consumer follow step failed; requesting a queue reset from={} to={} error={}",
                        sfrom, sto, e
                    );
                    if !await_reset(&recovery_tx, |reply| RecoveryRequest::Reset { reply }).await {
                        break 'consumer;
                    }
                }
            }
        }
    }
    if let Some((_, handle)) = pre_task.take() {
        handle.abort();
    }
    // A staged-unconfirmed window at shutdown vanishes wholly (crash class A): abort its drain;
    // resume re-fetches from the durable peak.
    if let Some((_, handle)) = pipeline.take() {
        handle.abort();
    }
}

// One confirmed-or-failed follow step awaiting consumption, keyed by its window bounds.
type StepOutcome = Result<Option<(Bytes32, u32)>, SyncError>;

// Join a spawned window drain, recording how long the confirm actually waited on it (the
// stage-ahead pipeline's backpressure gauge). A panicked drain fails closed: nothing confirms
// and the window re-stages after the queue reset.
async fn join_drain(
    handle: tokio::task::JoinHandle<dg_xch_node::sync::WindowVerdict>,
    metrics: &std::sync::Arc<dg_xch_node::sync::SyncMetrics>,
) -> dg_xch_node::sync::WindowVerdict {
    let waited = std::time::Instant::now();
    let verdict = handle.await;
    metrics
        .window_drain_wait_micros
        .store(waited.elapsed().as_micros() as u64, Ordering::Relaxed);
    verdict.unwrap_or_else(|_| dg_xch_node::sync::WindowVerdict::failed_closed())
}

// Send a `()`-reply recovery request and park on its completion (called only after the Chaser lock
// is dropped). Returns false if the driver channel is gone (shutdown) so the consumer can exit.
async fn await_reset(
    recovery_tx: &mpsc::Sender<RecoveryRequest>,
    make: impl FnOnce(oneshot::Sender<()>) -> RecoveryRequest,
) -> bool {
    let (tx, rx) = oneshot::channel();
    if recovery_tx.send(make(tx)).await.is_err() {
        return false;
    }
    // Bounded park: a driver loop that never services this recovery must not hang the confirm
    // consumer forever, which would stop the queue draining and park the producer on a full buffer.
    // On a reply-timeout, PROCEED (retry the window next iteration) rather than exit — the driver's
    // per-tick queue reconcile and the stall watchdog restore the head invariant independently, so
    // retrying is safe.
    match tokio::time::timeout(RESET_REPLY_TIMEOUT, rx).await {
        Ok(r) => r.is_ok(),
        Err(_) => {
            warn!("recovery reply timed out; consumer proceeding (retry next window)");
            true
        }
    }
}

// The readahead/queue byte budget + fan-out config: the shipped default, or the aggressive
// large-RAM profile when `--prefetch-memory-mb`/`--prefetch-max-inflight` is set. Shared by the queue
// (its byte ceiling) and the fetch scheduler (its lookahead depth + per-peer fan-out).
fn prefetch_config_for<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
) -> dg_xch_node::sync::PrefetchConfig {
    if node.config.prefetch_memory_mb.is_some() || node.config.prefetch_max_inflight.is_some() {
        let mb = node
            .config
            .prefetch_memory_mb
            .unwrap_or(dg_xch_node::sync::READAHEAD_BYTE_BUDGET / (1024 * 1024));
        dg_xch_node::sync::PrefetchConfig::aggressive(
            mb,
            node.config.prefetch_max_inflight,
            dg_xch_node::sync::TARGET_OUTBOUND,
        )
    } else {
        dg_xch_node::sync::PrefetchConfig::default()
    }
}

async fn follow_fill_claimed<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
) -> Option<u32> {
    // The weight-gated heaviest claim (`sync_store.get_heaviest_peak` behind the new_peak weight
    // drop): `None` = caught up or nothing heavier claimed — a longer-but-lighter fork never
    // becomes the FOLLOW fill target.
    let target = node.sync_target().await?;
    let claimed = target.height;
    let peak = node.store.get_peak().await.ok().flatten();
    let local = peak.map_or(0, |(_, h)| h);
    let has_peak = peak.is_some();
    if node.config.sync_from > 0 && !has_peak && !node.sync_from_anchored().await {
        return None; // the driver's anchor_at establishes the mid-chain span first
    }
    if !node.config.genesis_sync && node.config.sync_from == 0 && wants_long_sync(local, claimed) {
        if wants_fast_sync(local, claimed) {
            return None; // the driver's from-zero weight-proof fast-sync owns the band
        }
        // Mid-chain deep gap: fill only once the driver has anchored the landing (weight proof
        // validated + fork point resolved); an unanchored fill would batch-download toward an
        // unproven heavy claim.
        if !node.long_sync_anchored().await {
            return None;
        }
    }
    if in_near_tip_band(local, claimed, has_peak) {
        return None; // the event-driven tip_follower owns the near-tip band
    }
    // Clamp the fetch frontier to what our fetch sources (outbound peers) actually advertise. The
    // weight-heaviest target can ride an inbound/unfetchable claim past the servable tip; requesting
    // past it is a beyond-tip range every peer rejects, spun every tick (the producer's `from >
    // claimed` guard then idles at the tip while the validator drains the resident backlog). No
    // outbound claim yet -> leave it unclamped, as before.
    Some(
        node.peak_book
            .outbound_tip()
            .map_or(claimed, |tip| claimed.min(tip)),
    )
}

// A frozen fetch frontier is a genuine reservation wedge only when fetchable work remains BELOW the
// servable tip (windows nothing is requesting). Frozen AT the tip (`from == claimed`, `claimed`
// already clamped to the servable outbound tip) is the benign drain-the-backlog state, not a wedge.
fn frozen_frontier_is_wedge(from: u32, claimed: u32) -> bool {
    from < claimed
}

/// The detached fetch producer. Owns the readahead engine and the peer
/// sources (rebuilt ONLY when the live set changes),
/// and keeps the [`BlockQueue`] filled to its byte budget across peers, biased to over-fill so the
/// detached consumer is never starved. It touches neither the `Chaser` nor the recovery/announce
/// paths — it only fetches and `complete`s into the queue.
///
/// Reorg coordination is lock-free through the queue generation: a [`BlockQueue::rebase`] (driven by the
/// driver on a reorg or any driver-side peak change) bumps the generation, which is BOTH the stale-
/// completion guard AND this producer's signal to abort its in-flight windows and replan on the new
/// branch — so no separate coordination channel is needed. The dispatch generation is read just before
/// the fetch and carried into `complete`, so a window whose fetch spans a concurrent rebase is dropped by
/// the guard rather than admitted onto a superseded branch.
async fn fetch_scheduler<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<Node<S>>,
    registry: Arc<dyn OutboundPeers>,
    queue: Arc<BlockQueue>,
) {
    let cfg = prefetch_config_for(&node);
    let mut readahead = dg_xch_node::sync::WindowReadahead::with_config(
        node.sync_metrics.clone(),
        REQUEST_TIMEOUT,
        cfg,
    );
    let mut rotation = 0usize;
    let mut last_gen = queue.current_gen();
    let mut peer_sources: Vec<Arc<dyn BlockRangeSource>> = Vec::new();
    let mut source_sig: Vec<(String, u16)> = Vec::new();
    // Sync-stall diagnostics: track the fetch frontier across ticks to catch the decoupled-prefetch wedge
    // (the frontier freezes while the confirm cursor / claimed tip keeps moving).
    let mut last_from = 0u32;
    let mut frozen_from_ticks = 0u32;
    while node.run.load(Ordering::Relaxed) {
        if !queue.can_admit() {
            tokio::select! {
                () = queue.wait_space() => {}
                () = tokio::time::sleep(DRIVER_TICK) => {}
            }
            continue;
        }
        // Only the FOLLOW band is ours; pause (and drop in-flight windows) when another band owns catch-up.
        let Some(claimed) = follow_fill_claimed(&node).await else {
            readahead.abort_all();
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        };
        // Read the dispatch generation BEFORE the fetch frontier (and carry it to `complete`): any rebase
        // that interleaves makes the completion a harmless guard-drop, never a wrong-branch admit. A gen
        // change since our last dispatch means a rebase superseded our windows — abort and replan.
        let dispatch_gen = queue.current_gen();
        if dispatch_gen != last_gen {
            readahead.abort_all();
            last_gen = dispatch_gen;
        }
        let from = queue.next_fetch_height();
        // Sync-stall diagnostics. A frozen fetch frontier is only a real wedge when fetchable work
        // remains BELOW the servable tip (`from < claimed`): windows exist that nothing is
        // requesting. A frontier frozen AT the tip (`from == claimed`, `claimed` already clamped to
        // the servable outbound tip) is benign — nothing higher is fetchable and the validator is
        // draining the resident backlog — so it must not raise the scary WARN.
        if from == last_from && from <= claimed {
            frozen_from_ticks += 1;
            if frozen_frontier_is_wedge(from, claimed)
                && (frozen_from_ticks == 3 || frozen_from_ticks.is_multiple_of(16))
            {
                warn!(
                    "fetch frontier frozen below the tip while work remains — decoupled prefetch reservation wedge from={} claimed={} frozen_ticks={} low_water={} generation={} resident_windows={} readahead_inflight={}",
                    from,
                    claimed,
                    frozen_from_ticks,
                    queue.low_water(),
                    queue.current_gen(),
                    queue.len(),
                    node.sync_metrics.readahead_inflight.load(Ordering::Relaxed)
                );
            } else if !frozen_frontier_is_wedge(from, claimed) && frozen_from_ticks == 3 {
                debug!(
                    "fetch frontier at the servable tip; validator draining resident backlog from={} claimed={} resident_windows={}",
                    from,
                    claimed,
                    queue.len()
                );
            }
        } else {
            last_from = from;
            frozen_from_ticks = 0;
        }
        if from > claimed {
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        }
        let to = claimed.min(from.saturating_add(FOLLOW_BATCH - 1));
        // Refresh sources only when the live peer set changed — retires the per-tick rebuild.
        let peers = registry.live_peers().await;
        if peers.is_empty() {
            tokio::time::sleep(DRIVER_TICK).await;
            continue;
        }
        let sig: Vec<(String, u16)> = peers
            .iter()
            .map(|p| (p.endpoint.0.clone(), p.endpoint.1))
            .collect();
        if sig != source_sig {
            peer_sources = peers
                .iter()
                .map(|p| {
                    Arc::new(OutboundPeerSource::new(p.clone(), REQUEST_TIMEOUT))
                        as Arc<dyn BlockRangeSource>
                })
                .collect();
            source_sig = sig;
        }
        let rotation_start = rotation % peer_sources.len();
        rotation = rotation.wrapping_add(1);
        // The direct-fetch fallback peer: rotation-ordered, preferring one WITHOUT an in-flight readahead
        // window (so two ranges never collide on one connection under per_peer==1).
        let source = (0..peer_sources.len())
            .map(|i| &peer_sources[(rotation_start + i) % peer_sources.len()])
            .find(|s| !readahead.busy_peer(s.peer_id()))
            .cloned()
            .unwrap_or_else(|| peer_sources[rotation_start % peer_sources.len()].clone());
        let prefetched = readahead.take(from, to).await;
        if to < claimed {
            readahead.fill(&peer_sources, to.saturating_add(1), claimed, FOLLOW_BATCH);
        }
        let fetched = match prefetched {
            Some(blocks) => Some(blocks),
            None => {
                let started = std::time::Instant::now();
                let direct = source.fetch_range(from, to).await;
                readahead
                    .metrics()
                    .follow_fetch_wait_micros
                    .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                match direct {
                    Ok(mut blocks) => {
                        blocks.sort_by_key(dg_xch_core::blockchain::full_block::FullBlock::height);
                        Some(blocks)
                    }
                    Err(e) => {
                        warn!(
                            "producer fetch failed, retrying next tick from={} to={} error={}",
                            from, to, e
                        );
                        None
                    }
                }
            }
        };
        if let Some(blocks) = fetched
            && !blocks.is_empty()
        {
            // FOLLOW-band download liveness: count delivered bodies into the same counter the bulk
            // download worker feeds at its write-through (sync/mod.rs download_worker). Without
            // this the follow band was a metrics blindspot — `fullnode_blocks_downloaded_total`
            // (and the /health secondary liveness witness watching it) froze while the follow
            // producer was in fact delivering blocks into the queue.
            readahead
                .metrics()
                .blocks_downloaded
                .fetch_add(blocks.len() as u64, Ordering::Relaxed);
            for block in blocks {
                queue.complete(block, dispatch_gen);
            }
        }
    }
    readahead.abort_all();
}

pub(crate) async fn sync_driver<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: Arc<Node<S>>,
    registry: Arc<dyn OutboundPeers>,
    inbound_peers: PeerMap,
) {
    // Resume repair runs once per process before the first follow step: warm the cold walk cache and
    // backfill epoch-depth records if the store floor is too shallow (the restart walls).
    let mut repaired = false;
    // Rotation cursor for the driver's recovery peer (orphan backtrack / --sync-from ref fetch).
    let mut follow_rotation = 0usize;
    // Sync-decoupling: the three components run as independent tasks communicating only through the
    // bounded reorder queue and the recovery/announce channels. The FETCH producer
    // (fetch_scheduler) fills the queue to its byte budget across peers; the CONFIRM consumer
    // (block_processor) drains it in height order and runs the frozen validation core; this driver is
    // the thin orchestrator that runs gossip + the bulk/anchor/fast-sync bands, services the peer-free
    // consumer's recovery requests, and rebases the queue to keep the head invariant. A slow confirm no longer gates
    // the next fetch and a slow fetch no longer gates the confirm — the whole point of the refactor.
    let queue = Arc::new(BlockQueue::new(
        follow_head(&node).await,
        prefetch_config_for(&node).byte_budget,
        node.sync_metrics.clone(),
    ));
    let (recovery_tx, mut recovery_rx) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
    let (peak_tx, peak_rx) = mpsc::channel::<ConfirmedPeak>(PEAK_CHANNEL_CAP);
    let consumer = tokio::spawn(block_processor(
        node.clone(),
        queue.clone(),
        recovery_tx,
        peak_tx,
    ));
    let announcer = tokio::spawn(peak_announcer(
        node.clone(),
        registry.clone(),
        inbound_peers.clone(),
        peak_rx,
    ));
    let producer = tokio::spawn(fetch_scheduler(
        node.clone(),
        registry.clone(),
        queue.clone(),
    ));
    // Stall-reclaim watchdog for the decoupled pipeline (see RECLAIM_TIMEOUT). Tracks the confirmed
    // frontier (`queue.low_water`) across driver ticks and force-rebases a stalled pipeline, so any
    // stall — a stuck announcer, a lost wakeup, a silent peer set — is bounded rather than
    // permanent, and is counted in `reservations_reclaimed`.
    let mut stall_watchdog = dg_xch_node::sync::StallWatchdog::new(
        queue.low_water(),
        std::time::Instant::now(),
        RECLAIM_TIMEOUT,
    );
    while node.run.load(Ordering::Relaxed) {
        tokio::time::sleep(DRIVER_TICK).await;
        // Service any recovery the peer-free consumer delegated (orphan backtrack / --sync-from ref
        // fetch / missing-record repair / transient reset), running the UNMOVED recovery routines with
        // the driver's peer while the consumer is parked holding nothing. Each handler rebases the
        // queue to the engine peak, restoring the head invariant; the producer picks up the generation bump and replans.
        while let Ok(req) = recovery_rx.try_recv() {
            handle_recovery(&node, &registry, &queue, &mut follow_rotation, req).await;
        }
        // Reconcile the queue head with the engine peak when the consumer is idle: a driver-side path
        // (bulk/anchor/fast-sync/infusion/tip_follower) may have advanced or rewound the peak outside the
        // consumer, so the queue must rebase to `peak + 1`. Skipped while a confirm is in flight
        // (`follow_inflight_since != 0`) — the consumer's `low_water` legitimately leads the not-yet-
        // advanced peak across its drain→confirm window, and a rebase then would drop the live window.
        // The rebase bumps the generation; the fetch_scheduler treats that as its abort-and-replan signal.
        if node.follow_inflight_since.load(Ordering::Relaxed) == 0 {
            let head = follow_head(&node).await;
            if queue.low_water() != head {
                queue.rebase(head);
            }
        }
        // Recompute the synced flag every tick: it
        // opens the tip-context gossip gates only when the confirmed chain is CURRENT (last tx
        // block within 7 minutes), and decays back to false when the tip goes stale.
        node.update_synced().await;
        // Peak-claim retraction sweep: drop the
        // claims of inbound peers that left the live map and any claim past its liveness TTL, so a
        // dead peer's phantom peak un-pins the sync bands within one tick. (Outbound claims retract
        // with their connection's ClaimGuard drop; the TTL is the backstop.)
        {
            let live: std::collections::HashSet<Bytes32> =
                inbound_peers.read().await.keys().copied().collect();
            node.peak_book.reconcile(&live);
        }
        node.sync_metrics.outbound_tip.store(
            u64::from(node.peak_book.outbound_tip().unwrap_or(0)),
            Ordering::Relaxed,
        );
        let target = node.sync_target().await;
        let claimed = target.as_ref().map_or(0, |t| t.height);
        let peak = node.store.get_peak().await.ok().flatten();
        let local = peak.map_or(0, |(_, h)| h);
        // Bulk catch-up (and startup): batch commits + a quiet checkpointer, so the full slow-disk write
        // budget goes to the confirm writer. The tip_follower flips this to near-tip mode inside the band.
        if !in_near_tip_band(local, claimed, peak.is_some()) {
            node.store.set_near_tip(false);
        }
        if !repaired {
            match node.resume_repair(&registry, false).await {
                Ok(done) => repaired = done,
                Err(e) => warn!("resume repair failed, retrying next tick error={}", e),
            }
            if !repaired {
                continue;
            }
        }
        // Stall-reclaim watchdog: the decoupled pipeline has no per-reservation reclaim, so bound ANY
        // whole-pipeline stall here. If the confirmed frontier has not advanced for RECLAIM_TIMEOUT while
        // work remains, peers are live, and no confirm is legitimately in flight, force a rebase (bumps the
        // queue generation → the fetch_scheduler aborts its readahead and replans + is woken off
        // wait_space) and count the reclaim. `claimed` is the heaviest claim (the work frontier). A confirm
        // is "legitimately in flight" only while its marker is fresh; a marker older than the reclaim bound
        // is itself a wedged confirm and must not suppress the reclaim.
        {
            let peers_live = !registry.live_peers().await.is_empty();
            let since = node.follow_inflight_since.load(Ordering::Relaxed);
            let confirm_in_flight =
                since != 0 && unix_secs().saturating_sub(since) < RECLAIM_TIMEOUT.as_secs();
            if stall_watchdog.tick(
                &queue,
                &node.sync_metrics,
                std::time::Instant::now(),
                claimed,
                peers_live,
                confirm_in_flight,
            ) {
                warn!(
                    "decoupled sync pipeline stalled; forced queue rebase (stall reclaim) low_water={} next_fetch={} peak={} generation={} readahead_inflight={} resident_windows={} claimed={} outbound_tip={:?}",
                    queue.low_water(),
                    queue.next_fetch_height(),
                    local,
                    queue.current_gen(),
                    node.sync_metrics.readahead_inflight.load(Ordering::Relaxed),
                    queue.len(),
                    claimed,
                    node.peak_book.outbound_tip()
                );
            }
        }
        // Caught up = no claim strictly heavier than our confirmed peak — a height comparison
        // would chase a longer-but-lighter fork forever.
        if target.is_none() && peak.is_some() {
            continue;
        }
        // Far-behind from a near-empty store: tip-follow (FOLLOW_BATCH/step) can never converge on a ~6.9M
        // tip. Drive the weight-proof bulk sync to the recent chain, then fall through to tip-follow.
        // `--genesis-sync` disables this entirely: the historical chain is validated block by block from 0.
        // `--sync-from H`: with no confirmed peak yet, establish the mid-chain anchor first
        // (candidates for the span below H), then fall through to the follow loop which starts
        // at the span's base instead of 0. Retries every tick until a peer serves the proof+span.
        if node.config.sync_from > 0 && peak.is_none() && !node.sync_from_anchored().await {
            match node.anchor_at(&registry, node.config.sync_from).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    warn!("sync-from anchor failed, retrying next tick error={}", e);
                    continue;
                }
            }
        }
        if !node.config.genesis_sync
            && node.config.sync_from == 0
            && wants_long_sync(local, claimed)
        {
            if wants_fast_sync(local, claimed) {
                // Near-empty-store sub-band: the recent-chain jump (unchanged from-zero landing).
                match node.bulk_sync(&registry).await {
                    Ok(Some((_, h))) => {
                        info!("fast-sync landed at recent-chain peak height={}", h);
                        // Band-exit seam: the sync_range confirm path bypassed
                        // the per-block follow side effects, so fire peak-post-processing ONCE now
                        // — mempool revalidation + NewPeak/NewPeakTimelord/NewPeakWallet.
                        node.finish_sync_transition(&registry, &inbound_peers).await;
                    }
                    // No tip/peer yet, or the proof/download failed — retry next tick.
                    Ok(None) => {}
                    Err(e) => warn!("fast-sync failed, retrying next tick error={}", e),
                }
                continue;
            }
            // Mid-chain deep gap: validate the weight proof and resolve the
            // fork point ONCE per landing — including the reorg-across-the-gap reland when the
            // fork point is below our peak. Once anchored, fall through: gossip keeps running
            // while the detached fetch/confirm pipeline batch-syncs the gap from the fork point;
            // the per-tick queue reconcile above rebases onto a relanded peak automatically.
            match node.ensure_long_sync_anchor(&registry).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    warn!("long-sync anchor failed, retrying next tick error={}", e);
                    continue;
                }
            }
        }
        // Refresh the RequestPeers gossip answer from the live outbound set: what we can vouch
        // for is exactly who we are connected to right now.
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let snapshot: Vec<TimestampedPeerInfo> = registry
                .live_peers()
                .await
                .iter()
                .map(|p| TimestampedPeerInfo {
                    host: p.endpoint.0.clone(),
                    port: p.endpoint.1,
                    timestamp: now,
                })
                .collect();
            *node.known_peers.write().await = snapshot;
        }
        // Re-gossip freshly-admitted transactions: everything the
        // mempool accepted since last tick — from peer gossip or local push_tx — goes out as
        // NewTransaction to the peers we hold. Expire in-flight fetch guards by AGE:
        // a request older than REQUEST_TIMEOUT with no body is dropped,
        // so the id is re-requestable and cannot pin the pending map. A blanket clear here would
        // wipe a just-issued request and make its legitimate response look unsolicited.
        node.tx_requested
            .lock()
            .await
            .retain(|_, t| t.at.elapsed() < REQUEST_TIMEOUT);
        broadcast_transactions(&node, &registry).await;
        // Validate received slot gossip into the state machine, then relay what was
        // accepted (driver-side so validation always has record ancestry + next-SSI context).
        process_sp_inbox(&node).await;
        broadcast_sp_announcements(&node, &registry).await;
        // Push the farmer-form signage points to inbound farmer peers (the node→farmer
        // half of the farmer interface; the outbound relay above is full-node gossip only).
        broadcast_farmer_signage_points(&node, &inbound_peers).await;
        broadcast_ub_timelord_announcements(&node, &inbound_peers).await;
        // Pre-validate received unfinished blocks and relay the accepted ones.
        process_ub_inbox(&node).await;
        broadcast_ub_announcements(&node, &registry).await;
        process_ip_inbox(&node, &registry, &inbound_peers).await;
        // Compact-VDF consume: validate + swap pulled compact proofs, re-gossip accepted ones.
        process_compact_vdf_inbox(&node).await;
        broadcast_compact_vdf_announcements(&node, &registry).await;
        // The block-follow producer/consumer is fully detached: the fetch_scheduler keeps the queue
        // filled (FOLLOW band) and the block_processor drains + confirms it, both on their own tasks.
        // The near-tip band stays with the event-driven tip_follower and the far-behind band with the
        // bulk/anchor/fast-sync arms above; this loop is now pure orchestration + gossip.
    }
    // Drain-on-shutdown: the run flag is already clear, so the sub-tasks are winding down on their own
    // idle backstops; abort makes the teardown prompt and leaks no task past the driver.
    consumer.abort();
    announcer.abort();
    producer.abort();
}

// Ancestor window sized for get_next_sub_slot_iters_and_difficulty — and the SES machinery that
// shares its walks — anchored at `anchor` (the UB's parent, or the peak), depth per
// difficulty_record_depth (513..=896 mid-epoch, 5,121..=5,503 across an epoch turn and its first
// sub-epoch). Served from the node's in-memory record window (record_window.rs) with store
// fallback per miss — never a per-call DB walk, which would re-fetch the whole window from the
// backend on every peak (a throughput trough on the Postgres legs).
async fn difficulty_records_map<S: BlockStore + Send + Sync>(
    node: &Node<S>,
    anchor: &BlockRecord,
) -> HashMap<Bytes32, BlockRecord> {
    crate::record_window::windowed_records_map(
        &node.record_window,
        node.store.as_ref(),
        &node.sync_metrics,
        &node.constants,
        anchor,
    )
    .await
}

// `blockchain.get_sp_and_ip_sub_slots`: the EOS bundles ending the slots the peak's signage
// point and infusion sit in. Walks records back to the last first-in-sub-slot block and reads its
// stored finished sub-slots; the overflow case additionally needs the previous slot's bundle.
async fn sp_and_ip_sub_slots<S: BlockStore + Send + Sync>(
    store: &S,
    peak_hash: Bytes32,
) -> Option<(Option<EndOfSubSlotBundle>, Option<EndOfSubSlotBundle>)> {
    let peak = store.get_block_record(&peak_hash).await.ok().flatten()?;
    let is_overflow = peak.overflow;
    let mut curr_br = peak;
    while !curr_br.first_in_sub_slot() && curr_br.height > 0 {
        curr_br = store
            .get_block_record(&curr_br.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    let curr = store.get_block(&curr_br.header_hash).await.ok().flatten()?;
    if curr.finished_sub_slots.is_empty() {
        // Reached genesis with no sub-slots yet.
        return Some((None, None));
    }
    let ip_sub_slot = curr.finished_sub_slots.last()?.clone();
    if !is_overflow {
        // The PoS sub-slot is the infusion sub-slot.
        return Some((None, Some(ip_sub_slot)));
    }
    if curr.finished_sub_slots.len() > 1 {
        let sp = curr.finished_sub_slots[curr.finished_sub_slots.len() - 2].clone();
        return Some((Some(sp), Some(ip_sub_slot)));
    }
    // Overflow with a single finished slot: the SP slot ended at the PREVIOUS first-in-sub-slot
    // block's last bundle.
    let mut prev_br = match store.get_block_record(&curr.prev_header_hash()).await {
        Ok(Some(rec)) => rec,
        _ => return Some((None, Some(ip_sub_slot))), // curr is genesis
    };
    while prev_br.height > 0 && !prev_br.first_in_sub_slot() {
        prev_br = store
            .get_block_record(&prev_br.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    let prev_curr = store.get_block(&prev_br.header_hash).await.ok().flatten()?;
    match prev_curr.finished_sub_slots.last() {
        Some(bundle) => Some((Some(bundle.clone()), Some(ip_sub_slot))),
        None => Some((None, Some(ip_sub_slot))),
    }
}

// The relay announcement for an accepted signage point. Only
// index > 0 SPs reach this path (they are appended by `new_signage_point`, which rejects index 0), so
// the VDFs are present; `None` guards a malformed all-None SP and simply skips the announce.
fn announce_for_sp(
    state: &SlotState,
    index: u8,
    sp: &SignagePoint,
) -> Option<NewSignagePointOrEndOfSubSlot> {
    let cc_vdf = sp.cc_vdf.as_ref()?;
    let rc_vdf = sp.rc_vdf.as_ref()?;
    let prev_challenge = state.get_sub_slot(&cc_vdf.challenge).map(|(eos, _, _)| {
        eos.challenge_chain
            .challenge_chain_end_of_slot_vdf
            .challenge
    });
    Some(NewSignagePointOrEndOfSubSlot {
        prev_challenge_hash: prev_challenge,
        challenge_hash: cc_vdf.challenge,
        index_from_challenge: index,
        last_rc_infusion: rc_vdf.challenge,
    })
}

// The farmer-form signage point for an accepted SP (farmer_protocol.NewSignagePoint).
// The challenge hash is the SP's sub-slot cc challenge, same as the gossip
// announce; difficulty/SSI come from the accept site's next-SSI context so a farmer can size the
// plot filter. `None` only if the SP's VDF outputs fail to hash.
fn farmer_announce_for_sp(
    sp: &SignagePoint,
    index: u8,
    difficulty: u64,
    sub_slot_iters: u64,
    peak_height: u32,
    last_tx_height: u32,
) -> Option<NewSignagePoint> {
    new_signage_point_for_farmers(
        sp,
        sp.cc_vdf.as_ref()?.challenge,
        difficulty,
        sub_slot_iters,
        index,
        peak_height,
        last_tx_height,
    )
}

// (peak_height, last_transaction_block_height) for the farmer signage-point context: a tx-block
// peak is its own last-tx height; otherwise the peak carries the previous one. No peak (pre-genesis)
// is (0, 0).
fn farmer_heights(peak: Option<&BlockRecord>) -> (u32, u32) {
    match peak {
        Some(rec) if rec.is_transaction_block() => (rec.height, rec.height),
        Some(rec) => (rec.height, rec.prev_transaction_block_height),
        None => (0, 0),
    }
}

// The relay announcement for an accepted end-of-sub-slot (index 0 by protocol convention).
fn announce_for_eos(eos: &EndOfSubSlotBundle) -> Option<NewSignagePointOrEndOfSubSlot> {
    Some(NewSignagePointOrEndOfSubSlot {
        prev_challenge_hash: Some(
            eos.challenge_chain
                .challenge_chain_end_of_slot_vdf
                .challenge,
        ),
        challenge_hash: eos.challenge_chain.hash().ok()?,
        index_from_challenge: 0,
        last_rc_infusion: eos.reward_chain.end_of_slot_vdf.challenge,
    })
}

// The farmer-form index-0 signage point for a newly-finished sub-slot.
// A sub-slot start has no cc/rc SP VDF, so sp_source_data carries the challenge/reward SUB-SLOTS
// (sub_slot_data), not vdf_data; the SP hashes ARE the sub-slot hashes. The farmer counterpart of
// announce_for_eos (which serves the full-node NewSignagePointOrEndOfSubSlot). None only on hash fail.
fn farmer_announce_for_eos(
    eos: &EndOfSubSlotBundle,
    difficulty: u64,
    sub_slot_iters: u64,
    peak_height: u32,
    last_tx_height: u32,
) -> Option<NewSignagePoint> {
    let cc_hash = eos.challenge_chain.hash().ok()?;
    let rc_hash = eos.reward_chain.hash().ok()?;
    Some(NewSignagePoint {
        challenge_hash: cc_hash,
        challenge_chain_sp: cc_hash,
        reward_chain_sp: rc_hash,
        difficulty,
        sub_slot_iters,
        signage_point_index: 0,
        peak_height,
        last_tx_height,
        sp_source_data: Some(SignagePointSourceData {
            sub_slot_data: Some(SPSubSlotSourceData {
                cc_sub_slot: eos.challenge_chain,
                rc_sub_slot: eos.reward_chain,
            }),
            vdf_data: None,
        }),
    })
}

// Reset the slot state around a just-confirmed peak and queue relay announcements for anything
// the future caches released.
async fn update_slot_state_on_peak<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    peak_hash: Bytes32,
) {
    let Ok(Some(rec)) = node.store.get_block_record(&peak_hash).await else {
        return;
    };
    let Some((sp_ss, ip_ss)) = sp_and_ip_sub_slots(node.store.as_ref(), peak_hash).await else {
        return;
    };
    let blocks = difficulty_records_map(node, &rec).await;
    let (next_ssi, next_diff) = match get_next_sub_slot_iters_and_difficulty(
        &node.constants,
        true,
        Some(&rec),
        &blocks,
    ) {
        Ok(v) => v,
        Err(e) => {
            // This computation cannot fail for a connected peak; a failure here means the
            // record walk broke mid-chain. Skipping this peak's slot-state reset is strictly
            // safer than a fallback,
            // which fed difficulty 0 into the slot state and the farmer announcements.
            warn!(
                "slot-state peak update skipped: next SSI/difficulty computation failed event={} peak={} error={}",
                "slot_state.ssi_difficulty_fail", peak_hash, e
            );
            return;
        }
    };
    let mut state = node.slot_state.lock().await;
    let (new_eos, new_sps) = state.new_peak(
        &rec,
        PeakSlotContext {
            sp_sub_slot: sp_ss.as_ref(),
            ip_sub_slot: ip_ss.as_ref(),
            fork_block: None,
        },
        &blocks,
        next_ssi,
        next_diff,
        false,
    );
    // The advancing peak also obsoletes cached unfinished blocks at or below it.
    node.unfinished.lock().await.prune_below(rec.height);
    let (peak_height, last_tx_height) = farmer_heights(Some(&rec));
    let mut announces = node.sp_announce.lock().await;
    let mut farmer_announces = node.sp_farmer_announce.lock().await;
    if let Some(eos) = new_eos.as_ref() {
        if let Some(a) = announce_for_eos(eos) {
            announces.push(a);
        }
        if let Some(fa) =
            farmer_announce_for_eos(eos, next_diff, next_ssi, peak_height, last_tx_height)
        {
            farmer_announces.push(fa);
        }
    }
    for (index, sp) in &new_sps {
        if let Some(a) = announce_for_sp(&state, *index, sp) {
            announces.push(a);
        }
        if let Some(fa) =
            farmer_announce_for_sp(sp, *index, next_diff, next_ssi, peak_height, last_tx_height)
        {
            farmer_announces.push(fa);
        }
    }
}

// Validate every received slot-gossip payload into the state machine and queue relays for what
// was accepted — the driver half of respond_signage_point / respond_end_of_sub_slot.
async fn process_sp_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(node: &Arc<Node<S>>) {
    let events: Vec<SpEvent> = node.sp_inbox.lock().await.drain(..).collect();
    if events.is_empty() {
        return;
    }
    let peak = match node.store.get_peak().await {
        Ok(Some((hash, _))) => node.store.get_block_record(&hash).await.ok().flatten(),
        _ => None,
    };
    let blocks = match &peak {
        Some(rec) => difficulty_records_map(node, rec).await,
        None => HashMap::new(),
    };
    // A None peak yields the starting SSI/difficulty (never an Err). A connected peak cannot fail
    // either — so an Err here means the record walk broke mid-chain: drop the drained batch
    // (SP gossip is redundant across peers and ticks) rather than process it under difficulty 0,
    // a poisoning fallback.
    let (next_ssi, next_diff) = match get_next_sub_slot_iters_and_difficulty(
        &node.constants,
        true,
        peak.as_ref(),
        &blocks,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "signage-point batch dropped: next SSI/difficulty computation failed event={} error={}",
                "sp.ssi_difficulty_fail", e
            );
            return;
        }
    };
    let (peak_height, last_tx_height) = farmer_heights(peak.as_ref());
    let mut state = node.slot_state.lock().await;
    let mut announces = node.sp_announce.lock().await;
    let mut farmer_announces = node.sp_farmer_announce.lock().await;
    for event in events {
        match event {
            SpEvent::SignagePoint(sp) => {
                // A pulled RespondSignagePoint always carries real VDFs (index > 0); wrap them in the
                // now-optional SignagePoint fields (the stored SP form).
                let point = SignagePoint {
                    cc_vdf: Some(sp.challenge_chain_vdf),
                    cc_proof: Some(sp.challenge_chain_proof.clone()),
                    rc_vdf: Some(sp.reward_chain_vdf),
                    rc_proof: Some(sp.reward_chain_proof.clone()),
                };
                if state.new_signage_point(
                    sp.index_from_challenge,
                    &blocks,
                    peak.as_ref(),
                    next_ssi,
                    &point,
                    false,
                ) {
                    node.sp_current_index
                        .store(u32::from(sp.index_from_challenge), Ordering::Relaxed);
                    node.signage_points_total
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let (Ok(cc), Ok(rc)) = (
                        sp.challenge_chain_vdf.output.hash(),
                        sp.reward_chain_vdf.output.hash(),
                    ) {
                        info!(
                            "finished signage point index={} cc={} rc={}",
                            sp.index_from_challenge, cc, rc
                        );
                    }
                    if let Some(a) = announce_for_sp(&state, sp.index_from_challenge, &point) {
                        announces.push(a);
                    }
                    if let Some(fa) = farmer_announce_for_sp(
                        &point,
                        sp.index_from_challenge,
                        next_diff,
                        next_ssi,
                        peak_height,
                        last_tx_height,
                    ) {
                        farmer_announces.push(fa);
                    }
                }
            }
            SpEvent::EndOfSubSlot(eos) => {
                if state
                    .new_finished_sub_slot(
                        &eos.end_of_slot_bundle,
                        &blocks,
                        peak.as_ref(),
                        next_ssi,
                        next_diff,
                        false,
                    )
                    .is_some()
                {
                    // The "finished sub slot" INFO line, keyed by the challenge-chain hash.
                    if let Ok(cc) = eos.end_of_slot_bundle.challenge_chain.hash() {
                        info!("finished sub slot cc={}", cc);
                    }
                    if let Some(a) = announce_for_eos(&eos.end_of_slot_bundle) {
                        announces.push(a);
                    }
                    if let Some(fa) = farmer_announce_for_eos(
                        &eos.end_of_slot_bundle,
                        next_diff,
                        next_ssi,
                        peak_height,
                        last_tx_height,
                    ) {
                        farmer_announces.push(fa);
                    }
                }
            }
        }
    }
}

// The gossip-transaction validator worker: drains the
// bounded inbox OFF the websocket read loop, runs the bundle→conditions CLVM + aggregate-
// signature checks at next-block height, admits, and queues the re-gossip announcement.
#[allow(clippy::too_many_arguments)] // the worker's seams are the node's shared Arcs, one each
fn spawn_tx_validator<S: BlockStore + CoinStore + Send + Sync + 'static>(
    store: Arc<S>,
    mempool: Arc<Mutex<Mempool>>,
    constants: ConsensusConstants,
    tx_inbox: Arc<Mutex<TxQueue>>,
    tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    synced: Arc<AtomicBool>,
    run: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        while run.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            // NO_TRANSACTIONS_WHILE_SYNCING —
            // bundles that raced the synced-flag transition into the inbox are dropped without
            // running CLVM. The handler-side gates keep the inbox empty in steady not-synced
            // state; this covers the transition window.
            if !synced.load(Ordering::Relaxed) {
                tx_inbox.lock().await.clear();
                continue;
            }
            // Drain both lanes, high-priority (trusted) first.
            let batch: Vec<(Bytes32, SpendBundle)> = tx_inbox.lock().await.drain_batch();
            if batch.is_empty() {
                continue;
            }
            for (_peer, tx) in batch {
                // The origin was recorded at receipt (`on_respond_transaction` → `note_tx_origin`)
                // with the remote host, so the announce drain can exclude an outbound origin too;
                // the worker only validates + admits here.
                // The shared admission seam (tx_admission.rs, `full_node.add_transaction`):
                // CLVM + aggregate-signature validation at next-block height, `Mempool::admit`,
                // and the NewTransaction announce queued iff newly resident — identical to the
                // push_tx and p2p SendTransaction ingress paths.
                if let Err(e) = crate::tx_admission::admit_spend_bundle(
                    store.as_ref(),
                    &mempool,
                    &constants,
                    &tx_announce,
                    tx,
                )
                .await
                {
                    debug!("gossiped transaction rejected error={}", e);
                }
            }
        }
    });
}

// The weight-proof serving worker (the daemon arm of request_proof_of_weight): drains the bounded RequestProofOfWeight inbox OFF the websocket read
// loop, builds each requested proof through the crate's WeightProofServer — whose internal lock +
// tip-keyed cache is the single-flight — and responds to the requesting peer with the request
// id. Refusals send nothing: unknown tip and tip below WEIGHT_PROOF_RECENT_BLOCKS are logged and
// dropped.
fn spawn_wp_worker<S: BlockStore + Send + Sync + 'static>(
    store: Arc<S>,
    constants: ConsensusConstants,
    wp_inbox: Arc<Mutex<Vec<WpRequest>>>,
    net: Arc<NetCounters>,
    run: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let server = dg_xch_weight_proof::serve::WeightProofServer::new(store, constants);
        while run.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let batch: Vec<WpRequest> = wp_inbox.lock().await.drain(..).collect();
            for req in batch {
                match server.get_proof_of_weight(req.tip).await {
                    Ok(wp) => respond_weight_proof(&req, &wp, &net).await,
                    Err(e) if e.is_refusal() => {
                        info!("refusing weight proof request tip={} error={}", req.tip, e);
                    }
                    Err(e) => warn!("weight proof build failed tip={} error={}", req.tip, e),
                }
            }
        }
    });
}

// Compact-VDF solicitation scan cadence + window when `--uncompact` is on (off by default).
// The window is a bounded slice of confirmed blocks ending 5 below the peak (a block within 5 of
// the peak is never compactified).
const UNCOMPACT_INTERVAL: Duration = Duration::from_secs(300);
const UNCOMPACT_WINDOW: u32 = 256;
// The broadcast list chunks into `target_uncompact_proofs`-sized chunks (100) round-robined one
// chunk per connected timelord so blueboxes share the work rather than each grinding the whole
// list. Our fixed window rarely fills a chunk.
const UNCOMPACT_TARGET_PROOFS: usize = 100;
// Re-solicit suppression (see SolicitLedger): do not re-send a field's request for one hour (12
// scan ticks) — long enough that a connected bluebox is not spammed with duplicates while it grinds,
// short enough that a field left bulky (timelord gone / dropped the request) is retried, not
// abandoned. Capacity caps memory at a few window-fulls of distinct fields.
const UNCOMPACT_RESOLICIT_TTL: Duration = Duration::from_secs(3600);
const UNCOMPACT_LEDGER_CAP: usize = 8192;

// A minimal async seam over a solicitation target so the timelord-solicit fan-out is unit-testable
// with a recording mock: a live `SocketPeer` wraps a websocket sink that cannot be forged offline
// (WebsocketMsgStream has only TCP/TLS + hyper-upgrade variants), so the send path is proven against
// a mock here and against a real bluebox live. The production impl is `Arc<SocketPeer>`.
#[async_trait]
trait SolicitTarget: Send + Sync {
    async fn is_timelord(&self) -> bool;
    async fn negotiated_version(&self) -> ChiaProtocolVersion;
    async fn deliver(&self, msg: dg_xch_core::protocols::ChiaMessage) -> Result<(), Error>;
}

#[async_trait]
impl SolicitTarget for Arc<SocketPeer> {
    async fn is_timelord(&self) -> bool {
        *self.node_type.read().await == NodeType::Timelord
    }
    async fn negotiated_version(&self) -> ChiaProtocolVersion {
        *self.protocol_version.read().await
    }
    async fn deliver(&self, msg: dg_xch_core::protocols::ChiaMessage) -> Result<(), Error> {
        self.send(msg).await
    }
}

// SOLICIT: hand `reqs` to connected bluebox TIMELORDS.
// Filters `peers` to timelords, chunks the list into UNCOMPACT_TARGET_PROOFS-sized chunks, and
// round-robins one chunk per timelord (each bluebox gets a different slice),
// sending each field as a RequestCompactProofOfTime. Returns the number of request messages sent.
// An empty timelord set (the network-infused case: we run without a bluebox, the network compacts
// for us) sends nothing and returns 0.
async fn solicit_uncompact_from_timelords<T: SolicitTarget>(
    reqs: &[RequestCompactProofOfTime],
    peers: &[T],
    net: &NetCounters,
) -> usize {
    if reqs.is_empty() {
        return 0;
    }
    // Snapshot the timelord targets (async node_type read done once, not per chunk).
    let mut timelords: Vec<&T> = Vec::new();
    for p in peers {
        if p.is_timelord().await {
            timelords.push(p);
        }
    }
    if timelords.is_empty() {
        return 0;
    }
    let chunks: Vec<&[RequestCompactProofOfTime]> = reqs.chunks(UNCOMPACT_TARGET_PROOFS).collect();
    let mut sent = 0usize;
    for (i, peer) in timelords.into_iter().enumerate() {
        let chunk = chunks[i % chunks.len()];
        let version = peer.negotiated_version().await;
        for req in chunk {
            let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
                dg_xch_core::protocols::ProtocolMessageTypes::RequestCompactProofOfTime,
                version,
                req,
                None,
            ) else {
                continue;
            };
            net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::RequestCompactProofOfTime,
                msg.data.as_slice().len(),
            );
            if peer.deliver(msg).await.is_ok() {
                sent += 1;
            }
        }
    }
    sent
}

// SOLICITATION. Flag-gated OFF by default.
// Scans a bounded recent window of confirmed blocks for still-bulky VDF proofs and SENDS a
// RequestCompactProofOfTime to every connected bluebox TIMELORD (NodeType::Timelord) so it computes
// + returns the compact proof (RespondCompactProofOfTime → on_respond_compact_proof_of_time → the
// same consume/validate/replace/re-gossip path as full-node compact-VDF gossip). Re-solicit
// suppression (SolicitLedger) keeps a fixed-window scan from re-requesting the same field every
// tick. On a network-infused node with no timelord peer the scan runs and sends nothing (no target)
// — the code path exists and fires the moment a bluebox connects.
fn spawn_uncompact_scanner<S: BlockStore + Send + Sync + 'static>(
    store: Arc<S>,
    inbound_peers: PeerMap,
    net: Arc<NetCounters>,
    run: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut ledger = dg_xch_node::compact_vdf::SolicitLedger::new(
            UNCOMPACT_LEDGER_CAP,
            UNCOMPACT_RESOLICIT_TTL,
        );
        while run.load(Ordering::Relaxed) {
            tokio::time::sleep(UNCOMPACT_INTERVAL).await;
            let Ok(Some((_, peak_height))) = store.get_peak().await else {
                continue;
            };
            let top = peak_height.saturating_sub(5);
            let bottom = top.saturating_sub(UNCOMPACT_WINDOW);
            let now = std::time::Instant::now();
            let mut reqs: Vec<RequestCompactProofOfTime> = Vec::new();
            for h in bottom..=top {
                let Ok(Some(rec)) = store.get_block_record_by_height(h).await else {
                    continue;
                };
                let Ok(Some(block)) = store.get_block(&rec.header_hash).await else {
                    continue;
                };
                reqs.extend(dg_xch_node::compact_vdf::plan_block_solicitations(
                    &block,
                    rec.header_hash,
                    h,
                    &mut ledger,
                    now,
                ));
            }
            // Snapshot the inbound peers (cheap Arc clones) so the send holds no map lock; the
            // timelord filter runs inside solicit_uncompact_from_timelords.
            let peers: Vec<Arc<SocketPeer>> =
                inbound_peers.read().await.values().cloned().collect();
            let sent = solicit_uncompact_from_timelords(&reqs, &peers, &net).await;
            info!(
                "uncompact scan: bulky VDF proofs solicited from bluebox timelords bottom={} top={} solicited={} sent={} ledger={}",
                bottom,
                top,
                reqs.len(),
                sent,
                ledger.len()
            );
        }
    });
}

// Send one built proof back to its requester over the link map the request arrived on, encoded at
// the peer's negotiated protocol version and carrying the request id (the requester's oneshot on
// RespondProofOfWeight matches by type + id). A peer that disconnected while we built is dropped.
async fn respond_weight_proof(req: &WpRequest, wp: &WeightProof, net: &NetCounters) {
    let Some(peer) = req.peers.read().await.get(&req.peer).cloned() else {
        debug!(
            "weight proof requester disconnected before response peer={}",
            req.peer
        );
        return;
    };
    let version = *peer.protocol_version.read().await;
    let resp = RespondProofOfWeight {
        wp: wp.clone(),
        tip: req.tip,
    };
    let msg = match dg_xch_core::protocols::ChiaMessage::new(
        dg_xch_core::protocols::ProtocolMessageTypes::RespondProofOfWeight,
        version,
        &resp,
        req.id,
    ) {
        Ok(msg) => msg,
        Err(e) => {
            warn!("failed to serialize RespondProofOfWeight error={}", e);
            return;
        }
    };
    net.count_out(
        dg_xch_core::protocols::ProtocolMessageTypes::RespondProofOfWeight,
        msg.data.as_slice().len(),
    );
    if let Err(e) = peer.send(msg).await {
        warn!(
            "failed to send RespondProofOfWeight peer={} error={}",
            req.peer, e
        );
    }
}

async fn process_ub_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(node: &Arc<Node<S>>) {
    let blocks: Vec<UnfinishedBlock> = node.ub_inbox.lock().await.drain(..).collect();
    if blocks.is_empty() {
        return;
    }
    for block in blocks {
        let Ok(partial_hash) = block.reward_chain_block.hash() else {
            // Was a silent continue: a ready UB dies here with nothing logged.
            node.producer.candidate_dropped("ub_reward_hash_fail");
            warn!(
                "unfinished block dropped: reward_chain_block failed to hash event={} reason={}",
                "producer.ub.dropped", "ub_reward_hash_fail"
            );
            continue;
        };
        let foliage_hash = block.foliage.foliage_transaction_block_hash;
        // Capture the parent hash before the match — the store-error arm moves `block` back onto the
        // inbox, so it can no longer be read through `block` after that point.
        let prev_hash = block.foliage.prev_block_hash;
        // Three OUTCOMES, not two. The old `let Ok(Some(prev)) = .. else { drop }` collapsed a store
        // ERROR into the same "we are behind" placeholder-drop as a genuine miss — and that drop also
        // `remove_requesting`s, forfeiting the re-fetch. For an OWN-farmed winning candidate whose prev
        // IS the current committed peak (read from THIS store during declare→assemble), a transient
        // backend hiccup on this one lookup would silently lose the block. A candidate must never be lost
        // to a DB read: add_unfinished_block resolves prev against the in-memory Blockchain, not the DB.
        //
        // Note the store never lags the validated chain here (engine commits a record BEFORE inserting
        // it into its in-memory cache, and block records are never deleted — reorg only flips
        // in_main_chain, which get_block_record-by-hash ignores). So an `Ok(None)` really is "we do not
        // have this parent yet" (we are behind), and an OWN candidate's committed-peak parent can only
        // fail to resolve via an `Err` — which we now retry instead of dropping.
        let prev = match node.store.get_block_record(&prev_hash).await {
            Ok(Some(prev)) => prev,
            Ok(None) => {
                // Parent genuinely absent — we have not validated it yet. Park the placeholder and drop
                // the pending request so a re-announce after the peak catches up re-fetches it (a UB
                // whose prev is not in the chain cannot validate). This is the expected
                // steady-state outcome while syncing (the bulk of this counter).
                node.producer.candidate_dropped("ub_prev_unknown");
                info!(
                    "unfinished block parked: parent block not in store (we are behind) event={} reason={} partial={} prev={}",
                    "producer.ub.dropped", "ub_prev_unknown", partial_hash, prev_hash
                );
                node.unfinished
                    .lock()
                    .await
                    .remove_requesting(&partial_hash, foliage_hash.as_ref());
                continue;
            }
            Err(e) => {
                // A STORE ERROR is NOT "we are behind": the parent may well be present. Re-queue for the
                // next drain (bounded by SP_INBOX_CAP) and DO NOT remove_requesting, so the lookup retries
                // against a recovered backend and the candidate — possibly our own winning block — survives.
                let mut inbox = node.ub_inbox.lock().await;
                if inbox.len() < SP_INBOX_CAP {
                    inbox.push(block);
                    drop(inbox);
                    node.producer.candidate_requeued("ub_prev_store_error");
                    warn!(
                        "unfinished block re-queued: store error resolving parent (retryable, candidate preserved) event={} reason={} partial={} prev={} error={}",
                        "producer.ub.requeued", "ub_prev_store_error", partial_hash, prev_hash, e
                    );
                } else {
                    drop(inbox);
                    // Only with the inbox saturated ON TOP of the store error is the candidate lost.
                    node.producer.candidate_dropped("ub_inbox_full");
                    warn!(
                        "unfinished block dropped: store error resolving parent AND ub_inbox at cap event={} reason={} partial={} error={}",
                        "producer.ub.dropped", "ub_inbox_full", partial_hash, e
                    );
                }
                continue;
            }
        };
        // The dedup ladder, AFTER the disconnected-parent check (a disconnected block is not
        // seen-marked, so a parked "we are behind" block is
        // re-processable once we catch up) and BEFORE any validation: the seen set keyed on the
        // EXACT unfinished block hash (many foliages can share one trunk — the seen set is the
        // DoS bound), then the per-(reward, foliage) cache
        // check. Together they bound a burst of duplicate announces to ONE header validation and
        // ONE generator run.
        match block.hash() {
            Ok(ub_hash) => {
                if node.unfinished.lock().await.seen(ub_hash) {
                    node.producer.candidate_dropped("ub_duplicate");
                    debug!(
                        "unfinished block dropped: exact duplicate already processed event={} reason={} partial={}",
                        "producer.ub.dropped", "ub_duplicate", partial_hash
                    );
                    continue;
                }
            }
            Err(e) => {
                node.producer.candidate_dropped("ub_hash_fail");
                warn!(
                    "unfinished block dropped: unfinished block failed to hash event={} reason={} partial={} error={}",
                    "producer.ub.dropped", "ub_hash_fail", partial_hash, e
                );
                continue;
            }
        }
        {
            // get_unfinished_block2: already held at this (reward, foliage), or a BETTER
            // (smaller-foliage) variant held — ignore. Placeholder (requested-not-received)
            // entries do not count as held.
            let cache = node.unfinished.lock().await;
            let (existing, _, has_better) = cache.get_block2(&partial_hash, foliage_hash.as_ref());
            if existing.is_some() || has_better {
                drop(cache);
                node.producer.candidate_dropped("ub_already_cached");
                debug!(
                    "unfinished block dropped: already cached (or a better variant is) event={} reason={} partial={}",
                    "producer.ub.dropped", "ub_already_cached", partial_hash
                );
                continue;
            }
        }
        let records = difficulty_records_map(node, &prev).await;
        let is_first_in_sub_slot = !block.finished_sub_slots.is_empty();
        // With the window sized by difficulty_record_depth this cannot fail for a parent whose
        // ancestry is in the store. A failure means the record walk broke mid-chain — a real
        // invariant break worth the WARN.
        let (ssi, difficulty) = match get_next_sub_slot_iters_and_difficulty(
            &node.constants,
            is_first_in_sub_slot,
            Some(&prev),
            &records,
        ) {
            Ok(v) => v,
            Err(e) => {
                node.producer.candidate_dropped("ub_ssi_difficulty_fail");
                warn!(
                    "unfinished block dropped: next SSI/difficulty computation failed event={} reason={} partial={} error={}",
                    "producer.ub.dropped", "ub_ssi_difficulty_fail", partial_hash, e
                );
                continue;
            }
        };
        let header = UnfinishedHeaderBlock {
            finished_sub_slots: block.finished_sub_slots.clone(),
            reward_chain_block: block.reward_chain_block.clone(),
            challenge_chain_sp_proof: block.challenge_chain_sp_proof.clone(),
            reward_chain_sp_proof: block.reward_chain_sp_proof.clone(),
            foliage: block.foliage,
            foliage_transaction_block: block.foliage_transaction_block,
            transactions_filter: dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::default(),
        };
        match validate_unfinished_header_block(
            &node.constants,
            &dg_xch_node::header::PrimitiveVerifier(&dg_xch_node::NativePrimitives),
            &records,
            &header,
            ValidationState { ssi, difficulty },
            true,
        ) {
            Ok(required_iters) => {
                // The transactions generator RUNS — and the cost and
                // aggregate-signature rules hold — BEFORE the block may enter the served cache or
                // the relay queue. Peers ban the sender of an invalid unfinished block; a node that
                // relayed without running the generator serves the poisoned block to honest
                // peers and eats that ban itself. Own-farmed candidates take this same path.
                //
                // Ban-posture delta: peers ban the SENDER of the invalid unfinished block for
                // 600s. Our p2p layer has no timed ban list, and the
                // RespondUnfinishedBlock inbox does not carry the sender's peer id — the
                // enforceable action today is the drop + no-relay below, which closes the harm
                // vector (nothing invalid is served or announced). Sender punishment lands with
                // the p2p ban list (same posture as the tx path, p2p/src/handlers.rs
                // TransactionAnnounceAction::Ban).
                if let Err((reason, e)) =
                    validate_ub_body(node, &block, prev.height.saturating_add(1), &prev).await
                {
                    node.producer.candidate_dropped(reason);
                    info!(
                        "unfinished block dropped: transactions generator/body validation failed event={} reason={} partial={} error={}",
                        "producer.ub.dropped", reason, partial_hash, e
                    );
                    node.unfinished
                        .lock()
                        .await
                        .remove_requesting(&partial_hash, foliage_hash.as_ref());
                    continue;
                }
                // The "added unfinished block" INFO line.
                info!(
                    "added unfinished block event={} partial={}",
                    "producer.ub.added", partial_hash
                );
                // Build the NewUnfinishedBlockTimelord BEFORE the block is moved into the cache.
                // sub_slot_iters/difficulty are the same context the header validation used; ses
                // is the summary the NEXT block would include (None on a near-genesis chain);
                // rc_prev is the last reward-chain infusion before this SP (the index-0 vs
                // index>0 split).
                let timelord_request = {
                    let ses = next_sub_epoch_summary(
                        &node.constants,
                        &records,
                        required_iters,
                        &block,
                        true,
                    )
                    .unwrap_or(None);
                    let rcb = &block.reward_chain_block;
                    // Resolve the pos sub-slot's reward-chain hash under the slot lock (index-0 path only),
                    // then let the pure helper apply the index-0/index>0 rc_prev split.
                    let pos_sub_slot_rc_hash = if rcb.signage_point_index == 0 {
                        let slot = node.slot_state.lock().await;
                        slot.get_sub_slot(&rcb.pos_ss_cc_challenge_hash)
                            .and_then(|(eos, _, _)| eos.reward_chain.hash().ok())
                    } else {
                        None
                    };
                    let rc_prev = dg_xch_node::farmer::timelord_rc_prev(
                        node.constants.genesis_challenge,
                        rcb.signage_point_index,
                        rcb.pos_ss_cc_challenge_hash,
                        rcb.reward_chain_sp_vdf.as_ref(),
                        pos_sub_slot_rc_hash,
                    );
                    rc_prev.map(|rc_prev| NewUnfinishedBlockTimelord {
                        reward_chain_block: block.reward_chain_block.clone(),
                        difficulty,
                        sub_slot_iters: ssi,
                        foliage: block.foliage,
                        sub_epoch_summary: ses,
                        rc_prev,
                    })
                };
                node.unfinished.lock().await.add_block(
                    partial_hash,
                    prev.height.saturating_add(1),
                    block,
                    required_iters,
                );
                node.ub_announce.lock().await.push(NewUnfinishedBlock2 {
                    unfinished_reward_hash: partial_hash,
                    foliage_hash,
                });
                match timelord_request {
                    Some(req) => node.ub_timelord_announce.lock().await.push(req),
                    None => {
                        node.producer
                            .candidate_dropped("timelord_rc_prev_unresolved");
                        warn!(
                            "timelord broadcast: could not resolve rc_prev; skipping NewUnfinishedBlockTimelord event={} reason={} partial={}",
                            "producer.ub.dropped", "timelord_rc_prev_unresolved", partial_hash
                        );
                    }
                }
            }
            // Promoted debug!->info!: a validation failure of a block we may have
            // farmed is a wall the operator must see (read %e for the specific consensus reason).
            Err(e) => {
                node.producer.candidate_dropped("ub_validation_fail");
                info!(
                    "unfinished block failed pre-validation event={} reason={} partial={} error={}",
                    "producer.ub.dropped", "ub_validation_fail", partial_hash, e
                );
                node.unfinished
                    .lock()
                    .await
                    .remove_requesting(&partial_hash, foliage_hash.as_ref());
            }
        }
    }
}

async fn validate_ub_body<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    block: &UnfinishedBlock,
    height: u32,
    prev: &BlockRecord,
) -> Result<(), (&'static str, NodeError)> {
    let mut refs: Vec<GeneratorReference> =
        Vec::with_capacity(block.transactions_generator_ref_list.len());
    for (index, &ref_height) in block.transactions_generator_ref_list.iter().enumerate() {
        // Live only for pre-SF9-regime chains — past SF9 the pure validator below bans a
        // non-empty ref list before the refs are even read.
        let generator = match node.store.get_generator_at_height(ref_height).await {
            Ok(Some(g)) => g,
            Ok(None) => {
                return Err((
                    "ub_generator_ref_missing",
                    ChiaError::GeneratorRefHasNoGenerator.into(),
                ));
            }
            Err(e) => return Err(("ub_generator_ref_missing", e.into())),
        };
        refs.push(GeneratorReference {
            height: ref_height,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            generator,
        });
    }
    // The SF9 body rules key on the previous TRANSACTION block's height (the CLVM flag ladder
    // keys on the block's own). Two regimes, two keys, as in the engine.
    let prev_tx_height = if prev.is_transaction_block() {
        prev.height
    } else {
        prev.prev_transaction_block_height
    };
    validate_unfinished_block_body(
        &NativePrimitives,
        &node.constants,
        block,
        &refs,
        height,
        prev_tx_height,
    )
    .map(|_| ())
    .map_err(classify_ub_body_error)
}

// Fold a body-validation failure into its producer-metrics drop reason: the cost rules
// (`ub_cost_mismatch`), the structural bindings (`ub_body_fail`), and everything the generator
// RUN itself surfaces — deserialize failure, CLVM raise, bad aggregate signature — under
// `ub_generator_fail` (the GENERATOR_RUNTIME_ERROR family, the live-ban vector).
fn classify_ub_body_error(e: NodeError) -> (&'static str, NodeError) {
    let reason = match &e {
        NodeError::Consensus(
            ChiaError::InvalidBlockCost
            | ChiaError::BlockCostExceedsMax
            | ChiaError::InvalidCostResult,
        ) => "ub_cost_mismatch",
        NodeError::Consensus(
            ChiaError::InvalidTransactionsGeneratorHash
            | ChiaError::InvalidTransactionsInfoHash
            | ChiaError::TooManyGeneratorRefs
            | ChiaError::FutureGeneratorRefs
            | ChiaError::GeneratorRefHasNoGenerator
            | ChiaError::ComplexGeneratorReceived
            | ChiaError::TooManySpends,
        ) => "ub_body_fail",
        _ => "ub_generator_fail",
    };
    (reason, e)
}

/// Assemble the infused `FullBlock` for one `NewInfusionPointVDF` — the assembly half of
/// `new_infusion_point_vdf`, split out so it is unit-testable
/// against a seeded unfinished cache + `SlotState` without the block store's full validation engine.
/// Returns `None` on any bail (unknown unfinished block, prev block not reachable, disconnected
/// finished sub-slots, missing pos sub-slot, an iters failure, or an invalid pool signature) — the
/// caller then drops the infusion point (the timelord re-sends on the next `NewPeakTimelord`).
async fn assemble_infusion_block<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    req: &NewInfusionPointVDF,
) -> Option<FullBlock> {
    // 1. the unfinished block this infusion point finishes.
    let unfinished = node
        .unfinished
        .lock()
        .await
        .get_block(&req.unfinished_reward_hash)
        .cloned();
    let Some(unfinished) = unfinished else {
        warn!(
            "infusion point: no cached unfinished reward block, cannot finish unfinished_reward_hash={}",
            req.unfinished_reward_hash
        );
        return None;
    };

    // 2. backtrack the rc challenge through empty sub-slots, then find prev_b.
    let last_slot_cc_hash = req.challenge_chain_ip_vdf.challenge;
    let target_rc_hash = node
        .slot_state
        .lock()
        .await
        .backtrack_rc_challenge(req.reward_chain_ip_vdf.challenge);
    let prev_b: Option<BlockRecord> = if target_rc_hash == node.constants.genesis_challenge {
        None
    } else {
        let Ok(Some((peak_hash, _))) = node.store.get_peak().await else {
            debug!(
                "infusion point: no peak to backtrack prev block from target_rc_hash={}",
                target_rc_hash
            );
            return None;
        };
        let Ok(Some(peak_rec)) = node.store.get_block_record(&peak_hash).await else {
            return None;
        };
        match backtrack_prev_block(node.store.as_ref(), peak_rec, target_rc_hash).await {
            Some(pb) => pb,
            None => {
                // add_to_future_ip + return: the prev block is not reachable yet. We do
                // not model the future-ip cache; the timelord re-sends on the next NewPeakTimelord.
                warn!(
                    "infusion point: previous block not found (parked; timelord will re-send) target_rc_hash={} infusion={}",
                    target_rc_hash, req.reward_chain_ip_vdf.challenge
                );
                return None;
            }
        }
    };

    // 3. the finished sub-slots from challenge_in_chain to last_slot_cc_hash.
    let challenge_in_chain = match &prev_b {
        None => node.constants.genesis_challenge,
        Some(pb) => match challenge_in_chain(node.store.as_ref(), pb).await {
            Some(c) => c,
            None => {
                debug!("infusion point: challenge_in_chain walk hit a store gap");
                return None;
            }
        },
    };
    let finished_sub_slots = node
        .slot_state
        .lock()
        .await
        .get_finished_sub_slots(challenge_in_chain, last_slot_cc_hash);
    let Some(finished_sub_slots) = finished_sub_slots else {
        debug!("infusion point: finished sub-slots not connected");
        return None;
    };

    // 4. next SSI/difficulty, then SP total-iters from the pos sub-slot start.
    let records = match &prev_b {
        Some(pb) => difficulty_records_map(node, pb).await,
        None => HashMap::new(),
    };
    let (sub_slot_iters, difficulty) = match get_next_sub_slot_iters_and_difficulty(
        &node.constants,
        !finished_sub_slots.is_empty(),
        prev_b.as_ref(),
        &records,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "infusion point: next SSI/difficulty computation failed error={}",
                e
            );
            return None;
        }
    };
    let pos_ss_cc = unfinished.reward_chain_block.pos_ss_cc_challenge_hash;
    let sub_slot_start_iters: u128 = if pos_ss_cc == node.constants.genesis_challenge {
        0
    } else {
        match node.slot_state.lock().await.get_sub_slot(&pos_ss_cc) {
            Some((_, _, start_iters)) => start_iters,
            None => {
                warn!(
                    "infusion point: do not have pos sub-slot, cannot finish pos_ss_cc={}",
                    pos_ss_cc
                );
                return None;
            }
        }
    };
    let sp_iters = match dg_xch_core::consensus::pot_iterations::calculate_sp_iters(
        &node.constants,
        sub_slot_iters,
        unfinished.reward_chain_block.signage_point_index,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!("infusion point: sp_iters computation failed error={}", e);
            return None;
        }
    };
    let sp_total_iters = sub_slot_start_iters + u128::from(sp_iters);

    // get_prev_transaction_block's first return, computed against the store (core holds none).
    let is_transaction_block = match &prev_b {
        None => true,
        Some(pb) => {
            match resolve_prev_linkage(node.store.as_ref(), &node.constants, pb, sp_total_iters)
                .await
            {
                Some(linkage) => linkage.is_transaction_block,
                None => {
                    debug!("infusion point: prev-linkage walk hit a store gap");
                    return None;
                }
            }
        }
    };

    // 5. assemble the FullBlock from the unfinished block + infusion-point VDFs.
    let block = match unfinished_block_to_full_block(
        &unfinished,
        req.challenge_chain_ip_vdf,
        req.challenge_chain_ip_proof.clone(),
        req.reward_chain_ip_vdf,
        req.reward_chain_ip_proof.clone(),
        req.infused_challenge_chain_ip_vdf,
        req.infused_challenge_chain_ip_proof.clone(),
        finished_sub_slots,
        prev_b.as_ref(),
        is_transaction_block,
        difficulty,
    ) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "infusion point: FullBlock assembly failed to hash reward block error={:?}",
                e
            );
            return None;
        }
    };
    // — refuse a pre-farm block whose height is not 0 (invalid pool signature).
    if !has_valid_pool_sig(&node.constants, &block) {
        warn!("infusion point: block has an invalid pool signature; dropping");
        return None;
    }
    Some(block)
}

async fn process_ip_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
    inbound_peers: &PeerMap,
) {
    let reqs: Vec<NewInfusionPointVDF> = node.ip_inbox.lock().await.drain(..).collect();
    for req in reqs {
        // Steps 1-5: look up + assemble + pool-signature check. `None` = any bail.
        let Some(block) = assemble_infusion_block(node, &req).await else {
            continue;
        };

        // — add_block: validate, persist, set peak (raise_on_disconnected). We route
        // through the same follow path a peer's block takes; it fires the S8 farmed-header match (this
        // block IS one we farmed) and returns the new peak.
        let height = block.reward_chain_block.height;
        let partial = req.unfinished_reward_hash;
        match node.follow_step_blocks(std::slice::from_ref(&block)).await {
            Ok(Some((hash, new_height))) => {
                info!(
                    "infused our unfinished block into a FullBlock and set it as the new peak event={} height={} header={} partial={}",
                    "producer.infusion.peak", new_height, hash, partial
                );
                broadcast_new_peak(node, registry, hash, new_height).await;
                update_slot_state_on_peak(node, hash).await;
                broadcast_new_peak_timelord(node, inbound_peers, hash).await;
            }
            Ok(None) => {
                // Validated but did not become the peak (a competing/heavier chain already leads, or we
                // already hold it); no NewPeak in that case.
                info!(
                    "infusion point: assembled block confirmed but did not advance the peak height={} partial={}",
                    height, partial
                );
            }
            Err(e) => {
                // Consensus error validating the block; log and move on (the driver's per-tick
                // NewPeakTimelord broadcast covers the timelord resync).
                warn!(
                    "infusion point: assembled block failed consensus validation error={} height={} partial={}",
                    e, height, partial
                );
            }
        }
    }
}

// Which unfinished-block announce a peer at `version` must receive. The broadcast branches on
// the negotiated protocol version, NOT on a Capability enum variant: newer clients get
// NewUnfinishedBlock2 and older clients get NewUnfinishedBlock, split at version 0.0.35.
// True = send v2.
#[must_use]
fn announce_v2_for(version: ChiaProtocolVersion) -> bool {
    version > ChiaProtocolVersion::Chia0_0_35
}

// The negotiated protocol version an outbound peer reported in its handshake reply (captured by
// WsClient::perform_handshake); default when we somehow hold no handshake for it.
fn outbound_peer_version(peer: &OutboundPeer) -> ChiaProtocolVersion {
    peer.client
        .handshake
        .as_ref()
        .map(|h| {
            ChiaProtocolVersion::from_str(&h.protocol_version)
                .expect("ChiaProtocolVersion::from_str is Infallible")
        })
        .unwrap_or_default()
}

// Drain the unfinished-block relay queue, branching per peer on the negotiated protocol version:
// NewUnfinishedBlock2 to peers > 0.0.35, NewUnfinishedBlock (reward hash only) to older peers —
// (a validated partial relays onward so the timelord's input propagates ahead of infusion).
// Each message is encoded at that peer's own version.
async fn broadcast_ub_announcements<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announces: Vec<NewUnfinishedBlock2> = node.ub_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    let peers = registry.live_peers().await;
    if peers.is_empty() {
        info!(
            "unfinished block(s) validated but no full-node peer to announce to event={} pending={}",
            "producer.ub.no_full_node_peer",
            announces.len()
        );
        return;
    }
    for ann in announces {
        let v1 = NewUnfinishedBlock {
            unfinished_reward_hash: ann.unfinished_reward_hash,
        };
        for peer in &peers {
            let version = outbound_peer_version(peer);
            let (msg_type, msg) = if announce_v2_for(version) {
                (
                    dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock2,
                    dg_xch_core::protocols::ChiaMessage::new(
                        dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock2,
                        version,
                        &ann,
                        None,
                    ),
                )
            } else {
                (
                    dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock,
                    dg_xch_core::protocols::ChiaMessage::new(
                        dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock,
                        version,
                        &v1,
                        None,
                    ),
                )
            };
            let Ok(msg) = msg else { continue };
            node.net.count_out(msg_type, msg.data.as_slice().len());
            let _ = peer.client.send(msg).await;
        }
        // S7 — one broadcast (to all full-node peers) per validated partial.
        node.producer.ub_broadcast("full_node");
        info!(
            "unfinished block announced to full-node peers event={} partial={} peer_type={} peers={}",
            "producer.ub.broadcast",
            ann.unfinished_reward_hash,
            "full_node",
            peers.len()
        );
    }
}

// CONSUME driver pass (`full_node.add_compact_vdf`): validate each pulled compact proof off the
// read path, swap it into the stored block, and queue a NewCompactVDF re-gossip. The block re-write
// reuses the store's INSERT-OR-REPLACE body write-through under the SAME header hash (only a witness
// changes — the block identity is unchanged), so no store surface is added.
//
// NOTE — the accept-and-replace happy path is exercised only by a genuine normalized-to-identity
// proof from a live bluebox timelord; it cannot be forged offline. The guard/reject branches are
// unit-proven (full-node/tests/compact_vdf.rs); this end-to-end acceptance is gated live.
async fn process_compact_vdf_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
) {
    let queued: Vec<RespondCompactVDF> = node.compact_vdf_inbox.lock().await.drain(..).collect();
    if queued.is_empty() {
        return;
    }
    let Ok(Some((_, peak_height))) = node.store.get_peak().await else {
        return;
    };
    for resp in queued {
        let Ok(Some(block)) = node.store.get_block(&resp.header_hash).await else {
            continue;
        };
        if block.header_hash().ok() != Some(resp.header_hash) {
            continue;
        }
        if !dg_xch_node::compact_vdf::can_accept_compact_proof(
            &node.constants,
            &block,
            resp.field_vdf,
            &resp.vdf_info,
            &resp.vdf_proof,
            peak_height,
            resp.height,
        ) {
            debug!(
                "rejected compact vdf proof height={} field={}",
                resp.height, resp.field_vdf
            );
            continue;
        }
        let Some(new_block) = dg_xch_node::compact_vdf::replace_proof(
            &block,
            resp.field_vdf,
            &resp.vdf_info,
            &resp.vdf_proof,
        ) else {
            continue;
        };
        // Defense-in-depth: swapping a VDF *proof* (witness) must not change the block's identity —
        // the header hash commits to VdfInfo/foliage, not the proofs. `new_block` is stored under
        // `resp.header_hash`, so this guard rejects a replacement that altered a committed field
        // rather than writing content that mis-hashes its key.
        if new_block.header_hash().ok() != Some(resp.header_hash) {
            warn!(
                "compact vdf replace changed the header hash — refusing store re-write height={}",
                resp.height
            );
            continue;
        }
        // Re-write the body under the same header hash (INSERT OR REPLACE). One block, one commit.
        let rewrite = async {
            let mut batch = node.store.begin().await?;
            node.store.append_many(&mut batch, &[new_block]).await?;
            node.store.commit(batch).await
        };
        if let Err(e) = rewrite.await {
            warn!(
                "compact vdf block re-write failed height={} error={}",
                resp.height, e
            );
            continue;
        }
        info!(
            "replaced compact vdf proof height={} field={}",
            resp.height, resp.field_vdf
        );
        node.compact_vdf_announce.lock().await.push(NewCompactVDF {
            height: resp.height,
            header_hash: resp.header_hash,
            field_vdf: resp.field_vdf,
            vdf_info: resp.vdf_info,
        });
    }
}

// Drain the compact-VDF re-gossip queue as NewCompactVDF broadcasts to every live outbound peer
// (the origin peer would normally be excluded — our broadcast helpers fan out to all outbound
// peers and rely on the peers' own request-dedup, harmless).
async fn broadcast_compact_vdf_announcements<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announces: Vec<NewCompactVDF> = node.compact_vdf_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    let version = ChiaProtocolVersion::default();
    let peers = registry.live_peers().await;
    for ann in announces {
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewCompactVdf,
            version,
            &ann,
            None,
        ) else {
            continue;
        };
        for peer in &peers {
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewCompactVdf,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg.clone()).await;
        }
    }
}

// Drain the slot-gossip announce queue to every live outbound peer (same shape as the
// transaction re-gossip).
async fn broadcast_sp_announcements<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announces: Vec<NewSignagePointOrEndOfSubSlot> =
        node.sp_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    let version = ChiaProtocolVersion::default();
    let peers = registry.live_peers().await;
    for ann in announces {
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
            version,
            &ann,
            None,
        ) else {
            continue;
        };
        for peer in &peers {
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg.clone()).await;
        }
    }
}

// Snapshot the inbound peers that handshook as wallets (cheap Arc clones — the send loop holds no
// map lock), the NodeType::Wallet analog of the farmer/timelord snapshots below.
async fn wallet_peers(inbound_peers: &PeerMap) -> Vec<Arc<SocketPeer>> {
    let mut wallets = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Wallet {
            wallets.push(peer.clone());
        }
    }
    wallets
}

// Push a confirmed peak to the wallet peers as NewPeakWallet. Fire-and-forget:
// a wallet that misses one re-anchors on the next peak (and can always re-page via
// RequestPuzzleState).
async fn broadcast_new_peak_wallet(
    net: &NetCounters,
    wallets: &[Arc<SocketPeer>],
    announce: &NewPeakWallet,
) {
    for peer in wallets {
        let version = *peer.protocol_version.read().await;
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakWallet,
            version,
            announce,
            None,
        ) else {
            continue;
        };
        net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakWallet,
            msg.data.as_slice().len(),
        );
        let _ = peer.send(msg).await;
    }
}

// Push queued farmer-form signage points to inbound peers that handshook as farmers
// (farmer_protocol.NewSignagePoint). Farmers connect INBOUND to our peer
// server, so this walks the inbound PeerMap under a NodeType::Farmer filter — distinct from the
// outbound full-node gossip relay in broadcast_sp_announcements. Fire-and-forget: a farmer that
// misses one gets the next signage point (there are 64 per slot).
async fn broadcast_farmer_signage_points<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    inbound_peers: &PeerMap,
) {
    let announces: Vec<NewSignagePoint> = node.sp_farmer_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    // Snapshot the inbound farmer peers (cheap Arc clones) so the send loop holds no map lock.
    let mut farmers: Vec<Arc<SocketPeer>> = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Farmer {
            farmers.push(peer.clone());
        }
    }
    if farmers.is_empty() {
        return;
    }
    for ann in &announces {
        for peer in &farmers {
            let version = *peer.protocol_version.read().await;
            let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
                dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePoint,
                version,
                ann,
                None,
            ) else {
                continue;
            };
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePoint,
                msg.data.as_slice().len(),
            );
            let _ = peer.send(msg).await;
        }
    }
}

// Push queued NewUnfinishedBlockTimelord messages to inbound peers that handshook as
// timelords (`full_node.add_unfinished_block` → `send_to_all([timelord_msg], NodeType.TIMELORD)`).
// Timelords connect INBOUND to our peer server, so this walks the inbound PeerMap under a
// NodeType::Timelord filter — the timelord counterpart of broadcast_farmer_signage_points. Without
// this, a farmed UnfinishedBlock never reaches a timelord and the block never completes into a
// FullBlock. Fire-and-forget: a timelord that misses one gets the next; the partial is also relayed
// to full nodes via broadcast_ub_announcements (origin exclusion is moot — we originate here).
async fn broadcast_ub_timelord_announcements<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    inbound_peers: &PeerMap,
) {
    let announces: Vec<NewUnfinishedBlockTimelord> =
        node.ub_timelord_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    // Snapshot the inbound timelord peers (cheap Arc clones) so the send loop holds no map lock.
    let mut timelords: Vec<Arc<SocketPeer>> = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Timelord {
            timelords.push(peer.clone());
        }
    }
    if timelords.is_empty() {
        // THE expected first-block wall: a UB is ready to infuse but no timelord is connected, so it
        // never completes into a FullBlock. Count each
        // stranded UB so the /metrics funnel names this stage exactly — the METRIC fires per UB.
        for _ in &announces {
            node.producer.candidate_dropped("no_timelord_peer");
        }
        // The LOG is debounced: on a node that intentionally runs without a timelord peer (the
        // network infuses for it) this state is EXPECTED and fired 284×/window as WARN — log noise
        // that buried real warnings. One INFO per NO_TIMELORD_LOG_SECS carries the stranded count
        // since the last line; the per-UB evidence lives in the counter, queryable any time.
        static LAST_LOG_UNIX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static STRANDED_SINCE_LOG: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        const NO_TIMELORD_LOG_SECS: u64 = 600;
        STRANDED_SINCE_LOG.fetch_add(announces.len() as u64, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let last = LAST_LOG_UNIX.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= NO_TIMELORD_LOG_SECS
            && LAST_LOG_UNIX
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let n = STRANDED_SINCE_LOG.swap(0, Ordering::Relaxed);
            info!(
                "unfinished blocks ready but no timelord peer connected (expected on a \
                 network-infused node; see fullnode_producer_candidates_dropped_total) event={} stranded_since_last_log={}",
                "producer.ub.no_timelord_peer", n
            );
        }
        return;
    }
    for ann in &announces {
        for peer in &timelords {
            let version = *peer.protocol_version.read().await;
            let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
                dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlockTimelord,
                version,
                ann,
                None,
            ) else {
                continue;
            };
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlockTimelord,
                msg.data.as_slice().len(),
            );
            let _ = peer.send(msg).await;
        }
        // S7t — one broadcast (to all timelord peers) per ready partial.
        node.producer.ub_broadcast("timelord");
        info!(
            "unfinished block announced to timelord peers event={} partial={:?} peer_type={} timelords={}",
            "producer.ub.broadcast",
            ann.reward_chain_block.hash().ok(),
            "timelord",
            timelords.len()
        );
    }
}

fn get_recent_reward_challenges(
    constants: &ConsensusConstants,
    peak: &BlockRecord,
    records: &HashMap<Bytes32, BlockRecord>,
) -> Option<Vec<(Bytes32, u128)>> {
    let limit = 2usize * constants.max_sub_slot_blocks as usize;
    let mut recent_rc: Vec<(Bytes32, u128)> = Vec::new();
    let mut curr = peak;
    while recent_rc.len() < limit {
        if curr.header_hash != peak.header_hash {
            recent_rc.push((curr.reward_infusion_new_challenge, curr.total_iters));
        }
        if curr.first_in_sub_slot() {
            // finished_reward_slot_hashes is Some for a first-in-sub-slot record.
            let hashes = curr.finished_reward_slot_hashes.as_ref()?;
            let mut sub_slot_total_iters = curr.ip_sub_slot_total_iters(constants).ok()?;
            for rc in hashes.iter().rev() {
                if sub_slot_total_iters < u128::from(curr.sub_slot_iters) {
                    break;
                }
                recent_rc.push((*rc, sub_slot_total_iters));
                sub_slot_total_iters -= u128::from(curr.sub_slot_iters);
            }
        }
        if curr.height == 0 {
            break;
        }
        curr = records.get(&curr.prev_hash)?;
    }
    recent_rc.reverse();
    Some(recent_rc)
}

// send_peak_to_timelords. On every new peak the full node hands
// its timelords a NewPeakTimelord so they can begin infusing on top of it — the peak counterpart of
// broadcast_ub_timelord_announcements (timelords connect INBOUND, so this walks the inbound PeerMap
// under a NodeType::Timelord filter). Every field is fully derived: the in-slot
// difficulty, the peak record's deficit/sub_slot_iters, the next sub-epoch summary, the recent reward
// challenges, the last challenge-block-or-EOS total iters, and the passed-ses-height flag. Bails (sends
// nothing) if any derivation cannot be grounded in the loaded record window rather than shipping an
// approximate message. Fire-and-forget: a timelord that misses one gets the next peak.
async fn broadcast_new_peak_timelord<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    inbound_peers: &PeerMap,
    peak_hash: Bytes32,
) {
    // Snapshot inbound timelord peers first (cheap Arc clones); nothing to do without one.
    let mut timelords: Vec<Arc<SocketPeer>> = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Timelord {
            timelords.push(peer.clone());
        }
    }
    if timelords.is_empty() {
        return;
    }
    let Some(new_peak) = build_new_peak_timelord(
        node.store.as_ref(),
        &node.constants,
        &node.record_window,
        &node.sync_metrics,
        peak_hash,
    )
    .await
    else {
        return;
    };
    send_new_peak_timelord(node, &timelords, &new_peak).await;
}

// The construction half of `send_peak_to_timelords` — shared by the peak broadcast above and the
// on-connect TIMELORD greeting ([`FullNodeApi::timelord_peak`]).
async fn build_new_peak_timelord<S: BlockStore + Send + Sync>(
    store: &S,
    constants: &ConsensusConstants,
    record_window: &Mutex<BlockRecordCache>,
    sync_metrics: &SyncMetrics,
    peak_hash: Bytes32,
) -> Option<Box<NewPeakTimelord>> {
    let peak_block = store.get_block(&peak_hash).await.ok().flatten()?;
    let peak = store.get_block_record(&peak_hash).await.ok().flatten()?;
    // Depth must cover the deepest of the walks here: the difficulty computation's
    // can_finish_sub_and_full_epoch scan (up to 383 back mid-epoch, ~5120 at an epoch turn — a
    // fixed 512 fails both late-sub-epoch and epoch-turn peaks), passes_ses
    // (< sub_epoch_blocks = 384) and get_recent_reward_challenges (< 2*max_sub_slot_blocks = 256).
    // difficulty_record_depth's floor of 513 dominates the latter two.
    let records = crate::record_window::windowed_records_map(
        record_window,
        store,
        sync_metrics,
        constants,
        &peak,
    )
    .await;

    // difficulty: get_next_sub_slot_iters_and_difficulty(peak, False)[1], including the
    // height<=2 short-circuit to the starting difficulty.
    let difficulty = if peak.height <= 2 {
        constants.difficulty_starting
    } else {
        let Ok((_ssi, diff)) =
            get_next_sub_slot_iters_and_difficulty(constants, false, Some(&peak), &records)
        else {
            return None;
        };
        diff
    };

    // sub_epoch_summary: next_sub_epoch_summary takes an UnfinishedBlock; reconstruct one from
    // the peak FullBlock — it reads only signage_point_index, the prev-block hash, finished_sub_slots
    // and total_iters, all preserved by RewardChainBlock::get_unfinished and the shared foliage.
    let unfinished_peak = UnfinishedBlock {
        finished_sub_slots: peak_block.finished_sub_slots.clone(),
        reward_chain_block: peak_block.reward_chain_block.get_unfinished(),
        challenge_chain_sp_proof: peak_block.challenge_chain_sp_proof.clone(),
        reward_chain_sp_proof: peak_block.reward_chain_sp_proof.clone(),
        foliage: peak_block.foliage,
        foliage_transaction_block: peak_block.foliage_transaction_block,
        transactions_info: peak_block.transactions_info.clone(),
        transactions_generator: peak_block.transactions_generator.clone(),
        transactions_generator_ref_list: peak_block.transactions_generator_ref_list.clone(),
    };
    let sub_epoch_summary = next_sub_epoch_summary(
        constants,
        &records,
        peak.required_iters,
        &unfinished_peak,
        true,
    )
    .unwrap_or(None);

    // previous_reward_challenges: `blockchain.get_recent_reward_challenges`().
    let previous_reward_challenges = get_recent_reward_challenges(constants, &peak, &records)?;

    // last_challenge_sb_or_eos_total_iters: walk back to the last
    // challenge-block or first-in-sub-slot record; take its total_iters (challenge block) or the total
    // iters at the start of its infusion sub-slot (end-of-sub-slot case).
    let mut curr = &peak;
    while !curr.is_challenge_block(constants.min_blocks_per_challenge_block)
        && !curr.first_in_sub_slot()
    {
        curr = records.get(&curr.prev_hash)?;
    }
    let last_challenge_sb_or_eos_total_iters =
        if curr.is_challenge_block(constants.min_blocks_per_challenge_block) {
            curr.total_iters
        } else {
            curr.ip_sub_slot_total_iters(constants).ok()?
        };

    // passes_ses_height_but_not_yet_included: true unless a sub-epoch summary
    // was already included at or after the last sub-epoch-block height boundary.
    let mut curr = &peak;
    let mut passes_ses_height_but_not_yet_included = true;
    while curr.height % constants.sub_epoch_blocks != 0 {
        if curr.sub_epoch_summary_included.is_some() {
            passes_ses_height_but_not_yet_included = false;
        }
        curr = records.get(&curr.prev_hash)?;
    }
    if curr.sub_epoch_summary_included.is_some() || curr.height == 0 {
        passes_ses_height_but_not_yet_included = false;
    }

    Some(Box::new(NewPeakTimelord {
        reward_chain_block: peak_block.reward_chain_block.clone(),
        difficulty,
        deficit: peak.deficit,
        sub_slot_iters: peak.sub_slot_iters,
        sub_epoch_summary,
        previous_reward_challenges,
        last_challenge_sb_or_eos_total_iters,
        passes_ses_height_but_not_yet_included,
    }))
}

// The delivery half: NewPeakTimelord to every snapshotted timelord peer. Fire-and-forget: a
// timelord that misses one gets the next peak.
async fn send_new_peak_timelord<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    timelords: &[Arc<SocketPeer>],
    new_peak: &NewPeakTimelord,
) {
    for peer in timelords {
        let version = *peer.protocol_version.read().await;
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakTimelord,
            version,
            new_peak,
            None,
        ) else {
            continue;
        };
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakTimelord,
            msg.data.as_slice().len(),
        );
        let _ = peer.send(msg).await;
    }
    info!(
        "new peak announced to timelord peers event={} height={} timelords={}",
        "producer.peak.timelord_broadcast",
        new_peak.reward_chain_block.height,
        timelords.len()
    );
}

// Drain the tx-announce queue into NewTransaction broadcasts to EVERY connected full-node peer —
// outbound AND inbound — excluding each transaction's origin peer.
// Fire-and-forget like the peak announcement: a peer that misses one can still pull the bundle
// after any other node re-announces it.
//
// Origin-exclusion id space: inbound peers are keyed by their client-cert hash, so an
// inbound-sourced transaction never echoes to its origin. OUTBOUND connections all share one
// local dispatch id (OUR client-cert hash — clients/src/websocket/mod.rs:205), so an
// outbound-sourced transaction may still echo to its origin: a benign redundancy (the origin
// holds the item, the cost/fee consistency check passes, the announce is ignored) pending
// per-connection identity plumbing on the dial path.
// The remote identity a tx-origin exclusion keys on. An INBOUND link carries the peer's true
// cert-hash id (`peer_id` is exact); every OUTBOUND dial shares OUR client cert hash
// (clients/src/websocket/mod.rs `peer_id = hash_256(certs[0])` over our OWN cert), so an outbound
// origin is only distinguishable by its dialed remote `host`.
#[derive(Clone, Copy)]
struct TxOrigin {
    peer_id: Bytes32,
    host: Option<IpAddr>,
}

// Bounded insert into the tx-origin map: prune
// entries older than 60s and cap the map at 4096, so an unconsumed entry (a bundle that fails
// admission, hence is never announced/consumed) cannot grow it without bound.
async fn record_tx_origin(
    origins: &Mutex<HashMap<Bytes32, (TxOrigin, Instant)>>,
    txid: Bytes32,
    origin: TxOrigin,
) {
    let mut o = origins.lock().await;
    o.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
    if o.len() < 4096 {
        o.insert(txid, (origin, Instant::now()));
    }
}

// Whether a FULL_NODE peer is the origin of a re-broadcast tx and must be skipped from the
// NewTransaction send. Callers pass the dispatch id for INBOUND peers (exact cert-hash match)
// and the remote host
// for OUTBOUND peers (their shared cert hash cannot identify them); the peer is the origin when
// EITHER the id or the host matches what was recorded at receipt.
fn is_tx_rebroadcast_origin(
    origin: Option<&TxOrigin>,
    peer_id: Option<&Bytes32>,
    peer_host: Option<IpAddr>,
) -> bool {
    let Some(o) = origin else {
        return false;
    };
    if let Some(id) = peer_id
        && *id == o.peer_id
    {
        return true;
    }
    matches!((o.host, peer_host), (Some(a), Some(b)) if a == b)
}

// A tx's origin peer is excluded from the NewTransaction re-broadcast. Inbound origins are
// excluded by their exact cert-hash id; the residual is the OUTBOUND origin — every outbound
// dial shares our own client-cert hash as its dispatch id, so it cannot be identified that way.
// The exclusion records the origin's
// remote HOST too and excludes an outbound peer whose dialed host matches.
#[cfg(test)]
mod tx_origin_exclusion_tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    // An outbound dial's dispatch id is our OWN cert hash, shared across every outbound peer,
    // so exclusion must key on the remote host or the origin peer gets its own echo back.
    #[test]
    fn outbound_origin_is_excluded_by_remote_host() {
        let our_id = Bytes32::from([0xAB; 32]); // our shared outbound dispatch id
        let origin = TxOrigin {
            peer_id: our_id,
            host: Some(ip("203.0.113.7")),
        };
        // The origin outbound peer (same host) MUST be excluded.
        assert!(
            is_tx_rebroadcast_origin(Some(&origin), None, Some(ip("203.0.113.7"))),
            "the outbound peer the tx arrived from must not get an echo"
        );
        // A DIFFERENT outbound peer (other host) must still receive it.
        assert!(
            !is_tx_rebroadcast_origin(Some(&origin), None, Some(ip("198.51.100.9"))),
            "a non-origin outbound peer must still receive the re-broadcast"
        );
    }

    // The inbound-origin case stays exact on the cert-hash id (unchanged behavior).
    #[test]
    fn inbound_origin_is_excluded_by_exact_peer_id() {
        let origin_id = Bytes32::from([0x11; 32]);
        let origin = TxOrigin {
            peer_id: origin_id,
            host: Some(ip("198.51.100.9")),
        };
        assert!(
            is_tx_rebroadcast_origin(Some(&origin), Some(&origin_id), None),
            "the inbound origin is excluded by its exact cert-hash id"
        );
        let other = Bytes32::from([0x22; 32]);
        assert!(
            !is_tx_rebroadcast_origin(Some(&origin), Some(&other), None),
            "a different inbound peer still receives the re-broadcast"
        );
    }

    // No recorded origin (e.g. a locally-pushed tx) → nobody excluded.
    #[test]
    fn no_origin_excludes_nobody() {
        assert!(!is_tx_rebroadcast_origin(
            None,
            None,
            Some(ip("203.0.113.7"))
        ));
        assert!(!is_tx_rebroadcast_origin(
            None,
            Some(&Bytes32::from([0x33; 32])),
            None
        ));
    }
}

async fn broadcast_transactions<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announcements: Vec<NewTransaction> = node.tx_announce.lock().await.drain(..).collect();
    if announcements.is_empty() {
        return;
    }
    // Resolve and CONSUME each announcement's origin (peer id + remote host).
    let origin_of: HashMap<Bytes32, TxOrigin> = {
        let mut origins = node.tx_origin.lock().await;
        announcements
            .iter()
            .filter_map(|tx| {
                origins
                    .remove(&tx.transaction_id)
                    .map(|(origin, _)| (tx.transaction_id, origin))
            })
            .collect()
    };
    let version = ChiaProtocolVersion::default();
    let outbound = registry.live_peers().await;
    let inbound: Vec<(Bytes32, Arc<SocketPeer>)> = node
        .inbound_peers
        .read()
        .await
        .iter()
        .map(|(id, peer)| (*id, peer.clone()))
        .collect();
    for tx in announcements {
        let origin = origin_of.get(&tx.transaction_id);
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewTransaction,
            version,
            &tx,
            None,
        ) else {
            continue;
        };
        for peer in &outbound {
            // Exclude the origin outbound peer by its dialed remote host — an outbound dial's
            // dispatch id is our own shared cert hash, so host is its only distinct identity
            // (the origin exclusion).
            if is_tx_rebroadcast_origin(origin, None, peer.endpoint.0.parse::<IpAddr>().ok()) {
                continue;
            }
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewTransaction,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg.clone()).await;
        }
        for (peer_id, peer) in &inbound {
            // Inbound peers carry their true cert-hash id — exact origin match (unchanged).
            if is_tx_rebroadcast_origin(origin, Some(peer_id), None) {
                continue;
            }
            if *peer.node_type.read().await != NodeType::FullNode {
                continue;
            }
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewTransaction,
                msg.data.as_slice().len(),
            );
            let _ = peer.send(msg.clone()).await;
        }
    }
}

// Send NewPeak for the just-confirmed tip to every live outbound peer. Fire-and-forget: a
// send failure only means that peer misses one announcement (the next step re-announces).
// The message carries the UNFINISHED reward-chain-block hash of the peak — peers key
// their unfinished-block caches on it — derived from
// `reward_chain_block.get_unfinished()`.
async fn broadcast_new_peak<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<Node<S>>,
    registry: &Arc<dyn OutboundPeers>,
    header_hash: Bytes32,
    height: u32,
) {
    let Ok(Some(block)) = node.store.get_block(&header_hash).await else {
        return;
    };
    let version = ChiaProtocolVersion::default();
    let unfinished = block.reward_chain_block.get_unfinished();
    let Ok(unfinished_bytes) = unfinished.to_bytes(version) else {
        return;
    };
    let peak = NewPeak {
        header_hash,
        height,
        weight: block.reward_chain_block.weight,
        fork_point_with_previous_peak: height.saturating_sub(1),
        unfinished_reward_block_hash: dg_xch_core::utils::hash_256(&unfinished_bytes).into(),
    };
    let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
        dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
        version,
        &peak,
        None,
    ) else {
        return;
    };
    for peer in registry.live_peers().await {
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            msg.data.as_slice().len(),
        );
        let _ = peer.client.send(msg.clone()).await;
    }
    // Inbound full-node peers (peers that dialed US) hear peaks too. Without this they got one
    // on-connect greeting and then silence: their book's claim for us aged past its stale TTL,
    // and a fetch frontier clamped to a servable tip we never refreshed wedged their sync while
    // their weight-heaviest target kept riding fresh gossip.
    let inbound: Vec<Arc<SocketPeer>> = {
        let mut list = Vec::new();
        for peer in node.inbound_peers.read().await.values() {
            if *peer.node_type.read().await == NodeType::FullNode {
                list.push(peer.clone());
            }
        }
        list
    };
    for peer in inbound {
        let peer_version = *peer.protocol_version.read().await;
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            peer_version,
            &peak,
            None,
        ) else {
            continue;
        };
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            msg.data.as_slice().len(),
        );
        let _ = peer.send(msg).await;
    }
}

fn in_near_tip_band(local: u32, claimed: u32, has_peak: bool) -> bool {
    if !has_peak {
        return false;
    }
    let gap = claimed.saturating_sub(local);
    gap > 0 && gap <= SHORT_SYNC_BLOCKS_BEHIND_THRESHOLD
}

// catch-up forever after.
fn wants_fast_sync(local: u32, claimed: u32) -> bool {
    local < FAST_SYNC_GAP && claimed.saturating_sub(local) > FAST_SYNC_GAP
}

fn wants_long_sync(local: u32, claimed: u32) -> bool {
    claimed >= FAST_SYNC_GAP && claimed.saturating_sub(local) > SYNC_BLOCKS_BEHIND_THRESHOLD
}

// The action the mid-chain long-sync band takes once the WP fork point is resolved against the
// local chain (pure, so the decision is unit-testable). The next-block probe lifts the no-fork
// conservative point to the local peak when a peer's peak+1 block connects to our chain (its
// prev is our peak); otherwise the sync starts at the conservative
// point — below the peak that flows through the engine's atomic reorg reland, at/above it the
// detached pipeline simply extends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LongSyncPlan {
    // The chains agree through the local peak: the pipeline extends from peak + 1 in place.
    Extend,
    // The chains diverged (or the no-fork probe failed) below our peak: reland from the fork
    // point through the engine's atomic reorg before the pipeline may extend.
    Rewind { fork_point: u32 },
    // No fork point could be established (divergence below the record floor): fail closed and
    // retry next tick — never blindly extend a chain the proof does not attest.
    Stall,
}

fn long_sync_plan(fork: &WpForkPoint, local_peak: u32, next_block_connects: bool) -> LongSyncPlan {
    match *fork {
        WpForkPoint::NoForkDetected { conservative } => {
            if next_block_connects || conservative >= local_peak {
                // fork_point = our peak height.
                LongSyncPlan::Extend
            } else {
                // The probe did not confirm our tip is on the proof's chain: keep the
                // conservative two-sub-epoch back-off and let the reland decide (identical
                // blocks re-confirm as AlreadyHave; a divergent branch reorgs atomically).
                LongSyncPlan::Rewind {
                    fork_point: conservative,
                }
            }
        }
        WpForkPoint::Diverged { fork_point } if fork_point < local_peak => {
            LongSyncPlan::Rewind { fork_point }
        }
        WpForkPoint::Diverged { .. } => LongSyncPlan::Extend,
        WpForkPoint::Unknown => LongSyncPlan::Stall,
    }
}

// The next-block probe: fetch the block at our peak + 1 and report whether its prev header
// hash IS our peak — the lift that turns the conservative no-fork point into "start from our
// peak". The probe iterates the peers-with-peak
// until ONE confirms (a stale peer's miss does not veto); so does this.
async fn next_block_connects(
    peers: &[Arc<OutboundPeer>],
    peak_hash: Bytes32,
    peak_height: u32,
) -> bool {
    let next = peak_height.saturating_add(1);
    for peer in peers {
        let source = OutboundPeerSource::new(peer.clone(), REQUEST_TIMEOUT);
        if let Ok(blocks) = source.fetch_range(next, next).await
            && blocks
                .iter()
                .any(|b| b.height() == next && b.prev_header_hash() == peak_hash)
        {
            return true;
        }
    }
    false
}

// A minimal seam over the p2p registry so the driver depends on "give me a live peer", not the concrete
// registry type — keeps the driver testable and the coupling explicit.
#[async_trait]
pub trait OutboundPeers: Send + Sync {
    async fn first_live(&self) -> Option<Arc<OutboundPeer>>;
    // Every live outbound channel — the reservation slots the bulk-sync download spreads across.
    async fn live_peers(&self) -> Vec<Arc<OutboundPeer>>;
}

#[async_trait]
impl OutboundPeers for dg_xch_p2p::PeerRegistry {
    async fn first_live(&self) -> Option<Arc<OutboundPeer>> {
        self.outbound_peers()
            .await
            .into_iter()
            .find(|p| !p.is_closed())
    }

    async fn live_peers(&self) -> Vec<Arc<OutboundPeer>> {
        self.outbound_peers()
            .await
            .into_iter()
            .filter(|p| !p.is_closed())
            .collect()
    }
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::blockchain::coin_record::CoinRecord;
    use std::time::{SystemTime, UNIX_EPOCH};

    // A subscribed coin spent on branch A must read UNSPENT again after a reorg to branch B where
    // the spend never happened, which means delivering the POST-ROLLBACK records to subscribers.
    // Reporting only the reorg tip's own delta would leave the subscriber hearing nothing about
    // the rollback. Drives the daemon's confirm tail (finish_follow_step) with exactly what the
    // chaser produces for a landed reorg.
    #[tokio::test]
    async fn reorg_rollback_states_reach_subscribed_wallets() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!(
            "fn_reorg_wallet_{}_{nanos}.sqlite",
            std::process::id()
        ));
        let node = Node::boot(Config {
            p2p: P2pSettings::default(),
            listen: "127.0.0.1:0".parse().unwrap(),
            rpc: "127.0.0.1:0".parse().unwrap(),
            introducer: None,
            manual_peers: Vec::new(),
            advertise: None,
            backend: Backend::Sqlite(db),
            network_id: "mainnet".to_string(),
            metrics: None,
            capture_dir: None,
            genesis_sync: false,
            sync_from: 0,
            uncompact: false,
            prefetch_memory_mb: None,
            prefetch_max_inflight: None,
            trusted_peers: Vec::new(),
            trusted_cidrs: Vec::new(),
            rpc_tls: crate::config::RpcTlsMode::Local,
            debug_endpoints: false,
        })
        .await
        .expect("boot");

        // Coin X: created at 90 (below the fork), spent on branch A at 101 — the reorg to
        // branch B rolls the spend back. Coin Y: created on branch A at 101 — the reorg deletes
        // it. Both post-rollback records arrive from the engine's ReorgReport.
        let x = CoinRecord {
            coin: dg_xch_core::blockchain::coin::Coin {
                parent_coin_info: Bytes32::from([0xC0; 32]),
                puzzle_hash: Bytes32::from([0xC1; 32]),
                amount: 1_000,
            },
            confirmed_block_index: 90,
            spent_block_index: 0,
            coinbase: false,
            timestamp: 1_700_000_000,
            spent: false,
        };
        let y = CoinRecord {
            coin: dg_xch_core::blockchain::coin::Coin {
                parent_coin_info: Bytes32::from([0xC2; 32]),
                puzzle_hash: Bytes32::from([0xC3; 32]),
                amount: 2_000,
            },
            confirmed_block_index: 0, // rolled back: no longer on chain
            spent_block_index: 0,
            coinbase: false,
            timestamp: 0,
            spent: false,
        };

        // A wallet peer subscribed to both coins by id.
        let peer = Bytes32::from([0x77; 32]);
        let (rx, added) = node
            .wallet
            .register_for_coin_updates(peer, None, &[x.coin.name(), y.coin.name()])
            .await
            .expect("subscribe");
        let mut rx = rx.expect("first registration hands the delivery receiver");
        assert_eq!(added.len(), 2);

        // The reorg's first re-applied block, exactly as the chaser reports it: the branch-B
        // delta with the rollback attached (fork at 100).
        let rec: dg_xch_core::blockchain::block_record::BlockRecord = {
            let records: Vec<dg_xch_core::blockchain::block_record::BlockRecord> =
                serde_json::from_str(include_str!("../tests/fixtures/block_records.json"))
                    .expect("records fixture");
            records[0].clone()
        };
        let delta = dg_xch_node::BlockDelta {
            header_hash: Bytes32::from([0xB1; 32]),
            prev_hash: Bytes32::from([0xB0; 32]),
            height: 101,
            weight: 1_350,
            timestamp: 0, // non-transaction block: the mempool frame stays untouched
            record: rec,
            additions: Vec::new(),
            removals: Vec::new(),
            hints: Vec::new(),
        };
        let cd = ConfirmedDelta {
            delta,
            reorg: Some(ReorgWalletDelta {
                fork_height: 100,
                rolled_back: vec![
                    CoinRecord {
                        spent_block_index: 0,
                        spent: false,
                        ..x
                    },
                    y,
                ],
            }),
        };
        node.finish_follow_step(None, std::slice::from_ref(&cd))
            .await
            .expect("confirm tail");

        let update = rx.try_recv().expect(
            "the subscriber must hear the rolled-back coin states (pre-threading it heard nothing)",
        );
        assert_eq!(
            update.fork_height, 100,
            "the TRUE fork height, not height-1"
        );
        assert_eq!(update.height, 101);
        let x_state = update
            .items
            .iter()
            .find(|s| s.coin == x.coin)
            .expect("coin X state present");
        assert_eq!(
            x_state.spent_height, None,
            "spent-on-branch-A coin reads unspent again after the reorg"
        );
        assert_eq!(x_state.created_height, Some(90));
        let y_state = update
            .items
            .iter()
            .find(|s| s.coin == y.coin)
            .expect("coin Y state present");
        assert_eq!(
            y_state.created_height, None,
            "created-on-branch-A coin reads not-on-chain after the reorg"
        );
    }

    #[test]
    fn fast_sync_triggers_only_from_near_empty_store_far_behind() {
        assert!(wants_fast_sync(0, 9_000_000), "empty store, tip far ahead");
        assert!(
            wants_fast_sync(500, 9_000_000),
            "near-empty store, tip far ahead"
        );
        assert!(
            !wants_fast_sync(0, 10),
            "gap smaller than a follow-worthy delta"
        );
        assert!(
            !wants_fast_sync(9_000_000, 9_050_000),
            "local already synced: tip-follow owns it"
        );
        assert!(
            !wants_fast_sync(1500, 9_000_000),
            "local past the fresh-store gate"
        );
    }

    #[test]
    fn deep_mid_chain_gap_selects_the_wp_anchored_long_sync_band() {
        // A node at 2M offline for ~a month (gap ≈ 50k blocks): long sync (gap > 300).
        assert!(
            wants_long_sync(2_000_000, 2_050_000),
            "a deep mid-chain gap must enter the WP-anchored long-sync band \
             (sync_blocks_behind_threshold = 300)"
        );
        assert!(
            !wants_long_sync(2_000_000, 2_000_300),
            "a gap at/below the threshold stays with the follow band"
        );
        // A tip below WEIGHT_PROOF_RECENT_BLOCKS (1000) cannot be weight-proof-anchored —
        // batch sync from zero covers that band.
        assert!(
            !wants_long_sync(0, 900),
            "a tip below the weight-proof floor is never long-synced"
        );
        assert!(wants_long_sync(0, 9_000_000) && wants_fast_sync(0, 9_000_000));
        assert!(wants_long_sync(1500, 9_000_000) && !wants_fast_sync(1500, 9_000_000));
    }

    // The gap-closes-mid-sync exit ladder: while the gap stays past the threshold
    // the long-sync band owns catch-up toward the (re-polled, possibly advanced) target; within
    // the threshold it hands off to the FOLLOW band; within the short-sync threshold the
    // event-driven tip_follower owns the last blocks.
    #[test]
    fn long_sync_band_exits_cleanly_through_follow_then_near_tip() {
        let local = 2_000_000u32;
        // Deep in the gap — long sync, and the target advancing mid-sync keeps the SAME band.
        assert!(wants_long_sync(local, 2_050_000));
        assert!(
            wants_long_sync(local, 2_050_500),
            "advanced target: still long sync"
        );
        // Caught up to within the threshold: FOLLOW owns it (neither long-sync nor near-tip).
        let near = 2_050_500u32 - 200;
        assert!(!wants_long_sync(near, 2_050_500));
        assert!(!in_near_tip_band(near, 2_050_500, true));
        // Within the short-sync threshold: the tip_follower's event-driven band.
        assert!(in_near_tip_band(2_050_495, 2_050_500, true));
        // Caught up: no band wants work.
        assert!(!wants_long_sync(2_050_500, 2_050_500));
        assert!(!in_near_tip_band(2_050_500, 2_050_500, true));
    }

    // The fork-point → action mapping.
    #[test]
    fn long_sync_plan_follows_chia_fork_point_semantics() {
        let peak = 2_000_000u32;
        // No fork detected + a peer's peak+1 connects → lift to our peak: extend in place.
        assert_eq!(
            long_sync_plan(
                &WpForkPoint::NoForkDetected {
                    conservative: 1_999_000
                },
                peak,
                true
            ),
            LongSyncPlan::Extend
        );
        // No fork detected but NO peer confirms our tip: keep the conservative two-sub-epoch
        // back-off — the reland re-follows from there (identical blocks are AlreadyHave).
        assert_eq!(
            long_sync_plan(
                &WpForkPoint::NoForkDetected {
                    conservative: 1_999_000
                },
                peak,
                false
            ),
            LongSyncPlan::Rewind {
                fork_point: 1_999_000
            }
        );
        // A detected divergence below our peak MUST rewind through the engine reorg — never
        // blindly extend the stale branch.
        assert_eq!(
            long_sync_plan(
                &WpForkPoint::Diverged {
                    fork_point: 1_998_500
                },
                peak,
                false
            ),
            LongSyncPlan::Rewind {
                fork_point: 1_998_500
            }
        );
        // No fork point within the walk window: fail closed, retry — never batch-sync blind.
        assert_eq!(
            long_sync_plan(&WpForkPoint::Unknown, peak, false),
            LongSyncPlan::Stall
        );
    }

    // The peer-free consumer's recovery signal round-trips through the driver
    // channel and unparks it. `await_reset` sends a `()`-reply request and awaits; a mock driver drains
    // the channel and replies — the RecoveryRequest → oneshot handshake the real orphan/repair/reset
    // paths ride on (the consumer holds no lock while parked here).
    #[tokio::test]
    async fn recovery_signal_round_trips_and_unparks_the_consumer() {
        let (tx, mut rx) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
        let driver = tokio::spawn(async move {
            match rx.recv().await {
                Some(RecoveryRequest::Orphan { from, to, reply }) => {
                    assert_eq!(
                        (from, to),
                        (100, 131),
                        "the driver sees the orphaned window"
                    );
                    reply.send(()).is_ok()
                }
                _ => false,
            }
        });
        let unparked = await_reset(&tx, |reply| RecoveryRequest::Orphan {
            from: 100,
            to: 131,
            reply,
        })
        .await;
        assert!(unparked, "consumer unparked on the driver's reply");
        assert!(
            driver.await.unwrap(),
            "driver replied after servicing recovery"
        );
    }

    // Shutdown path: the driver dropped its receiver; the consumer's recovery send fails so it reports
    // the channel closed and can exit its loop instead of parking forever.
    #[tokio::test]
    async fn recovery_send_reports_closed_channel_for_clean_consumer_exit() {
        let (tx, rx) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
        drop(rx);
        let ok = await_reset(&tx, |reply| RecoveryRequest::Reset { reply }).await;
        assert!(
            !ok,
            "no driver → await_reset reports closed so the consumer exits"
        );
    }

    // A WEDGED driver that never services recovery must not hang the confirm consumer forever.
    // An unbounded `await_reset` hangs permanently: the consumer stops draining the queue, the
    // producer parks on a full buffer, and the whole pipeline freezes. It must return within
    // RESET_REPLY_TIMEOUT and retry. The receiver is kept alive so the send succeeds but is never
    // replied to; virtual time auto-advances to the internal timeout.
    #[tokio::test(start_paused = true)]
    async fn recovery_reply_timeout_unparks_the_consumer_when_the_driver_is_wedged() {
        // Keep the receiver alive so the request is buffered but NEVER replied to.
        let (tx, _rx_alive) = mpsc::channel::<RecoveryRequest>(RECOVERY_CHANNEL_CAP);
        // Outer bound is a test guard; the internal RESET_REPLY_TIMEOUT must fire first.
        let out = tokio::time::timeout(
            RESET_REPLY_TIMEOUT + Duration::from_secs(5),
            await_reset(&tx, |reply| RecoveryRequest::Reset { reply }),
        )
        .await;
        assert!(
            out.is_ok(),
            "await_reset must not hang when the driver never replies (bounded park)"
        );
        assert!(
            out.unwrap(),
            "on a reply-timeout the consumer proceeds (retry next window), it does not exit"
        );
    }

    // The confirm consumer must NEVER block on the announcer. A raw `peak_tx.send().await` on a
    // full, undrained channel blocks (the first assertion proves it), so a wedged announcer stops
    // the consumer draining the BlockQueue and parks the producer — a permanent wedge.
    // `emit_confirmed_peak` drops the best-effort announcement under backpressure instead.
    #[tokio::test]
    async fn emit_confirmed_peak_never_blocks_the_consumer_on_a_wedged_announcer() {
        let (tx, _rx_wedged) = mpsc::channel::<ConfirmedPeak>(1); // never drained = stalled announcer
        tx.try_send(ConfirmedPeak {
            hash: Bytes32::default(),
            height: 1,
        })
        .expect("first fits");

        // A raw bounded send on the full channel blocks — the consumer wedge.
        let blocked = tokio::time::timeout(
            Duration::from_millis(200),
            tx.send(ConfirmedPeak {
                hash: Bytes32::default(),
                height: 2,
            }),
        )
        .await;
        assert!(
            blocked.is_err(),
            "a raw send on a full channel blocks the consumer"
        );

        // emit_confirmed_peak returns immediately (drops under backpressure) and never awaits.
        let emitted = emit_confirmed_peak(
            &tx,
            ConfirmedPeak {
                hash: Bytes32::default(),
                height: 3,
            },
        );
        assert!(
            emitted,
            "a dropped best-effort NewPeak announcement is not a fatal error — the consumer proceeds"
        );

        // And when the announcer is GONE, emit reports failure so the consumer exits cleanly.
        drop(_rx_wedged);
        assert!(
            !emit_confirmed_peak(
                &tx,
                ConfirmedPeak {
                    hash: Bytes32::default(),
                    height: 4,
                }
            ),
            "a closed announcer channel → emit reports failure for a clean consumer exit"
        );
    }

    // Red-first (Item 2, capabilities/version branching): a peer's unfinished-block announce type is
    // chosen by its negotiated protocol version, split at 0.0.35 — old peers get
    // v1 (NewUnfinishedBlock), new peers get v2 (NewUnfinishedBlock2).
    #[test]
    fn unfinished_announce_version_split_matches_chia_0_0_35_boundary() {
        assert!(
            !announce_v2_for(ChiaProtocolVersion::Chia0_0_34),
            "0.0.34 is an old client — v1"
        );
        assert!(
            !announce_v2_for(ChiaProtocolVersion::Chia0_0_35),
            "0.0.35 is the boundary, still old-client (<= 0.0.35) — v1"
        );
        assert!(
            announce_v2_for(ChiaProtocolVersion::Chia0_0_36),
            "0.0.36 is a new client — v2"
        );
        assert!(
            announce_v2_for(ChiaProtocolVersion::Chia0_0_37),
            "0.0.37 is a new client — v2"
        );
    }

    // A fresh peak book with its published claimed-peak gauge, as boot_with_store wires them.
    fn test_book() -> (Arc<AtomicU32>, Arc<PeakBook>) {
        let claimed_peak = Arc::new(AtomicU32::new(0));
        let book = Arc::new(PeakBook::new(claimed_peak.clone()));
        (claimed_peak, book)
    }

    // Builds a StoreApi over a throwaway SQLite store sharing the given peak book — the peak-claim
    // tests below all need the same scaffold. `claim_guard` None = the shared inbound server api
    // (claims keyed by the real peer id); Some = one outbound connection (claims keyed by the guard).
    async fn peak_test_api(
        claimed_peak: &Arc<AtomicU32>,
        book: &Arc<PeakBook>,
        claim_guard: Option<Arc<ClaimGuard>>,
    ) -> StoreApi<SqliteStore> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fn_peakclaim_{}_{nanos}.sqlite",
            std::process::id()
        ));
        let store = open_backend(&Backend::Sqlite(path))
            .await
            .expect("open store");
        StoreApi {
            store,
            mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
            constants: MAINNET,
            claimed_peak: claimed_peak.clone(),
            peak_book: book.clone(),
            claim_guard,
            new_peak_signal: Arc::new(Notify::new()),
            known_peers: Arc::new(RwLock::new(Vec::new())),
            tx_requested: Arc::new(Mutex::new(HashMap::new())),
            slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
            sp_inbox: Arc::new(Mutex::new(Vec::new())),
            unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
            ub_inbox: Arc::new(Mutex::new(Vec::new())),
            ip_inbox: Arc::new(Mutex::new(Vec::new())),
            synced: Arc::new(AtomicBool::new(true)),
            wallet_compat: Arc::new(AtomicBool::new(false)),
            tx_inbox: Arc::new(Mutex::new(TxQueue::new(
                TX_INBOX_CAP,
                TX_INBOX_PER_PEER,
                MAINNET.max_block_cost_clvm / 2,
            ))),
            tx_announce: Arc::new(Mutex::new(Vec::new())),
            tx_origin: Arc::new(Mutex::new(HashMap::new())),
            wp_inbox: Arc::new(Mutex::new(Vec::new())),
            compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
            proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
            candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
            producer: Arc::new(ProducerMetrics::default()),
            farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
            wallet: Arc::new(WalletNotifier::new()),
            trust: Arc::new(TrustPolicy::default()),
            wallet_sync_sem: Arc::new(LimitedSemaphore::new(
                WALLET_SYNC_ACTIVE_LIMIT,
                WALLET_SYNC_WAITING_LIMIT,
            )),
            record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
            sync_metrics: Arc::new(SyncMetrics::default()),
        }
    }

    // An outbound peer's NewPeak records its per-connection claim (hash, height, WEIGHT —
    // sync_store.peer_has_block), and the connection's NEWEST announcement replaces it (the
    // withdrawal path). Another connection's lighter claim never lowers the published heaviest.
    #[tokio::test]
    async fn outbound_new_peak_records_claimed_peak_and_tip() {
        let (claimed_peak, book) = test_book();
        let api = peak_test_api(&claimed_peak, &book, Some(Arc::new(book.outbound_guard()))).await;

        let tip_hash = Bytes32::const_new([7u8; 32]);
        let peak = NewPeak {
            header_hash: tip_hash,
            height: 9_054_698,
            weight: 9_000,
            fork_point_with_previous_peak: 0,
            unfinished_reward_block_hash: Bytes32::default(),
        };
        api.on_new_peak(Bytes32::default(), peak.clone()).await;
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_054_698);
        assert_eq!(
            book.heaviest(),
            Some(PeakClaim {
                header_hash: tip_hash,
                height: 9_054_698,
                weight: 9_000,
            })
        );

        // A LIGHTER claim from a DIFFERENT connection does not lower the published heaviest.
        let other =
            peak_test_api(&claimed_peak, &book, Some(Arc::new(book.outbound_guard()))).await;
        let lower = NewPeak {
            header_hash: Bytes32::const_new([8u8; 32]),
            height: 5,
            weight: 10,
            ..peak.clone()
        };
        other.on_new_peak(Bytes32::default(), lower.clone()).await;
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_054_698);

        // The SAME connection re-announcing lower REPLACES its claim
        // — the withdrawal path the old fetch_max slot lacked.
        api.on_new_peak(Bytes32::default(), lower).await;
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 5);
    }

    fn peak(hash: [u8; 32], height: u32, weight: u128) -> NewPeak {
        NewPeak {
            header_hash: Bytes32::const_new(hash),
            height,
            weight,
            fork_point_with_previous_peak: 0,
            unfinished_reward_block_hash: Bytes32::default(),
        }
    }

    // WEIGHT is the
    // fork-choice ordering key, not height. A heavier-but-shorter peak must be the sync/weight-proof
    // target over a longer-but-lighter fork, regardless of announcement order.
    #[tokio::test]
    async fn peak_selection_prefers_weight_over_height() {
        let heavy_short = peak([0xAA; 32], 100, 1_000);
        let light_long = peak([0xBB; 32], 120, 900);
        let expected = PeakClaim {
            header_hash: heavy_short.header_hash,
            height: heavy_short.height,
            weight: heavy_short.weight,
        };

        // Order 1: the heavy peak arrives first, the light-but-longer fork second.
        let (claimed_peak, book) = test_book();
        let api = peak_test_api(&claimed_peak, &book, None).await;
        api.on_new_peak(Bytes32::const_new([1; 32]), heavy_short.clone())
            .await;
        api.on_new_peak(Bytes32::const_new([2; 32]), light_long.clone())
            .await;
        assert_eq!(
            book.heaviest(),
            Some(expected),
            "heaviest claim is the target even when a longer-but-lighter fork arrives later"
        );
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 100);

        // Order 2: the light-but-longer fork arrives first.
        let (claimed_peak, book) = test_book();
        let api = peak_test_api(&claimed_peak, &book, None).await;
        api.on_new_peak(Bytes32::const_new([2; 32]), light_long.clone())
            .await;
        api.on_new_peak(Bytes32::const_new([1; 32]), heavy_short.clone())
            .await;
        assert_eq!(
            book.heaviest(),
            Some(expected),
            "heaviest claim is the target even when it is shorter than an earlier claim"
        );
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 100);
    }

    // A peer's
    // peak claim dies with its connection. A bogus high announcement from a peer that then disconnects
    // must not pin the claimed slot (and with it the FOLLOW band) forever.
    #[tokio::test]
    async fn withdrawn_claim_retracts_when_the_announcing_connection_drops() {
        let (claimed_peak, book) = test_book();
        // One OUTBOUND connection, as the factory builds it: claim keyed by the minted guard.
        let api = peak_test_api(&claimed_peak, &book, Some(Arc::new(book.outbound_guard()))).await;
        api.on_new_peak(
            Bytes32::const_new([9; 32]),
            peak([0xEE; 32], 9_999_999, u128::MAX),
        )
        .await;
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_999_999);
        // The announcing connection goes away — its per-connection handler map (and with it this
        // StoreApi and its ClaimGuard) is dropped. The claim retracts in
        // sync_store.peer_disconnected.
        drop(api);
        assert_eq!(
            claimed_peak.load(Ordering::Relaxed),
            0,
            "a dead peer's phantom peak must not pin the claimed slot"
        );
        assert_eq!(
            book.heaviest(),
            None,
            "a dead peer's phantom tip must not remain the weight-proof target"
        );
    }

    // Inbound claims (the shared server api keys them by the REAL peer id) retract through the
    // driver's per-tick reconcile against the live inbound map — the other half of the
    // on_disconnect → sync_store.peer_disconnected.
    #[tokio::test]
    async fn inbound_claim_retracts_when_the_peer_leaves_the_live_map() {
        let (claimed_peak, book) = test_book();
        let api = peak_test_api(&claimed_peak, &book, None).await;
        let peer_id = Bytes32::const_new([9; 32]);
        api.on_new_peak(peer_id, peak([0xEE; 32], 9_999_999, u128::MAX))
            .await;
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 9_999_999);
        // The driver's sweep with the peer absent from the live inbound map retracts its claim.
        book.reconcile(&std::collections::HashSet::new());
        assert_eq!(claimed_peak.load(Ordering::Relaxed), 0);
        assert_eq!(book.heaviest(), None);
    }

    // The weight-proof ↔ claim cross-check inputs ("Weight proof
    // had the wrong height/weight"): validated_proof compares the proof's LAST recent-chain block
    // (height, weight) against the announced claim. Against the real mainnet proof fixture, the
    // attested pair is the fixture tip with a real (nonzero) weight — so an announcement whose
    // height or weight differs (a phantom peak with an inflated weight) cannot pass the comparison.
    #[test]
    fn weight_proof_recent_chain_attests_the_claimed_tip() {
        let bytes =
            include_bytes!("../../weight-proof/tests/fixtures/weight_proof_mainnet_9054698.bin");
        let wp = dg_xch_core::blockchain::weight_proof::WeightProof::from_bytes(
            &mut std::io::Cursor::new(&bytes[..]),
            ChiaProtocolVersion::default(),
        )
        .expect("real mainnet weight proof deserializes");
        let last = wp.recent_chain_data.last().expect("recent chain non-empty");
        assert_eq!(
            last.height(),
            9_054_698,
            "the proof attests the fixture tip height"
        );
        assert!(
            last.weight() > 0,
            "the proof carries the tip's real cumulative weight for the claim comparison"
        );
    }

    #[tokio::test]
    async fn sync_target_weight_gates_against_the_local_peak() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!(
            "fn_synctarget_{}_{nanos}.sqlite",
            std::process::id()
        ));
        let config = Config {
            p2p: P2pSettings::default(),
            listen: "127.0.0.1:0".parse().unwrap(),
            rpc: "127.0.0.1:0".parse().unwrap(),
            introducer: None,
            manual_peers: Vec::new(),
            advertise: None,
            backend: Backend::Sqlite(db),
            network_id: "mainnet".to_string(),
            metrics: None,
            capture_dir: None,
            genesis_sync: false,
            sync_from: 0,
            uncompact: false,
            prefetch_memory_mb: None,
            prefetch_max_inflight: None,
            trusted_peers: Vec::new(),
            trusted_cidrs: Vec::new(),
            rpc_tls: crate::config::RpcTlsMode::Local,
            debug_endpoints: false,
        };
        let node = Node::boot(config).await.expect("boot");

        // No local peak, no claims: no target.
        assert_eq!(node.sync_target().await, None);

        let records: Vec<dg_xch_core::blockchain::block_record::BlockRecord> =
            serde_json::from_str(include_str!("../tests/fixtures/block_records.json"))
                .expect("records fixture");
        let rec = records
            .iter()
            .find(|r| r.height == 5_000_000)
            .expect("peak record present")
            .clone();
        node.store
            .add_block_records(std::slice::from_ref(&rec))
            .await
            .expect("records");
        node.store.set_peak(&rec.header_hash).await.expect("peak");
        assert_eq!(node.local_peak_weight().await, Some(rec.weight));

        // A longer-but-LIGHTER fork claim: taller than local, lighter than local — refused.
        let peer = Bytes32::const_new([1; 32]);
        node.peak_book.record(
            peer,
            true,
            PeakClaim {
                header_hash: Bytes32::const_new([0xBB; 32]),
                height: rec.height + 500,
                weight: rec.weight - 1,
            },
        );
        assert_eq!(
            node.sync_target().await,
            None,
            "a longer-but-lighter fork must not become the sync target"
        );

        // A strictly heavier claim IS the target.
        let heavy = PeakClaim {
            header_hash: Bytes32::const_new([0xCC; 32]),
            height: rec.height + 1,
            weight: rec.weight + 100,
        };
        node.peak_book
            .record(Bytes32::const_new([2; 32]), true, heavy);
        assert_eq!(node.sync_target().await, Some(heavy));

        // Quarantined (its weight proof failed): never re-selected, even though still claimed.
        node.peak_book.quarantine(heavy.header_hash, heavy.height);
        assert_eq!(
            node.sync_target().await,
            None,
            "a quarantined peak must not be re-selected while quarantined"
        );
    }

    // Red-first (beyond-tip reservation wedge, idxphase pg leg): the FOLLOW producer must clamp its
    // fetch frontier to the SERVABLE outbound tip, not to the weight-heaviest claim. An inbound peer
    // over-announcing past the real tip becomes the weight-heaviest target, but no peer we fetch from
    // serves that range — driving the producer to it is a beyond-tip rejection spin (`claimed=9208323
    // > tip=9208311`) that also emits a false "reservation wedge" WARN. Before the clamp,
    // follow_fill_claimed returned the over-claim height; after, it clamps to the outbound tip.
    #[tokio::test]
    async fn follow_fill_clamps_the_frontier_to_the_servable_outbound_tip() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!(
            "fn_clampfrontier_{}_{nanos}.sqlite",
            std::process::id()
        ));
        let config = Config {
            p2p: P2pSettings::default(),
            listen: "127.0.0.1:0".parse().unwrap(),
            rpc: "127.0.0.1:0".parse().unwrap(),
            introducer: None,
            manual_peers: Vec::new(),
            advertise: None,
            backend: Backend::Sqlite(db),
            network_id: "mainnet".to_string(),
            metrics: None,
            capture_dir: None,
            // genesis-sync so the FOLLOW band owns the fill (no weight-proof long-sync detour).
            genesis_sync: true,
            sync_from: 0,
            uncompact: false,
            prefetch_memory_mb: None,
            prefetch_max_inflight: None,
            trusted_peers: Vec::new(),
            trusted_cidrs: Vec::new(),
            rpc_tls: crate::config::RpcTlsMode::Local,
            debug_endpoints: false,
        };
        let node = Arc::new(Node::boot(config).await.expect("boot"));

        // An INBOUND peer over-claims 12 past the real tip with the heaviest weight -> the
        // weight-heaviest sync target, but a peer we never fetch from.
        node.peak_book.record(
            Bytes32::const_new([1; 32]),
            true,
            PeakClaim {
                header_hash: Bytes32::const_new([0xEE; 32]),
                height: 9_208_323,
                weight: u128::MAX,
            },
        );
        // The OUTBOUND peer we actually fetch from tops out at the real tip. Keep the guard alive so
        // the claim is not retracted before the assertion.
        let out_guard = node.peak_book.outbound_guard();
        node.peak_book.record(
            out_guard.key(),
            false,
            PeakClaim {
                header_hash: Bytes32::const_new([0xAA; 32]),
                height: 9_208_311,
                weight: 9_000,
            },
        );

        assert_eq!(
            node.sync_target().await.map(|t| t.height),
            Some(9_208_323),
            "the inbound over-claim is the weight-heaviest target",
        );
        assert_eq!(
            follow_fill_claimed(&node).await,
            Some(9_208_311),
            "the producer clamps the fetch frontier to the servable outbound tip, not the over-claim",
        );
    }

    // Red-first (`--sync-from` wedge): the anchor stages ancestry but sets no peak — the first
    // confirmed body does — so gating the FOLLOW fill on a peak alone deadlocks the pipeline:
    // the producer waits on a peak that only its own fill can create, while the driver re-anchors
    // every tick. Once the anchor is staged the fill must open with no peak in the store.
    #[tokio::test]
    async fn follow_fill_opens_the_sync_from_band_once_anchored() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db = std::env::temp_dir().join(format!(
            "fn_syncfromband_{}_{nanos}.sqlite",
            std::process::id()
        ));
        let config = Config {
            p2p: P2pSettings::default(),
            listen: "127.0.0.1:0".parse().unwrap(),
            rpc: "127.0.0.1:0".parse().unwrap(),
            introducer: None,
            manual_peers: Vec::new(),
            advertise: None,
            backend: Backend::Sqlite(db),
            network_id: "mainnet".to_string(),
            metrics: None,
            capture_dir: None,
            genesis_sync: false,
            sync_from: 5_490_000,
            uncompact: false,
            prefetch_memory_mb: None,
            prefetch_max_inflight: None,
            trusted_peers: Vec::new(),
            trusted_cidrs: Vec::new(),
            rpc_tls: crate::config::RpcTlsMode::Local,
            debug_endpoints: false,
        };
        let node = Arc::new(Node::boot(config).await.expect("boot"));

        let out_guard = node.peak_book.outbound_guard();
        node.peak_book.record(
            out_guard.key(),
            false,
            PeakClaim {
                header_hash: Bytes32::const_new([0xAA; 32]),
                height: 9_222_868,
                weight: u128::MAX,
            },
        );

        assert_eq!(
            follow_fill_claimed(&node).await,
            None,
            "before the anchor is staged the driver's anchor_at owns the band",
        );

        *node.sync_from_anchor.write().await = Some(5_489_936);
        assert_eq!(
            follow_fill_claimed(&node).await,
            Some(9_222_868),
            "once the anchor is staged the fill opens with no peak in the store",
        );
    }

    // The wedge detector distinguishes the benign at-tip drain from the real pathology: frozen BELOW
    // the servable tip (fetchable work nothing is requesting) is a wedge; frozen AT the tip is not.
    #[test]
    fn frozen_frontier_is_wedge_only_below_the_tip() {
        assert!(
            frozen_frontier_is_wedge(9_169_638, 9_208_311),
            "work below the tip left unrequested is a real wedge",
        );
        assert!(
            !frozen_frontier_is_wedge(9_208_311, 9_208_311),
            "frozen AT the servable tip is the benign drain-the-backlog state, not a wedge",
        );
    }

    // A candidate stored at declare time (placeholder foliage sigs) plus a farmer SignedValues
    // reply: the foliage_block_data signature is verified against the plot key, both signatures
    // are spliced in, and the finished block is pushed to ub_inbox — the same path a received
    // unfinished block takes to the driver's validate+broadcast.
    #[tokio::test]
    async fn signed_values_splices_farmer_sigs_and_queues_for_broadcast() {
        use blst::min_pk::SecretKey;
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
        use dg_xch_core::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
        use dg_xch_core::blockchain::sized_bytes::{Bytes48, Bytes96};
        use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
        use dg_xch_core::blockchain::vdf_info::VdfInfo;
        use dg_xch_core::blockchain::vdf_proof::VdfProof;
        use dg_xch_core::clvm::bls_bindings::sign;
        use dg_xch_core::consensus::producer::{
            FarmerSignatures, create_unfinished_block_with_sigs, g2_infinity,
        };

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("fn_signed_{}_{nanos}.sqlite", std::process::id()));
        let store = open_backend(&Backend::Sqlite(path))
            .await
            .expect("open store");

        // A real plot key so the fbd-signature verify in on_signed_values passes.
        let sk = SecretKey::key_gen_v3(&[0x5Au8; 32], &[]).expect("sk");
        let plot_pk: Bytes48 = sk.sk_to_pk().into();
        let pos = ProofOfSpace {
            version: 0,
            plot_index: 0,
            meta_group: 0,
            strength: 0,
            challenge: Bytes32::from([1u8; 32]),
            pool_public_key: None,
            pool_contract_puzzle_hash: Some(Bytes32::from([2u8; 32])),
            plot_public_key: plot_pk,
            size: 32,
            proof: ProofBytes::from(vec![7u8; 64]),
        };
        let vdf = |c: u8, n: u64| VdfInfo {
            challenge: Bytes32::from([c; 32]),
            number_of_iterations: n,
            output: ClassgroupElement::get_default_element(),
        };
        let proof = |w: u8| VdfProof {
            witness_type: w,
            witness: UnsizedBytes::new(vec![0xAA]),
            normalized_to_identity: true,
        };
        // A transaction-block candidate so both foliage signatures get spliced. Placeholder foliage sigs.
        let placeholder = FarmerSignatures {
            challenge_chain_sp_signature: g2_infinity(),
            reward_chain_sp_signature: g2_infinity(),
            foliage_block_data_signature: g2_infinity(),
            foliage_transaction_block_signature: g2_infinity(),
        };
        let candidate = create_unfinished_block_with_sigs(
            &MAINNET,
            10,
            0,
            pos,
            MAINNET.genesis_challenge,
            Some(vdf(0x10, 1)),
            Some(proof(1)),
            Some(vdf(0x11, 2)),
            Some(proof(2)),
            Vec::new(),
            0,
            true,
            &[],
            None,
            MAINNET.genesis_challenge,
            MAINNET.genesis_challenge,
            dg_xch_core::blockchain::pool_target::PoolTarget {
                puzzle_hash: Bytes32::from([1u8; 32]),
                max_height: 0,
            },
            None,
            Bytes32::from([0xDDu8; 32]),
            1_600_000_000,
            b"daemon-emit",
            placeholder,
        )
        .expect("candidate builds");

        // The two hashes the farmer signs, and its real signatures over them (SignedValues).
        let fbd_hash = candidate
            .foliage
            .foliage_block_data
            .hash()
            .expect("fbd hash");
        let ftb_hash = candidate
            .foliage
            .foliage_transaction_block_hash
            .expect("tx block");
        let quality_string = Bytes32::from([0x99u8; 32]);
        let signed = SignedValues {
            quality_string,
            foliage_block_data_signature: sign(&sk, fbd_hash.as_ref()).into(),
            foliage_transaction_block_signature: sign(&sk, ftb_hash.as_ref()).into(),
        };

        let ub_inbox = Arc::new(Mutex::new(Vec::new()));
        let candidates = Arc::new(Mutex::new(CandidateBlockStore::default()));
        candidates.lock().await.insert(quality_string, 0, candidate);
        let api = StoreApi {
            store,
            mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
            constants: MAINNET,
            claimed_peak: Arc::new(AtomicU32::new(0)),
            peak_book: Arc::new(PeakBook::new(Arc::new(AtomicU32::new(0)))),
            claim_guard: None,
            new_peak_signal: Arc::new(Notify::new()),
            known_peers: Arc::new(RwLock::new(Vec::new())),
            tx_requested: Arc::new(Mutex::new(HashMap::new())),
            slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
            sp_inbox: Arc::new(Mutex::new(Vec::new())),
            unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
            ub_inbox: ub_inbox.clone(),
            ip_inbox: Arc::new(Mutex::new(Vec::new())),
            synced: Arc::new(AtomicBool::new(true)),
            wallet_compat: Arc::new(AtomicBool::new(false)),
            tx_inbox: Arc::new(Mutex::new(TxQueue::new(
                TX_INBOX_CAP,
                TX_INBOX_PER_PEER,
                MAINNET.max_block_cost_clvm / 2,
            ))),
            tx_announce: Arc::new(Mutex::new(Vec::new())),
            tx_origin: Arc::new(Mutex::new(HashMap::new())),
            wp_inbox: Arc::new(Mutex::new(Vec::new())),
            compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
            proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
            candidates,
            producer: Arc::new(ProducerMetrics::default()),
            farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
            wallet: Arc::new(WalletNotifier::new()),
            trust: Arc::new(TrustPolicy::default()),
            wallet_sync_sem: Arc::new(LimitedSemaphore::new(
                WALLET_SYNC_ACTIVE_LIMIT,
                WALLET_SYNC_WAITING_LIMIT,
            )),
            record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
            sync_metrics: Arc::new(SyncMetrics::default()),
        };

        api.on_signed_values(Bytes32::default(), signed.clone())
            .await;

        // The finished block landed in ub_inbox with the REAL (non-placeholder) farmer signatures.
        let queued = ub_inbox.lock().await;
        assert_eq!(queued.len(), 1, "one finished block queued for broadcast");
        let block = &queued[0];
        assert_eq!(
            block.foliage.foliage_block_data_signature, signed.foliage_block_data_signature,
            "fbd signature spliced"
        );
        assert_eq!(
            block.foliage.foliage_transaction_block_signature,
            Some(signed.foliage_transaction_block_signature),
            "ftb signature spliced for a tx block"
        );
        assert_ne!(
            block.foliage.foliage_block_data_signature,
            g2_infinity(),
            "no longer the placeholder"
        );

        // A bad fbd signature (wrong key) is rejected: nothing new queued.
        let wrong_sk = SecretKey::key_gen_v3(&[0xA5u8; 32], &[]).expect("sk2");
        let bad = SignedValues {
            quality_string,
            foliage_block_data_signature: sign(&wrong_sk, fbd_hash.as_ref()).into(),
            foliage_transaction_block_signature: Bytes96::from([0u8; 96]),
        };
        drop(queued);
        api.on_signed_values(Bytes32::default(), bad).await;
        assert_eq!(
            ub_inbox.lock().await.len(),
            1,
            "wrong-key signature rejected: no additional block queued"
        );
    }

    #[test]
    fn mempool_payload_gate_matches_chia() {
        // All gates pass: a tx-block candidate, no coercion, mempool frame == prev tx block.
        assert!(may_build_transactions(true, false, Some(100), 100));
        // A non-transaction candidate never carries transactions.
        assert!(!may_build_transactions(false, false, Some(100), 100));
        // The empty-block coercion fired (candidate SP at/before the tx-peak window).
        assert!(!may_build_transactions(true, true, Some(100), 100));
        // Mempool frame lags the candidate's prev tx block (mid-reorg / stale revalidation).
        assert!(!may_build_transactions(true, false, Some(99), 100));
        // Pre-genesis: no mempool frame at all — fails closed.
        assert!(!may_build_transactions(true, false, None, 0));
    }

    // Chain (header_hash = [h;32]): h0 genesis tx + first-in-sub-slot; h1 non-tx; h2 tx; h3 non-tx peak.
    #[tokio::test]
    async fn candidate_store_walks_match_chia() {
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;

        fn rec(
            height: u32,
            timestamp: Option<u64>,
            first_slot_cc: Option<Bytes32>,
            total_iters: u128,
            rin: u8,
            fees: Option<u64>,
        ) -> BlockRecord {
            BlockRecord {
                header_hash: Bytes32::from([height as u8; 32]),
                prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
                height,
                weight: u128::from(height),
                total_iters,
                signage_point_index: 0,
                challenge_vdf_output: ClassgroupElement::get_default_element(),
                infused_challenge_vdf_output: None,
                reward_infusion_new_challenge: Bytes32::from([rin; 32]),
                challenge_block_info_hash: Bytes32::default(),
                sub_slot_iters: MAINNET.sub_slot_iters_starting,
                pool_puzzle_hash: Bytes32::from([0xB0 + height as u8; 32]),
                farmer_puzzle_hash: Bytes32::from([0xF0 + height as u8; 32]),
                required_iters: 1,
                deficit: 0,
                overflow: false,
                prev_transaction_block_height: 0,
                timestamp,
                prev_transaction_block_hash: None,
                fees,
                reward_claims_incorporated: None,
                finished_challenge_slot_hashes: first_slot_cc.map(|c| vec![c]),
                finished_infused_challenge_slot_hashes: None,
                finished_reward_slot_hashes: None,
                sub_epoch_summary_included: None,
            }
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("fn_walks_{}_{nanos}.sqlite", std::process::id()));
        let store = open_backend(&Backend::Sqlite(path))
            .await
            .expect("open store");

        let future_ts = now_secs() + 1_000_000;
        let h0 = rec(
            0,
            Some(500),
            Some(Bytes32::from([0xC0; 32])),
            100,
            0xE0,
            Some(9),
        );
        let h1 = rec(1, None, None, 150, 0xE1, None);
        let h2 = rec(2, Some(future_ts), None, 200, 0xE2, Some(50));
        let h3 = rec(3, None, Some(Bytes32::from([0xC3; 32])), 250, 0xE3, None);
        store
            .add_block_records(&[h0.clone(), h1.clone(), h2.clone(), h3.clone()])
            .await
            .expect("seed records");

        // Reward-chain backtrack: an exact reward_infusion match returns that block; a deeper match walks.
        let found = backtrack_prev_block(store.as_ref(), h3.clone(), Bytes32::from([0xE3; 32]))
            .await
            .expect("found")
            .expect("prev present");
        assert_eq!(found.height, 3, "first-hop reward_infusion match");
        let deep = backtrack_prev_block(store.as_ref(), h3.clone(), Bytes32::from([0xE1; 32]))
            .await
            .expect("found")
            .expect("prev present");
        assert_eq!(deep.height, 1, "backtrack walks to the matching block");

        // challenge_in_chain: h3 is itself first-in-sub-slot; h2 walks back to h0's finished challenge.
        assert_eq!(
            challenge_in_chain(store.as_ref(), &h3).await,
            Some(Bytes32::from([0xC3; 32]))
        );
        assert_eq!(
            challenge_in_chain(store.as_ref(), &h2).await,
            Some(Bytes32::from([0xC0; 32]))
        );

        // Prev linkage + reward-claim walk: prev tx block h2 (with its fees) then the non-tx h1 (fees 0).
        let linkage = resolve_prev_linkage(store.as_ref(), &MAINNET, &h3, 300)
            .await
            .expect("linkage");
        assert!(linkage.is_transaction_block, "sp total-iters past h2 => tx");
        assert_eq!(linkage.prev_block_hash, h3.header_hash);
        assert_eq!(linkage.prev_transaction_block_hash, h2.header_hash);
        assert_eq!(
            linkage.prev_transaction_block_height, 2,
            "the produce-path mempool gate keys on the prev tx block's height"
        );
        assert_eq!(linkage.reward_claims.len(), 2);
        assert_eq!(linkage.reward_claims[0].height, 2);
        assert_eq!(
            linkage.reward_claims[0].fees, 50,
            "prev tx block keeps its fees"
        );
        assert_eq!(linkage.reward_claims[1].height, 1);
        assert_eq!(
            linkage.reward_claims[1].fees, 0,
            "intermediate non-tx blocks: fees 0"
        );

        // Below the prev tx block's total-iters => not a tx block, no claims.
        let non_tx = resolve_prev_linkage(store.as_ref(), &MAINNET, &h3, 150)
            .await
            .expect("linkage");
        assert!(!non_tx.is_transaction_block);
        assert!(non_tx.reward_claims.is_empty());
        assert_eq!(
            non_tx.prev_transaction_block_height, 2,
            "a non-tx candidate still records the true prev tx block height"
        );

        // Timestamp is bumped strictly past the previous transaction block (h2's future timestamp).
        assert_eq!(
            candidate_timestamp(store.as_ref(), &h3).await,
            future_ts + 1,
            "timestamp > prev transaction block"
        );
    }

    // Build a genesis-style unfinished block (prev_block_hash == pos sub-slot cc challenge ==
    // GENESIS_CHALLENGE, signage-point index 0) via the same producer path the live emit uses. This is the
    // block a timelord's index-0 infusion point finishes into the genesis FullBlock.
    fn genesis_unfinished_block() -> UnfinishedBlock {
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
        use dg_xch_core::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
        use dg_xch_core::blockchain::sized_bytes::Bytes48;
        use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
        use dg_xch_core::blockchain::vdf_info::VdfInfo;
        use dg_xch_core::blockchain::vdf_proof::VdfProof;
        use dg_xch_core::consensus::producer::{
            FarmerSignatures, create_unfinished_block_with_sigs, g2_infinity,
        };
        let pos = ProofOfSpace {
            version: 0,
            plot_index: 0,
            meta_group: 0,
            strength: 0,
            challenge: MAINNET.genesis_challenge,
            pool_public_key: None,
            pool_contract_puzzle_hash: Some(Bytes32::from([2u8; 32])),
            plot_public_key: Bytes48::from([3u8; 48]),
            size: 32,
            proof: ProofBytes::from(vec![7u8; 64]),
        };
        let vdf = |c: u8, n: u64| VdfInfo {
            challenge: Bytes32::from([c; 32]),
            number_of_iterations: n,
            output: ClassgroupElement::get_default_element(),
        };
        let proof = |w: u8| VdfProof {
            witness_type: w,
            witness: UnsizedBytes::new(vec![0xAA]),
            normalized_to_identity: true,
        };
        let placeholder = FarmerSignatures {
            challenge_chain_sp_signature: g2_infinity(),
            reward_chain_sp_signature: g2_infinity(),
            foliage_block_data_signature: g2_infinity(),
            foliage_transaction_block_signature: g2_infinity(),
        };
        create_unfinished_block_with_sigs(
            &MAINNET,
            0,
            0,
            pos,
            MAINNET.genesis_challenge,
            Some(vdf(0x10, 1)),
            Some(proof(1)),
            Some(vdf(0x11, 2)),
            Some(proof(2)),
            Vec::new(),
            0,
            true,
            &[],
            None,
            MAINNET.genesis_challenge,
            MAINNET.genesis_challenge,
            dg_xch_core::blockchain::pool_target::PoolTarget {
                puzzle_hash: MAINNET.genesis_pre_farm_pool_puzzle_hash,
                max_height: 0,
            },
            None,
            Bytes32::from([0xDDu8; 32]),
            1_600_000_000,
            b"infusion-genesis",
            placeholder,
        )
        .expect("genesis unfinished block builds")
    }

    #[tokio::test]
    async fn infusion_return_handlers_queue_only_when_synced() {
        use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
        use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
        use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
        use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
        use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
        use dg_xch_core::blockchain::vdf_info::VdfInfo;
        use dg_xch_core::blockchain::vdf_proof::VdfProof;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("fn_ipdisp_{}_{nanos}.sqlite", std::process::id()));
        let store = open_backend(&Backend::Sqlite(path)).await.expect("store");

        let ip_inbox = Arc::new(Mutex::new(Vec::new()));
        let sp_inbox = Arc::new(Mutex::new(Vec::new()));
        let synced = Arc::new(AtomicBool::new(true));
        let make_api = |synced: Arc<AtomicBool>| StoreApi {
            store: store.clone(),
            mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
            constants: MAINNET,
            claimed_peak: Arc::new(AtomicU32::new(0)),
            peak_book: Arc::new(PeakBook::new(Arc::new(AtomicU32::new(0)))),
            claim_guard: None,
            new_peak_signal: Arc::new(Notify::new()),
            known_peers: Arc::new(RwLock::new(Vec::new())),
            tx_requested: Arc::new(Mutex::new(HashMap::new())),
            slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
            sp_inbox: sp_inbox.clone(),
            unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
            ub_inbox: Arc::new(Mutex::new(Vec::new())),
            ip_inbox: ip_inbox.clone(),
            synced,
            wallet_compat: Arc::new(AtomicBool::new(false)),
            tx_inbox: Arc::new(Mutex::new(TxQueue::new(
                TX_INBOX_CAP,
                TX_INBOX_PER_PEER,
                MAINNET.max_block_cost_clvm / 2,
            ))),
            tx_announce: Arc::new(Mutex::new(Vec::new())),
            tx_origin: Arc::new(Mutex::new(HashMap::new())),
            wp_inbox: Arc::new(Mutex::new(Vec::new())),
            compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
            proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
            candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
            producer: Arc::new(ProducerMetrics::default()),
            farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
            wallet: Arc::new(WalletNotifier::new()),
            trust: Arc::new(TrustPolicy::default()),
            wallet_sync_sem: Arc::new(LimitedSemaphore::new(
                WALLET_SYNC_ACTIVE_LIMIT,
                WALLET_SYNC_WAITING_LIMIT,
            )),
            record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
            sync_metrics: Arc::new(SyncMetrics::default()),
        };

        let vdf = |c: u8| VdfInfo {
            challenge: Bytes32::from([c; 32]),
            number_of_iterations: 1,
            output: ClassgroupElement::get_default_element(),
        };
        let proof = VdfProof {
            witness_type: 0,
            witness: UnsizedBytes::default(),
            normalized_to_identity: false,
        };
        let ip = NewInfusionPointVDF {
            unfinished_reward_hash: Bytes32::from([9u8; 32]),
            challenge_chain_ip_vdf: vdf(1),
            challenge_chain_ip_proof: proof.clone(),
            reward_chain_ip_vdf: vdf(2),
            reward_chain_ip_proof: proof.clone(),
            infused_challenge_chain_ip_vdf: None,
            infused_challenge_chain_ip_proof: None,
        };
        let sp = NewSignagePointVDF {
            index_from_challenge: 5,
            challenge_chain_sp_vdf: vdf(3),
            challenge_chain_sp_proof: proof.clone(),
            reward_chain_sp_vdf: vdf(4),
            reward_chain_sp_proof: proof.clone(),
        };
        let eos_bundle = EndOfSubSlotBundle {
            challenge_chain: ChallengeChainSubSlot {
                challenge_chain_end_of_slot_vdf: vdf(5),
                infused_challenge_chain_sub_slot_hash: None,
                subepoch_summary_hash: None,
                new_sub_slot_iters: None,
                new_difficulty: None,
            },
            infused_challenge_chain: None,
            reward_chain: RewardChainSubSlot {
                end_of_slot_vdf: vdf(6),
                challenge_chain_sub_slot_hash: Bytes32::from([7u8; 32]),
                infused_challenge_chain_sub_slot_hash: None,
                deficit: 0,
            },
            proofs: SubSlotProofs {
                challenge_chain_slot_proof: proof.clone(),
                infused_challenge_chain_slot_proof: None,
                reward_chain_slot_proof: proof,
            },
        };
        let eos = NewEndOfSubSlotVDF {
            end_of_sub_slot_bundle: eos_bundle,
        };

        // Synced: all three queue for the driver.
        let api = make_api(synced.clone());
        api.on_new_infusion_point_vdf(Bytes32::default(), ip.clone())
            .await;
        api.on_new_signage_point_vdf(Bytes32::default(), sp.clone())
            .await;
        api.on_new_end_of_sub_slot_vdf(Bytes32::default(), eos.clone())
            .await;
        assert_eq!(
            ip_inbox.lock().await.len(),
            1,
            "infusion point queued when synced"
        );
        assert_eq!(
            sp_inbox.lock().await.len(),
            2,
            "signage-point + end-of-sub-slot VDFs routed to the slot-state inbox when synced"
        );

        // Not synced: all three drop (`if sync_store.get_sync_mode(): return None`).
        ip_inbox.lock().await.clear();
        sp_inbox.lock().await.clear();
        let api = make_api(Arc::new(AtomicBool::new(false)));
        api.on_new_infusion_point_vdf(Bytes32::default(), ip).await;
        api.on_new_signage_point_vdf(Bytes32::default(), sp).await;
        api.on_new_end_of_sub_slot_vdf(Bytes32::default(), eos)
            .await;
        assert!(
            ip_inbox.lock().await.is_empty(),
            "infusion point dropped while syncing"
        );
        assert!(
            sp_inbox.lock().await.is_empty(),
            "SP/EOS dropped while syncing"
        );
    }

    // The in-process infusion assembly (`new_infusion_point_vdf`, the load-bearing path):
    // a cached genesis unfinished block + an index-0 infusion point (all GENESIS_CHALLENGE) is finished by
    // `assemble_infusion_block` into exactly the FullBlock `unfinished_block_to_full_block` produces — proving
    // the cache lookup, the genesis rc-backtrack (prev_b = None), the empty finished-sub-slot collection, the
    // genesis sub-slot-start iters (0), and the assembly all wire together against a populated SlotState.
    // The final engine peak-set on a REAL block is proven byte-identically by the core fixture reconstruction
    // (unfinished_to_full_block_reconstruct.rs) — a fake-VDF genesis block cannot clear live consensus in-test,
    // so this asserts the assembly, and process_ip_inbox is exercised to prove the full drive path runs.
    #[tokio::test]
    async fn infusion_point_finishes_cached_genesis_unfinished_block() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db =
            std::env::temp_dir().join(format!("fn_ipasm_{}_{nanos}.sqlite", std::process::id()));
        let node = Arc::new(
            Node::boot(Config {
                p2p: P2pSettings::default(),
                listen: "127.0.0.1:0".parse().unwrap(),
                rpc: "127.0.0.1:0".parse().unwrap(),
                introducer: None,
                manual_peers: Vec::new(),
                advertise: None,
                backend: Backend::Sqlite(db),
                network_id: "mainnet".to_string(),
                metrics: None,
                capture_dir: None,
                genesis_sync: false,
                sync_from: 0,
                uncompact: false,
                prefetch_memory_mb: None,
                prefetch_max_inflight: None,
                trusted_peers: Vec::new(),
                trusted_cidrs: Vec::new(),
                rpc_tls: crate::config::RpcTlsMode::Local,
                debug_endpoints: false,
            })
            .await
            .expect("boot node"),
        );

        let ub = genesis_unfinished_block();
        let partial_hash = ub.reward_chain_block.hash().expect("reward hash");
        node.unfinished
            .lock()
            .await
            .add_block(partial_hash, 0, ub.clone(), 1);

        // An index-0 infusion point whose challenges are all GENESIS_CHALLENGE: the rc backtrack is the
        // identity on the fresh (genesis-only) SlotState, so target_rc_hash == GENESIS ⇒ prev_b = None;
        // last_slot_cc_hash == GENESIS == challenge_in_chain ⇒ finished_sub_slots == [].
        let genesis = MAINNET.genesis_challenge;
        let ip_vdf = |n: u64| dg_xch_core::blockchain::vdf_info::VdfInfo {
            challenge: genesis,
            number_of_iterations: n,
            output:
                dg_xch_core::blockchain::class_group_element::ClassgroupElement::get_default_element(
                ),
        };
        let ip_proof = |w: u8| dg_xch_core::blockchain::vdf_proof::VdfProof {
            witness_type: w,
            witness: dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(vec![0xBB]),
            normalized_to_identity: true,
        };
        let req = NewInfusionPointVDF {
            unfinished_reward_hash: partial_hash,
            challenge_chain_ip_vdf: ip_vdf(100),
            challenge_chain_ip_proof: ip_proof(1),
            reward_chain_ip_vdf: ip_vdf(200),
            reward_chain_ip_proof: ip_proof(2),
            infused_challenge_chain_ip_vdf: None,
            infused_challenge_chain_ip_proof: None,
        };

        let assembled = assemble_infusion_block(&node, &req)
            .await
            .expect("genesis infusion assembles a FullBlock");

        // It must equal the independent unfinished_block_to_full_block construction (prev None ⇒ genesis
        // tx block, height 0, weight == difficulty_starting, empty finished sub-slots).
        let expected = unfinished_block_to_full_block(
            &ub,
            req.challenge_chain_ip_vdf,
            req.challenge_chain_ip_proof.clone(),
            req.reward_chain_ip_vdf,
            req.reward_chain_ip_proof.clone(),
            None,
            None,
            Vec::new(),
            None,
            true,
            MAINNET.difficulty_starting,
        )
        .expect("expected build");
        assert_eq!(
            assembled, expected,
            "assembled infusion block matches the reference construction"
        );
        assert_eq!(assembled.reward_chain_block.height, 0, "genesis height");
        assert_eq!(
            assembled.reward_chain_block.weight,
            u128::from(MAINNET.difficulty_starting),
            "genesis weight == difficulty_starting"
        );
        assert!(
            assembled.reward_chain_block.is_transaction_block,
            "genesis block is a transaction block"
        );
        // The reward-chain infusion VDFs are the timelord's, spliced into the finished reward block.
        assert_eq!(
            assembled.reward_chain_block.challenge_chain_ip_vdf,
            req.challenge_chain_ip_vdf
        );
        assert_eq!(
            assembled.reward_chain_block.reward_chain_ip_vdf,
            req.reward_chain_ip_vdf
        );
        // The foliage's reward_block_hash was re-derived from the finished reward block.
        assert_eq!(
            assembled.foliage.reward_block_hash,
            assembled
                .reward_chain_block
                .hash()
                .expect("finished reward hash"),
            "foliage commits the finished reward block hash"
        );

        // Drive the full inbox path once: process_ip_inbox drains the queue, assembles, and routes to the
        // engine (a fake-VDF genesis block is rejected by consensus — logged, no panic). The proof here is
        // that the drive path runs to completion and the inbox is drained.
        node.ip_inbox.lock().await.push(req);
        let registry: Arc<dyn OutboundPeers> =
            Arc::new(dg_xch_p2p::PeerRegistry::new(P2pSettings::default()));
        let inbound: PeerMap = Arc::new(RwLock::new(HashMap::new()));
        process_ip_inbox(&node, &registry, &inbound).await;
        assert!(
            node.ip_inbox.lock().await.is_empty(),
            "process_ip_inbox drained the infusion inbox"
        );
    }

    struct FaultStore {
        inner: Arc<SqliteStore>,
        fail_get_block_record: Arc<AtomicBool>,
        // Edge-controller probes: how many times the daemon fired the deferred build / the
        // falling-edge shed. Counted at the store seam so the tests observe the spawned tasks'
        // actual store calls, not the latch state.
        build_calls: Arc<std::sync::atomic::AtomicUsize>,
        shed_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FaultStore {
        fn new(inner: Arc<SqliteStore>, fail_get_block_record: Arc<AtomicBool>) -> Self {
            Self {
                inner,
                fail_get_block_record,
                build_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                shed_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl dg_xch_stores::CoinStore for FaultStore {
        async fn get_coin_record(
            &self,
            coin_name: &Bytes32,
        ) -> Result<Option<CoinRecord>, dg_xch_stores::StoreError> {
            self.inner.get_coin_record(coin_name).await
        }
        async fn get_coin_records(
            &self,
            names: &[Bytes32],
        ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
            self.inner.get_coin_records(names).await
        }
        #[cfg(any(feature = "coin-index", test))]
        async fn batch_coin_states_by_puzzle_hashes(
            &self,
            puzzle_hashes: &[Bytes32],
            min_height: u32,
            filters: &dg_xch_core::protocols::wallet::CoinStateFilters,
            max_items: usize,
        ) -> Result<
            (Vec<dg_xch_core::protocols::wallet::CoinState>, Option<u32>),
            dg_xch_stores::StoreError,
        > {
            self.inner
                .batch_coin_states_by_puzzle_hashes(puzzle_hashes, min_height, filters, max_items)
                .await
        }
        async fn apply_block(
            &self,
            height: u32,
            timestamp: u64,
            additions: &[CoinRecord],
            removals: &[Bytes32],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner
                .apply_block(height, timestamp, additions, removals)
                .await
        }
        async fn apply_block_in(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            height: u32,
            timestamp: u64,
            additions: &[CoinRecord],
            removals: &[Bytes32],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner
                .apply_block_in(batch, height, timestamp, additions, removals)
                .await
        }
        async fn rollback_to(&self, fork_height: u32) -> Result<u64, dg_xch_stores::StoreError> {
            self.inner.rollback_to(fork_height).await
        }
        async fn rollback_to_in(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            fork_height: u32,
        ) -> Result<u64, dg_xch_stores::StoreError> {
            self.inner.rollback_to_in(batch, fork_height).await
        }
        #[cfg(any(feature = "coin-index", test))]
        async fn get_unspent_by_puzzle_hash(
            &self,
            ph: &Bytes32,
        ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
            self.inner.get_unspent_by_puzzle_hash(ph).await
        }
        #[cfg(any(feature = "coin-index", test))]
        async fn get_coins_by_parent(
            &self,
            parent: &Bytes32,
        ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
            self.inner.get_coins_by_parent(parent).await
        }
        #[cfg(any(feature = "coin-index", test))]
        async fn get_coins_added_at_height(
            &self,
            height: u32,
        ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
            self.inner.get_coins_added_at_height(height).await
        }
        #[cfg(any(feature = "coin-index", test))]
        async fn get_coins_removed_at_height(
            &self,
            height: u32,
        ) -> Result<Vec<CoinRecord>, dg_xch_stores::StoreError> {
            self.inner.get_coins_removed_at_height(height).await
        }
        async fn apply_hints_in(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            pairs: &[(Bytes32, Bytes32)],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.apply_hints_in(batch, pairs).await
        }
        async fn apply_hints(
            &self,
            pairs: &[(Bytes32, Bytes32)],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.apply_hints(pairs).await
        }
        #[cfg(feature = "hint")]
        async fn get_coins_for_hint(
            &self,
            hint: &Bytes32,
            max_items: usize,
        ) -> Result<Vec<Bytes32>, dg_xch_stores::StoreError> {
            self.inner.get_coins_for_hint(hint, max_items).await
        }
        #[cfg(any(feature = "coin-index", test))]
        async fn get_coin_states_by_puzzle_hashes(
            &self,
            puzzle_hashes: &[Bytes32],
            min_height: u32,
            include_spent: bool,
            max_items: usize,
        ) -> Result<Vec<dg_xch_core::protocols::wallet::CoinState>, dg_xch_stores::StoreError>
        {
            self.inner
                .get_coin_states_by_puzzle_hashes(
                    puzzle_hashes,
                    min_height,
                    include_spent,
                    max_items,
                )
                .await
        }
    }

    #[async_trait]
    impl dg_xch_stores::BlockStore for FaultStore {
        async fn get_block_record(
            &self,
            hh: &Bytes32,
        ) -> Result<Option<BlockRecord>, dg_xch_stores::StoreError> {
            if self.fail_get_block_record.load(Ordering::Relaxed) {
                return Err(dg_xch_stores::StoreError::Corrupt(
                    "injected get_block_record fault".to_string(),
                ));
            }
            self.inner.get_block_record(hh).await
        }
        async fn get_block_record_by_height(
            &self,
            h: u32,
        ) -> Result<Option<BlockRecord>, dg_xch_stores::StoreError> {
            self.inner.get_block_record_by_height(h).await
        }
        async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, dg_xch_stores::StoreError> {
            self.inner.get_peak().await
        }
        async fn min_record_height(&self) -> Result<Option<u32>, dg_xch_stores::StoreError> {
            self.inner.min_record_height().await
        }
        async fn get_block(
            &self,
            hh: &Bytes32,
        ) -> Result<Option<dg_xch_core::blockchain::full_block::FullBlock>, dg_xch_stores::StoreError>
        {
            self.inner.get_block(hh).await
        }
        async fn add_block_records(
            &self,
            records: &[BlockRecord],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.add_block_records(records).await
        }
        async fn add_block_records_in(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            records: &[BlockRecord],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.add_block_records_in(batch, records).await
        }
        async fn begin(&self) -> Result<dg_xch_stores::BatchHandle, dg_xch_stores::StoreError> {
            self.inner.begin().await
        }
        async fn append_many(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            blocks: &[dg_xch_core::blockchain::full_block::FullBlock],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.append_many(batch, blocks).await
        }
        async fn commit(
            &self,
            batch: dg_xch_stores::BatchHandle,
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.commit(batch).await
        }
        fn near_tip(&self) -> bool {
            self.inner.near_tip()
        }
        fn set_near_tip(&self, near_tip: bool) {
            self.inner.set_near_tip(near_tip);
        }
        async fn get_unassociated(
            &self,
            limit: usize,
        ) -> Result<Vec<u32>, dg_xch_stores::StoreError> {
            self.inner.get_unassociated(limit).await
        }
        async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, dg_xch_stores::StoreError> {
            self.inner.set_peak(new_peak).await
        }
        async fn set_peak_in(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            new_peak: &Bytes32,
        ) -> Result<u64, dg_xch_stores::StoreError> {
            self.inner.set_peak_in(batch, new_peak).await
        }
        async fn get_status(
            &self,
            hh: &Bytes32,
        ) -> Result<dg_xch_stores::BlockStatus, dg_xch_stores::StoreError> {
            self.inner.get_status(hh).await
        }
        async fn set_status(
            &self,
            hh: &Bytes32,
            s: dg_xch_stores::BlockStatus,
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.set_status(hh, s).await
        }
        async fn set_status_in(
            &self,
            batch: &mut dg_xch_stores::BatchHandle,
            hh: &Bytes32,
            s: dg_xch_stores::BlockStatus,
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.set_status_in(batch, hh, s).await
        }
        async fn savepoint(&self) -> Result<dg_xch_stores::Savepoint, dg_xch_stores::StoreError> {
            self.inner.savepoint().await
        }
        async fn rollback(
            &self,
            sp: dg_xch_stores::Savepoint,
        ) -> Result<u64, dg_xch_stores::StoreError> {
            self.inner.rollback(sp).await
        }
        async fn get_generator_at_height(
            &self,
            h: u32,
        ) -> Result<Option<dg_xch_core::clvm::program::SerializedProgram>, dg_xch_stores::StoreError>
        {
            self.inner.get_generator_at_height(h).await
        }
        async fn get_sub_epoch_segments(
            &self,
            ses_hash: &Bytes32,
        ) -> Result<Option<Vec<u8>>, dg_xch_stores::StoreError> {
            self.inner.get_sub_epoch_segments(ses_hash).await
        }
        async fn persist_sub_epoch_segments(
            &self,
            ses_hash: &Bytes32,
            bytes: &[u8],
        ) -> Result<(), dg_xch_stores::StoreError> {
            self.inner.persist_sub_epoch_segments(ses_hash, bytes).await
        }
        async fn build_indexes(&self) -> Result<(), dg_xch_stores::StoreError> {
            self.build_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.build_indexes().await
        }
        async fn shed_service_indexes(&self) -> Result<(), dg_xch_stores::StoreError> {
            self.shed_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.shed_service_indexes().await
        }
    }

    // A STORE ERROR resolving an unfinished block's parent must NOT be misclassified as "we are
    // behind" and lost. Treating any `Err` like a missing parent drops the candidate and
    // `remove_requesting`s it, which loses our OWN winning candidate on a DB hiccup. The
    // candidate is re-queued instead and never counted as `ub_prev_unknown`.
    #[tokio::test]
    async fn ub_store_error_requeues_candidate_never_counts_it_as_prev_unknown() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db =
            std::env::temp_dir().join(format!("fn_ubfault_{}_{nanos}.sqlite", std::process::id()));
        let inner = open_backend(&Backend::Sqlite(db)).await.expect("store");
        let fail = Arc::new(AtomicBool::new(false));
        let store = Arc::new(FaultStore::new(inner, fail.clone()));
        let node = Arc::new(
            Node::boot_with_store(
                Config {
                    p2p: P2pSettings::default(),
                    listen: "127.0.0.1:0".parse().unwrap(),
                    rpc: "127.0.0.1:0".parse().unwrap(),
                    introducer: None,
                    manual_peers: Vec::new(),
                    advertise: None,
                    backend: Backend::Sqlite(std::path::PathBuf::from("unused")),
                    network_id: "mainnet".to_string(),
                    metrics: None,
                    capture_dir: None,
                    genesis_sync: false,
                    sync_from: 0,
                    uncompact: false,
                    prefetch_memory_mb: None,
                    prefetch_max_inflight: None,
                    trusted_peers: Vec::new(),
                    trusted_cidrs: Vec::new(),
                    rpc_tls: crate::config::RpcTlsMode::Local,
                    debug_endpoints: false,
                },
                store,
            )
            .expect("boot node with fault store"),
        );

        // A ready unfinished block sits in the inbox; the parent lookup is the first store touch it hits.
        let ub = genesis_unfinished_block();
        node.ub_inbox.lock().await.push(ub);

        // Arm the fault: the parent lookup now errors (transient backend outage), even though on a healthy
        // store this parent would resolve.
        fail.store(true, Ordering::Relaxed);
        process_ub_inbox(&node).await;

        // The candidate was PRESERVED: put back on the inbox for a retry, NOT dropped.
        assert_eq!(
            node.ub_inbox.lock().await.len(),
            1,
            "store error must re-queue the candidate, not lose it"
        );
        // A store error is NEVER the same event as a genuine 'we are behind' miss.
        assert_eq!(
            node.producer.dropped_count("ub_prev_unknown"),
            0,
            "a store error must not be counted as ub_prev_unknown"
        );
        assert_eq!(
            node.producer.requeued_count("ub_prev_store_error"),
            1,
            "the re-queue must be recorded under its own reason"
        );

        // With the backend recovered, the same candidate now resolves its (absent) genesis parent as a
        // genuine miss — the correct 'we are behind' park — proving the retry path is real, not a black hole.
        fail.store(false, Ordering::Relaxed);
        process_ub_inbox(&node).await;
        assert_eq!(
            node.ub_inbox.lock().await.len(),
            0,
            "recovered store drains the re-queued candidate"
        );
        assert_eq!(
            node.producer.dropped_count("ub_prev_unknown"),
            1,
            "genesis parent absent on a healthy store is the genuine ub_prev_unknown park"
        );
    }

    // The two-edge index-phase controller in update_synced. Rising edge (not-synced -> synced):
    // build the deferred secondary indexes once. Falling edge (deep behind: tip_lag past the
    // hysteresis band): shed them once, so re-catch-up runs index-lean with HOT spends, and
    // re-arm the build latch so the next tip edge rebuilds. A shallow dip (a few blocks, a
    // restart wobble) must never churn a multi-GB drop/rebuild — only depth past
    // SHED_TIP_LAG_BLOCKS sheds.
    #[tokio::test]
    async fn deep_fall_behind_sheds_indexes_once_and_the_tip_edge_rebuilds() {
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;

        // A minimal main-chain record at `height` whose transaction-block timestamp is `ts` —
        // chain_is_current derives synced solely from that timestamp's freshness.
        fn rec_at(height: u32, ts: u64) -> BlockRecord {
            BlockRecord {
                header_hash: Bytes32::from([height as u8; 32]),
                prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
                height,
                weight: u128::from(height) * 100,
                total_iters: u128::from(height),
                signage_point_index: 0,
                challenge_vdf_output: ClassgroupElement::get_default_element(),
                infused_challenge_vdf_output: None,
                reward_infusion_new_challenge: Bytes32::default(),
                challenge_block_info_hash: Bytes32::default(),
                sub_slot_iters: MAINNET.sub_slot_iters_starting,
                pool_puzzle_hash: Bytes32::default(),
                farmer_puzzle_hash: Bytes32::default(),
                required_iters: 1,
                deficit: 0,
                overflow: false,
                prev_transaction_block_height: 0,
                timestamp: Some(ts),
                prev_transaction_block_hash: None,
                fees: None,
                reward_claims_incorporated: None,
                finished_challenge_slot_hashes: None,
                finished_infused_challenge_slot_hashes: None,
                finished_reward_slot_hashes: None,
                sub_epoch_summary_included: None,
            }
        }

        async fn confirm_at<S: dg_xch_stores::BlockStore + dg_xch_stores::CoinStore>(
            store: &S,
            height: u32,
            ts: u64,
        ) {
            let rec = rec_at(height, ts);
            store
                .add_block_records(std::slice::from_ref(&rec))
                .await
                .expect("record");
            store.set_peak(&rec.header_hash).await.expect("peak");
        }

        async fn wait_count(counter: &Arc<std::sync::atomic::AtomicUsize>, want: usize) -> bool {
            for _ in 0..200 {
                if counter.load(Ordering::Relaxed) == want {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            counter.load(Ordering::Relaxed) == want
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db =
            std::env::temp_dir().join(format!("fn_idxphase_{}_{nanos}.sqlite", std::process::id()));
        let inner = open_backend(&Backend::Sqlite(db)).await.expect("store");
        let store = Arc::new(FaultStore::new(inner, Arc::new(AtomicBool::new(false))));
        let node = Arc::new(
            Node::boot_with_store(
                Config {
                    p2p: P2pSettings::default(),
                    listen: "127.0.0.1:0".parse().unwrap(),
                    rpc: "127.0.0.1:0".parse().unwrap(),
                    introducer: None,
                    manual_peers: Vec::new(),
                    advertise: None,
                    backend: Backend::Sqlite(std::path::PathBuf::from("unused")),
                    network_id: "mainnet".to_string(),
                    metrics: None,
                    capture_dir: None,
                    genesis_sync: false,
                    sync_from: 0,
                    uncompact: false,
                    prefetch_memory_mb: None,
                    prefetch_max_inflight: None,
                    trusted_peers: Vec::new(),
                    trusted_cidrs: Vec::new(),
                    rpc_tls: crate::config::RpcTlsMode::Local,
                    debug_endpoints: false,
                },
                store.clone(),
            )
            .expect("boot node"),
        );
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };

        // Rising edge at tip: the deferred build fires exactly once.
        confirm_at(&*store, 100, now()).await;
        node.claimed_peak.store(100, Ordering::Relaxed);
        node.update_synced().await;
        assert!(
            wait_count(&store.build_calls, 1).await,
            "the sync->tip rising edge fires the deferred index build"
        );

        // A shallow fall (stale tip, a few blocks behind) must NOT shed — hysteresis.
        confirm_at(&*store, 110, now() - 3_600).await;
        node.claimed_peak.store(120, Ordering::Relaxed);
        for _ in 0..3 {
            node.update_synced().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            store.shed_calls.load(Ordering::Relaxed),
            0,
            "a shallow dip below tip must never churn the index set"
        );

        // A DEEP fall (tip_lag past the hysteresis band): shed fires exactly once, off the
        // follow path, no matter how often update_synced re-observes the phase.
        node.claimed_peak.store(110 + 50_001, Ordering::Relaxed);
        node.update_synced().await;
        assert!(
            wait_count(&store.shed_calls, 1).await,
            "the deep falling edge sheds the secondary indexes"
        );
        for _ in 0..3 {
            node.update_synced().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            store.shed_calls.load(Ordering::Relaxed),
            1,
            "the shed is one-shot per falling edge"
        );

        // Back at tip: the rising edge re-fires the build (the shed re-armed the build latch).
        confirm_at(&*store, 200, now()).await;
        node.claimed_peak.store(200, Ordering::Relaxed);
        node.update_synced().await;
        assert!(
            wait_count(&store.build_calls, 2).await,
            "the next tip edge rebuilds what the shed dropped"
        );
    }

    #[test]
    fn index_zero_eos_carries_sub_slot_source_data() {
        use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
        use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
        use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
        use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
        use dg_xch_core::blockchain::vdf_info::VdfInfo;
        use dg_xch_core::blockchain::vdf_proof::VdfProof;

        let vdf = VdfInfo {
            challenge: Bytes32::from([1u8; 32]),
            number_of_iterations: 1,
            output: ClassgroupElement::get_default_element(),
        };
        let proof = VdfProof {
            witness_type: 0,
            witness: UnsizedBytes::default(),
            normalized_to_identity: false,
        };
        let eos = EndOfSubSlotBundle {
            challenge_chain: ChallengeChainSubSlot {
                challenge_chain_end_of_slot_vdf: vdf,
                infused_challenge_chain_sub_slot_hash: None,
                subepoch_summary_hash: None,
                new_sub_slot_iters: None,
                new_difficulty: None,
            },
            infused_challenge_chain: None,
            reward_chain: RewardChainSubSlot {
                end_of_slot_vdf: vdf,
                challenge_chain_sub_slot_hash: Bytes32::from([2u8; 32]),
                infused_challenge_chain_sub_slot_hash: None,
                deficit: 0,
            },
            proofs: SubSlotProofs {
                challenge_chain_slot_proof: proof.clone(),
                infused_challenge_chain_slot_proof: None,
                reward_chain_slot_proof: proof,
            },
        };
        let sp = farmer_announce_for_eos(&eos, 100, 200, 9, 8).expect("eos hashes");
        assert_eq!(sp.signage_point_index, 0);
        let src = sp
            .sp_source_data
            .as_ref()
            .expect("sp_source_data populated at index 0");
        assert!(
            src.sub_slot_data.is_some(),
            "index 0 must carry sub_slot_data"
        );
        assert!(src.vdf_data.is_none(), "index 0 must NOT carry vdf_data");
        let ss = src.sub_slot_data.as_ref().unwrap();
        assert_eq!(ss.cc_sub_slot, eos.challenge_chain);
        assert_eq!(ss.rc_sub_slot, eos.reward_chain);
        let bytes = sp.to_bytes(ChiaProtocolVersion::Chia0_0_37).unwrap();
        let back = NewSignagePoint::from_bytes(
            &mut std::io::Cursor::new(bytes.as_slice()),
            ChiaProtocolVersion::Chia0_0_37,
        )
        .unwrap();
        assert_eq!(back, sp);
    }

    #[test]
    fn near_tip_band_matches_chia_short_sync_threshold() {
        // No confirmed peak: never the near-tip band (from-zero catch-up is the bulk/batch job).
        assert!(!in_near_tip_band(0, 20, false));
        // At the tip (gap 0): nothing to follow.
        assert!(!in_near_tip_band(100, 100, true));
        assert!(in_near_tip_band(100, 101, true), "1 behind engages");
        assert!(
            in_near_tip_band(100, 120, true),
            "20 behind (the threshold) engages"
        );
        assert!(
            !in_near_tip_band(100, 121, true),
            "21 behind is the batch band"
        );
        assert!(
            !in_near_tip_band(100, 9_160_916, true),
            "far behind is bulk/batch, not near-tip"
        );
        assert_eq!(
            SHORT_SYNC_BLOCKS_BEHIND_THRESHOLD, 20,
            "short_sync_blocks_behind_threshold"
        );
    }

    // Emission contract, farmer leg: a normal-index
    // accepted SP is announced to farmers as NewSignagePoint carrying sp_source_data.vdf_data (the cc/rc
    // SP-VDF outputs), never sub_slot_data, with the SP sub-slot challenge as challenge_hash.
    #[test]
    fn farmer_sp_announce_emits_vdf_source_data() {
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
        use dg_xch_core::blockchain::signage_point::SignagePoint;
        use dg_xch_core::blockchain::vdf_info::VdfInfo;
        let vdf = |c: u8| VdfInfo {
            challenge: Bytes32::from([c; 32]),
            number_of_iterations: 1,
            output: ClassgroupElement::get_default_element(),
        };
        let sp = SignagePoint {
            cc_vdf: Some(vdf(1)),
            cc_proof: None,
            rc_vdf: Some(vdf(2)),
            rc_proof: None,
        };
        let out = farmer_announce_for_sp(&sp, 7, 100, 200, 9, 8).expect("vdfs present");
        assert_eq!(
            out.challenge_hash,
            Bytes32::from([1u8; 32]),
            "cc sub-slot challenge"
        );
        assert_eq!(out.signage_point_index, 7);
        let src = out.sp_source_data.expect("sp_source_data populated");
        assert!(src.vdf_data.is_some(), "normal index carries vdf_data");
        assert!(
            src.sub_slot_data.is_none(),
            "normal index has no sub_slot_data"
        );
    }

    // Emission contract, full-node leg:
    // an accepted SP relays to full nodes as NewSignagePointOrEndOfSubSlot keyed on the SP sub-slot
    // challenge + index; an accepted EOS relays at index 0 keyed on the finished sub-slot hash with the
    // previous challenge chained.
    #[test]
    fn full_node_sp_and_eos_announces_carry_chia_fields() {
        use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
        use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
        use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
        use dg_xch_core::blockchain::signage_point::SignagePoint;
        use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
        use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
        use dg_xch_core::blockchain::vdf_info::VdfInfo;
        use dg_xch_core::blockchain::vdf_proof::VdfProof;
        let vdf = |c: u8| VdfInfo {
            challenge: Bytes32::from([c; 32]),
            number_of_iterations: 1,
            output: ClassgroupElement::get_default_element(),
        };
        let sp = SignagePoint {
            cc_vdf: Some(vdf(3)),
            cc_proof: None,
            rc_vdf: Some(vdf(4)),
            rc_proof: None,
        };
        let state = dg_xch_node::slots::SlotState::new(MAINNET);
        let a = announce_for_sp(&state, 9, &sp).expect("vdfs present");
        assert_eq!(a.challenge_hash, Bytes32::from([3u8; 32]));
        assert_eq!(a.index_from_challenge, 9);
        assert_eq!(a.last_rc_infusion, Bytes32::from([4u8; 32]));

        let proof = VdfProof {
            witness_type: 0,
            witness: UnsizedBytes::default(),
            normalized_to_identity: false,
        };
        let eos = EndOfSubSlotBundle {
            challenge_chain: ChallengeChainSubSlot {
                challenge_chain_end_of_slot_vdf: vdf(5),
                infused_challenge_chain_sub_slot_hash: None,
                subepoch_summary_hash: None,
                new_sub_slot_iters: None,
                new_difficulty: None,
            },
            infused_challenge_chain: None,
            reward_chain: RewardChainSubSlot {
                end_of_slot_vdf: vdf(6),
                challenge_chain_sub_slot_hash: Bytes32::from([7u8; 32]),
                infused_challenge_chain_sub_slot_hash: None,
                deficit: 0,
            },
            proofs: SubSlotProofs {
                challenge_chain_slot_proof: proof.clone(),
                infused_challenge_chain_slot_proof: None,
                reward_chain_slot_proof: proof,
            },
        };
        let e = announce_for_eos(&eos).expect("hashable");
        assert_eq!(
            e.index_from_challenge, 0,
            "an EOS announce is index 0 by protocol convention"
        );
        assert_eq!(
            e.prev_challenge_hash,
            Some(Bytes32::from([5u8; 32])),
            "previous challenge chained from the EOS cc VDF"
        );
        assert_eq!(e.challenge_hash, eos.challenge_chain.hash().unwrap());
        assert_eq!(e.last_rc_infusion, Bytes32::from([6u8; 32]));
    }

    // ---- light-wallet query surface, against the real mainnet block 5,000,000 --------------
    // The block is a transaction block with a generator, 275 additions across 50 puzzle hashes, and 301
    // removals — so the served puzzle/solution, additions, removals, and header-block paths run on real
    // wire data, exactly what a light wallet pulls during trusted sync.
    #[cfg(feature = "coin-index")]
    mod wallet_queries {
        use super::*;
        use dg_xch_core::blockchain::block_record::BlockRecord;
        use dg_xch_core::blockchain::full_block::FullBlock;

        const PEAK: u32 = 5_000_000;

        fn fixture_block() -> FullBlock {
            serde_json::from_str(include_str!("../tests/fixtures/full_block_5000000.json"))
                .expect("block fixture")
        }
        fn fixture_peak_record() -> BlockRecord {
            let recs: Vec<BlockRecord> =
                serde_json::from_str(include_str!("../tests/fixtures/block_records.json"))
                    .expect("records fixture");
            recs.into_iter()
                .find(|r| r.height == PEAK)
                .expect("peak record present")
        }
        fn fixture_adds_rems() -> (Vec<CoinRecord>, Vec<CoinRecord>) {
            #[derive(serde::Deserialize)]
            struct AddsRems {
                additions: Vec<CoinRecord>,
                removals: Vec<CoinRecord>,
            }
            let ar: AddsRems =
                serde_json::from_str(include_str!("../tests/fixtures/adds_rems_5000000.json"))
                    .expect("adds_rems fixture");
            (ar.additions, ar.removals)
        }

        async fn store_at_peak() -> Arc<SqliteStore> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("fn_wallet_{}_{nanos}.sqlite", std::process::id()));
            let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
            let rec = fixture_peak_record();
            let block = fixture_block();
            assert_eq!(rec.header_hash, block.header_hash().expect("hh"));
            let (adds, rems) = fixture_adds_rems();
            // Create the removal coins first, then spend them + create the additions at the peak: the
            // coin store's spent_index/confirmed_index come from the apply_block height, so this makes
            // block 5,000,000's real removals resolvable as spent-at-PEAK and its additions as
            // created-at-PEAK.
            store
                .apply_block(PEAK - 1, 0, &rems, &[])
                .await
                .expect("seed removal coins");
            let rem_names: Vec<Bytes32> = rems.iter().map(|r| r.coin.name()).collect();
            store
                .apply_block(PEAK, rec.timestamp.unwrap_or(0), &adds, &rem_names)
                .await
                .expect("apply peak deltas");
            store
                .add_block_records(std::slice::from_ref(&rec))
                .await
                .expect("records");
            let mut batch = store.begin().await.expect("begin");
            store
                .append_many(&mut batch, std::slice::from_ref(&block))
                .await
                .expect("append body");
            store.commit(batch).await.expect("commit");
            store.set_peak(&rec.header_hash).await.expect("set peak");
            store
        }

        fn api(store: Arc<SqliteStore>) -> StoreApi<SqliteStore> {
            api_tuned(
                store,
                Arc::new(WalletNotifier::new()),
                MAX_SUBSCRIBE_RESPONSE_ITEMS,
                Arc::new(LimitedSemaphore::new(
                    WALLET_SYNC_ACTIVE_LIMIT,
                    WALLET_SYNC_WAITING_LIMIT,
                )),
            )
        }

        // An api with test-scale wallet-serve bounds: an injected subscription registry (small caps), an
        // injected initial-state response budget, and an injected wallet-sync semaphore — the production
        // numbers (100k budget, 200k subscriptions) are impractical to seed in a unit test. The response
        // budget becomes an untrusted-everywhere [`TrustPolicy`] (no peer trusted), so behaviour matches
        // the pre-tier default; the trusted-tier tests inject their own policy via [`api_trust`].
        fn api_tuned(
            store: Arc<SqliteStore>,
            wallet: Arc<WalletNotifier>,
            max_subscribe_response_items: usize,
            wallet_sync_sem: Arc<LimitedSemaphore>,
        ) -> StoreApi<SqliteStore> {
            let trust = Arc::new(TrustPolicy::with_caps(
                std::collections::HashSet::new(),
                usize::MAX,
                usize::MAX,
                max_subscribe_response_items,
                max_subscribe_response_items,
            ));
            api_trust(store, wallet, trust, wallet_sync_sem)
        }

        fn api_trust(
            store: Arc<SqliteStore>,
            wallet: Arc<WalletNotifier>,
            trust: Arc<TrustPolicy>,
            wallet_sync_sem: Arc<LimitedSemaphore>,
        ) -> StoreApi<SqliteStore> {
            StoreApi {
                store,
                mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
                constants: MAINNET,
                claimed_peak: Arc::new(AtomicU32::new(0)),
                peak_book: Arc::new(PeakBook::new(Arc::new(AtomicU32::new(0)))),
                claim_guard: None,
                new_peak_signal: Arc::new(Notify::new()),
                known_peers: Arc::new(RwLock::new(Vec::new())),
                tx_requested: Arc::new(Mutex::new(HashMap::new())),
                slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
                sp_inbox: Arc::new(Mutex::new(Vec::new())),
                unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
                ub_inbox: Arc::new(Mutex::new(Vec::new())),
                ip_inbox: Arc::new(Mutex::new(Vec::new())),
                synced: Arc::new(AtomicBool::new(true)),
                wallet_compat: Arc::new(AtomicBool::new(false)),
                tx_inbox: Arc::new(Mutex::new(TxQueue::new(
                    TX_INBOX_CAP,
                    TX_INBOX_PER_PEER,
                    MAINNET.max_block_cost_clvm / 2,
                ))),
                tx_announce: Arc::new(Mutex::new(Vec::new())),
                tx_origin: Arc::new(Mutex::new(HashMap::new())),
                wp_inbox: Arc::new(Mutex::new(Vec::new())),
                compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
                proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
                candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
                producer: Arc::new(ProducerMetrics::default()),
                farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
                wallet,
                trust,
                wallet_sync_sem,
                record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
                sync_metrics: Arc::new(SyncMetrics::default()),
            }
        }

        #[tokio::test]
        async fn puzzle_solution_guards_reject_unknown_unspent_and_wrong_height() {
            let store = store_at_peak().await;
            let (adds, rems) = fixture_adds_rems();
            let api = api(store);

            // Unknown coin: not in the store at all.
            assert!(
                api.puzzle_solution(Bytes32::from([0x13; 32]), PEAK)
                    .await
                    .is_none()
            );

            // Known but UNSPENT coin (an addition at the peak, spent_index == 0): refused before any
            // generator work.
            let unspent = adds[0].coin.name();
            assert!(api.puzzle_solution(unspent, PEAK).await.is_none());

            // A coin spent at the peak, queried at the WRONG height (spent_block_index != height):
            // refused.
            let spent = rems[0].coin.name();
            assert!(api.puzzle_solution(spent, PEAK - 1).await.is_none());
        }

        // RequestBlockHeader: the confirmed block at a height serves its HeaderBlock; an unknown height
        // rejects.
        #[tokio::test]
        async fn block_header_serves_and_rejects() {
            let store = store_at_peak().await;
            let api = api(store);
            match api.block_header(PEAK).await {
                BlockHeaderReply::Respond(hb) => assert_eq!(hb.height(), PEAK),
                _ => panic!("the peak block must serve a header"),
            }
            assert!(matches!(
                api.block_header(PEAK + 500).await,
                BlockHeaderReply::Reject(h) if h == PEAK + 500
            ));
        }

        // G3 closed — the served header carries the block's REAL BIP158 transactions_filter: its
        // sha256 is the foliage filter_hash the wallet validates against, byte-equal to the
        // validation-side builder over the same delta, identical across all three header-serving
        // handlers; return_filter=false serves the encoded-empty b"\x00".
        #[tokio::test]
        async fn served_header_filter_matches_the_blocks_filter_hash() {
            let store = store_at_peak().await;
            let block = fixture_block();
            let ftb = block.foliage_transaction_block.expect("tx block");
            let (adds, rems) = fixture_adds_rems();
            let api = api(store);

            let hb = match api.block_header(PEAK).await {
                BlockHeaderReply::Respond(hb) => hb,
                _ => panic!("the peak header must serve"),
            };
            let filter = hb.transactions_filter.as_slice().to_vec();
            assert_eq!(
                Bytes32::from(dg_xch_core::utils::hash_256(&filter)),
                ftb.filter_hash,
                "sha256(served filter) must equal the foliage filter_hash"
            );
            // Byte-equality with the proven validation-side construction (engine rule 12): every
            // added coin's puzzle hash (incl. reward claims) then every removed coin's name.
            let mut items: Vec<Vec<u8>> = Vec::new();
            for a in &adds {
                items.push(a.coin.puzzle_hash.bytes().to_vec());
            }
            for r in &rems {
                items.push(r.coin.name().bytes().to_vec());
            }
            assert_eq!(
                filter,
                dg_xch_core::consensus::block_filter::chia_block_filter(&items),
                "served filter bytes equal the rule-12 builder's"
            );

            // The two range handlers serve the same filter bytes.
            match api.header_blocks(PEAK, PEAK).await {
                HeaderBlocksReply::Respond(r) => assert_eq!(
                    r.header_blocks[0].transactions_filter.as_slice(),
                    filter.as_slice(),
                    "request_header_blocks serves the same filter"
                ),
                _ => panic!("header_blocks must serve"),
            }
            match api.block_headers(PEAK, PEAK, true).await {
                BlockHeadersReply::Respond(r) => assert_eq!(
                    r.header_blocks[0].transactions_filter.as_slice(),
                    filter.as_slice(),
                    "request_block_headers(return_filter=true) serves the same filter"
                ),
                _ => panic!("block_headers must serve"),
            }
            // return_filter = false: serve the one-byte encoded-empty filter, NOT the real
            // one and NOT a zero-length string (header_block_from_block).
            match api.block_headers(PEAK, PEAK, false).await {
                BlockHeadersReply::Respond(r) => assert_eq!(
                    r.header_blocks[0].transactions_filter.as_slice(),
                    &[0u8],
                    "return_filter=false serves b\"\\x00\""
                ),
                _ => panic!("block_headers must serve"),
            }
        }

        // A non-transaction block's served filter is the encoded-empty b"\x00" (PyBIP158([]) —
        // the same constant the fast path hardcodes), never the real-filter computation.
        #[tokio::test]
        async fn non_transaction_block_serves_the_encoded_empty_filter() {
            let store = store_at_peak().await;
            let api = api(store);
            let mut non_tx = fixture_block();
            non_tx.foliage_transaction_block = None;
            non_tx.transactions_info = None;
            let hb = api
                .served_header_block(&non_tx, true)
                .await
                .expect("serves");
            assert_eq!(hb.transactions_filter.as_slice(), &[0u8]);
        }

        // G2 closed — the specific-puzzle-hashes path serves coins WITH MerkleSet proofs: an
        // inclusion proof for a present hash (plus the hash_coin_ids inclusion proof), an
        // exclusion proof for an absent one, all verifying against the block's REAL foliage
        // additions_root (what the wallet checks them against).
        #[tokio::test]
        async fn additions_serve_proofs_that_verify_against_the_foliage_root() {
            use dg_xch_core::consensus::merkle_set::validate_merkle_proof;
            let store = store_at_peak().await;
            let additions_root = fixture_block()
                .foliage_transaction_block
                .as_ref()
                .expect("tx block")
                .additions_root;
            let (adds, _rems) = fixture_adds_rems();
            let api = api(store);

            let included_ph = adds[0].coin.puzzle_hash;
            let excluded_ph = Bytes32::from([0x13; 32]);
            let req = RequestAdditions {
                height: PEAK,
                header_hash: None,
                puzzle_hashes: Some(vec![included_ph, excluded_ph]),
            };
            let r = match api.additions(req).await {
                AdditionsReply::Respond(r) => r,
                AdditionsReply::Reject(_) => {
                    panic!("a proof-requiring RequestAdditions must serve (G2)")
                }
            };
            let proofs = r.proofs.expect("the proof path carries proofs");
            assert_eq!(proofs.len(), 2, "one proof triple per requested hash");
            let root = additions_root.bytes();

            // Included hash: coins served, both proofs verify as INCLUSION.
            let (ph, proof, coin_proof) = &proofs[0];
            assert_eq!(*ph, included_ph);
            assert_eq!(
                validate_merkle_proof(proof, &ph.bytes(), &root),
                Ok(true),
                "puzzle-hash inclusion proof verifies against the foliage additions_root"
            );
            let served_coins = &r
                .coins
                .iter()
                .find(|(p, _)| *p == included_ph)
                .expect("served entry")
                .1;
            assert!(
                !served_coins.is_empty(),
                "the present hash serves its coins"
            );
            let names: Vec<[u8; 32]> = served_coins.iter().map(|c| c.name().bytes()).collect();
            let coin_ids_hash = hash_coin_ids(&names);
            assert_eq!(
                validate_merkle_proof(
                    coin_proof
                        .as_ref()
                        .expect("inclusion carries the coin-ids proof"),
                    &coin_ids_hash,
                    &root
                ),
                Ok(true),
                "hash_coin_ids inclusion proof verifies"
            );

            // Excluded hash: empty coins, an EXCLUSION proof, no coin-ids proof.
            let (ph_e, proof_e, coin_proof_e) = &proofs[1];
            assert_eq!(*ph_e, excluded_ph);
            assert!(coin_proof_e.is_none());
            assert_eq!(
                validate_merkle_proof(proof_e, &ph_e.bytes(), &root),
                Ok(false),
                "exclusion proof verifies as NOT-in-set against the additions_root"
            );
            let excluded_entry = &r
                .coins
                .iter()
                .find(|(p, _)| *p == excluded_ph)
                .expect("excluded entry present")
                .1;
            assert!(excluded_entry.is_empty(), "an absent hash serves no coins");
        }

        // The empty-puzzle-hashes short-circuit answers proofs=Some([]) — [] (an EMPTY
        // proofs list), not None; a wallet distinguishes the two on
        // the wire.
        #[tokio::test]
        async fn additions_empty_request_serves_some_empty_proofs() {
            let store = store_at_peak().await;
            let api = api(store);
            let req = RequestAdditions {
                height: PEAK,
                header_hash: None,
                puzzle_hashes: Some(Vec::new()),
            };
            match api.additions(req).await {
                AdditionsReply::Respond(r) => {
                    assert!(r.coins.is_empty());
                    assert_eq!(
                        r.proofs,
                        Some(Vec::new()),
                        "the empty short-circuit sends proofs=[], not None"
                    );
                }
                AdditionsReply::Reject(_) => panic!("the empty request must serve"),
            }
        }

        // G2 closed — request_removals with specific coin names serves the removal coins with
        // MerkleSet proofs verifying against the foliage removals_root; Some-empty behaves like
        // None (all removals, proofs=None — ).
        #[tokio::test]
        async fn removals_serve_proofs_that_verify_against_the_foliage_root() {
            use dg_xch_core::consensus::merkle_set::validate_merkle_proof;
            let store = store_at_peak().await;
            let header_hash = fixture_peak_record().header_hash;
            let removals_root = fixture_block()
                .foliage_transaction_block
                .as_ref()
                .expect("tx block")
                .removals_root;
            let (_adds, rems) = fixture_adds_rems();
            let api = api(store);

            let included_name = rems[0].coin.name();
            let excluded_name = Bytes32::from([0x31; 32]);
            let req = RequestRemovals {
                height: PEAK,
                header_hash,
                coin_names: Some(vec![included_name, excluded_name]),
            };
            let r = match api.removals(req).await {
                RemovalsReply::Respond(r) => r,
                RemovalsReply::Reject(_) => {
                    panic!("a proof-requiring RequestRemovals must serve (G2)")
                }
            };
            let proofs = r.proofs.expect("the proof path carries proofs");
            assert_eq!(proofs.len(), 2);
            let root = removals_root.bytes();

            let (name, proof) = &proofs[0];
            assert_eq!(*name, included_name);
            assert_eq!(
                validate_merkle_proof(proof, &name.bytes(), &root),
                Ok(true),
                "removal inclusion proof verifies against the foliage removals_root"
            );
            assert_eq!(
                r.coins[0],
                (included_name, Some(rems[0].coin)),
                "the present name serves its coin"
            );

            let (name_e, proof_e) = &proofs[1];
            assert_eq!(*name_e, excluded_name);
            assert_eq!(
                validate_merkle_proof(proof_e, &name_e.bytes(), &root),
                Ok(false),
                "removal exclusion proof verifies as NOT-in-set"
            );
            assert_eq!(r.coins[1], (excluded_name, None));

            // Some-empty = the trusted all-removals path with proofs None.
            let req_empty = RequestRemovals {
                height: PEAK,
                header_hash,
                coin_names: Some(Vec::new()),
            };
            match api.removals(req_empty).await {
                RemovalsReply::Respond(r) => {
                    assert_eq!(r.coins.len(), rems.len(), "Some-empty serves ALL removals");
                    assert!(
                        r.proofs.is_none(),
                        "Some-empty carries proofs=None like None"
                    );
                }
                RemovalsReply::Reject(_) => panic!("Some-empty must serve"),
            }
        }

        #[tokio::test]
        async fn served_proofs_match_protocol_vectors() {
            #[derive(serde::Deserialize)]
            struct AddCase {
                puzzle_hash: String,
                included: bool,
                proof: String,
                coin_ids_proof: Option<String>,
            }
            #[derive(serde::Deserialize)]
            struct RemCase {
                coin_name: String,
                included: bool,
                proof: String,
            }
            #[derive(serde::Deserialize)]
            struct Fixture {
                additions: Vec<AddCase>,
                removals: Vec<RemCase>,
            }
            fn b32(s: &str) -> Bytes32 {
                Bytes32::from_str(s).expect("hex")
            }
            let fixture: Fixture =
                serde_json::from_str(include_str!("../tests/fixtures/merkle_proofs_5000000.json"))
                    .expect("proof fixture");

            let store = store_at_peak().await;
            let header_hash = fixture_peak_record().header_hash;
            let api = api(store);

            let req = RequestAdditions {
                height: PEAK,
                header_hash: None,
                puzzle_hashes: Some(
                    fixture
                        .additions
                        .iter()
                        .map(|c| b32(&c.puzzle_hash))
                        .collect(),
                ),
            };
            let r = match api.additions(req).await {
                AdditionsReply::Respond(r) => r,
                AdditionsReply::Reject(_) => panic!("additions must serve"),
            };
            let proofs = r.proofs.expect("proofs");
            assert_eq!(proofs.len(), fixture.additions.len());
            for (case, (ph, proof, coin_proof)) in fixture.additions.iter().zip(&proofs) {
                assert_eq!(*ph, b32(&case.puzzle_hash));
                assert_eq!(
                    hex::encode(proof),
                    case.proof,
                    "addition proof mismatch for {}",
                    case.puzzle_hash
                );
                match (&case.coin_ids_proof, coin_proof) {
                    (Some(expected), Some(served)) => assert_eq!(
                        hex::encode(served),
                        *expected,
                        "coin-ids proof mismatch for {}",
                        case.puzzle_hash
                    ),
                    (None, None) => assert!(!case.included),
                    _ => panic!("coin-ids proof presence mismatch for {}", case.puzzle_hash),
                }
            }

            let req = RequestRemovals {
                height: PEAK,
                header_hash,
                coin_names: Some(fixture.removals.iter().map(|c| b32(&c.coin_name)).collect()),
            };
            let r = match api.removals(req).await {
                RemovalsReply::Respond(r) => r,
                RemovalsReply::Reject(_) => panic!("removals must serve"),
            };
            let proofs = r.proofs.expect("proofs");
            assert_eq!(proofs.len(), fixture.removals.len());
            for (case, (name, proof)) in fixture.removals.iter().zip(&proofs) {
                assert_eq!(*name, b32(&case.coin_name));
                assert_eq!(
                    hex::encode(proof),
                    case.proof,
                    "removal proof mismatch for {} (included={})",
                    case.coin_name,
                    case.included
                );
            }
        }

        // RequestAdditions (trusted, no-proof path): every addition coin comes back grouped by puzzle
        // hash; a fork header hash and an oversized puzzle-hash list both reject.
        #[tokio::test]
        async fn additions_group_by_puzzle_hash_and_reject_forks() {
            let store = store_at_peak().await;
            let header_hash = fixture_peak_record().header_hash;
            let (adds, _rems) = fixture_adds_rems();
            let api = api(store);

            let req = RequestAdditions {
                height: PEAK,
                header_hash: None,
                puzzle_hashes: None,
            };
            match api.additions(req).await {
                AdditionsReply::Respond(r) => {
                    assert_eq!(r.header_hash, header_hash);
                    let total: usize = r.coins.iter().map(|(_, cs)| cs.len()).sum();
                    assert_eq!(total, adds.len(), "every addition coin is served");
                }
                AdditionsReply::Reject(_) => panic!("the peak additions must serve"),
            }

            // A header hash that is not the confirmed block at this height is a fork → reject.
            let forked = RequestAdditions {
                height: PEAK,
                header_hash: Some(Bytes32::from([0x99; 32])),
                puzzle_hashes: None,
            };
            assert!(matches!(
                api.additions(forked).await,
                AdditionsReply::Reject(_)
            ));

            // Too many puzzle hashes → reject before any DB work.
            let oversized = RequestAdditions {
                height: PEAK,
                header_hash: None,
                puzzle_hashes: Some(vec![Bytes32::default(); MAX_COIN_HASHES_PER_REQUEST + 1]),
            };
            assert!(matches!(
                api.additions(oversized).await,
                AdditionsReply::Reject(_)
            ));
        }

        // RequestRemovals (trusted, no-proof path): every removed coin comes back; an unknown block
        // rejects.
        #[tokio::test]
        async fn removals_serve_all_and_reject_unknown_block() {
            let store = store_at_peak().await;
            let header_hash = fixture_peak_record().header_hash;
            let (_adds, rems) = fixture_adds_rems();
            let api = api(store);

            let req = RequestRemovals {
                height: PEAK,
                header_hash,
                coin_names: None,
            };
            match api.removals(req).await {
                RemovalsReply::Respond(r) => {
                    assert_eq!(r.coins.len(), rems.len(), "every removed coin is served");
                    assert!(r.coins.iter().all(|(_, c)| c.is_some()));
                }
                RemovalsReply::Reject(_) => panic!("the peak removals must serve"),
            }

            let unknown = RequestRemovals {
                height: PEAK,
                header_hash: Bytes32::from([0x77; 32]),
                coin_names: None,
            };
            assert!(matches!(
                api.removals(unknown).await,
                RemovalsReply::Reject(_)
            ));
        }

        // RequestChildren: the coin states of a coin's children (spent + unspent), read from the parent
        // index.
        #[tokio::test]
        async fn children_returns_coin_states_by_parent() {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "fn_wallet_kids_{}_{nanos}.sqlite",
                std::process::id()
            ));
            let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
            let parent = Bytes32::from([0x0C; 32]);
            let child = Coin {
                parent_coin_info: parent,
                puzzle_hash: Bytes32::from([0x0D; 32]),
                amount: 42,
            };
            let record = CoinRecord {
                coin: child,
                confirmed_block_index: 10,
                spent_block_index: 0,
                coinbase: false,
                timestamp: 0,
                spent: false,
            };
            store
                .apply_block(10, 0, &[record], &[])
                .await
                .expect("seed");
            let api = api(store);

            let states = api.children(parent).await;
            assert_eq!(states.len(), 1);
            assert_eq!(states[0].coin.name(), child.name());
            assert_eq!(states[0].created_height, Some(10));
            assert_eq!(states[0].spent_height, None);

            // A coin with no children yields an empty (still valid) response.
            assert!(api.children(Bytes32::from([0xEE; 32])).await.is_empty());
        }

        // RegisterForPhUpdates returns the initial matching CoinState set — and it is FULL history:
        // both an unspent addition and a SPENT removal at the peak come back (unspent-only would be a
        // broken wallet backend). The first registration also yields the delivery receiver.
        #[tokio::test]
        async fn register_ph_returns_spent_and_unspent_initial_state() {
            let store = store_at_peak().await;
            let (adds, rems) = fixture_adds_rems();
            let api = api(store);

            let unspent_ph = adds[0].coin.puzzle_hash;
            let reg = api
                .register_for_ph_updates(
                    Bytes32::from([0xA1; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![unspent_ph],
                        min_height: 0,
                    },
                )
                .await;
            assert!(
                reg.receiver.is_some(),
                "the first registration hands back the delivery receiver"
            );
            assert_eq!(reg.response.puzzle_hashes, vec![unspent_ph]);
            assert!(
                reg.response
                    .coin_states
                    .iter()
                    .any(|cs| cs.coin.puzzle_hash == unspent_ph
                        && cs.created_height == Some(PEAK)
                        && cs.spent_height.is_none()),
                "an unspent addition with the subscribed ph is in the initial state"
            );

            // A puzzle hash that only a SPENT (removal) coin carries must still come back, with its
            // spent height set — the full-history guarantee.
            let spent_ph = rems[0].coin.puzzle_hash;
            let reg2 = api
                .register_for_ph_updates(
                    Bytes32::from([0xA2; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![spent_ph],
                        min_height: 0,
                    },
                )
                .await;
            assert!(
                reg2.response
                    .coin_states
                    .iter()
                    .any(|cs| cs.spent_height == Some(PEAK)),
                "a spent coin is included in the initial state (spent + unspent history)"
            );
        }

        // A CoinStateUpdate reaches a subscribed peer's delivery channel when a new peak creates a coin
        // with its puzzle hash — the peak-delta push the daemon's confirm path drives, exercised through
        // the same WalletNotifier the register handler subscribed against.
        #[tokio::test]
        async fn coin_state_update_delivered_across_a_peak_advance() {
            let store = store_at_peak().await;
            let (adds, _rems) = fixture_adds_rems();
            let api = api(store);

            let ph = adds[0].coin.puzzle_hash;
            let mut rx = api
                .register_for_ph_updates(
                    Bytes32::from([0xB1; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![ph],
                        min_height: 0,
                    },
                )
                .await
                .receiver
                .expect("receiver");

            // A new block at PEAK+1 creates a coin with the subscribed puzzle hash.
            let new_coin = Coin {
                parent_coin_info: Bytes32::from([0xEE; 32]),
                puzzle_hash: ph,
                amount: 7,
            };
            let record = CoinRecord {
                coin: new_coin,
                confirmed_block_index: PEAK + 1,
                spent_block_index: 0,
                coinbase: false,
                timestamp: 0,
                spent: false,
            };
            api.wallet
                .on_new_peak(
                    api.store.as_ref(),
                    crate::wallet::WalletUpdate {
                        peak_hash: Bytes32::from([0xF1; 32]),
                        height: PEAK + 1,
                        fork_height: PEAK,
                        created: &[record],
                        spent_ids: &[],
                        hints: &[],
                    },
                )
                .await
                .expect("push");

            let update = rx.recv().await.expect("a CoinStateUpdate is delivered");
            assert_eq!(update.height, PEAK + 1);
            assert!(
                update
                    .items
                    .iter()
                    .any(|cs| cs.coin.name() == new_coin.name()
                        && cs.created_height == Some(PEAK + 1)),
                "the created coin is in the update"
            );
        }

        // Disconnect hygiene: reconciling the registry against the live inbound peer set drops the gone
        // peer's subscription, and dropping its subscriber closes its delivery channel — so the per-peer
        // forwarder task ends (no leak).
        #[tokio::test]
        async fn disconnect_reconciliation_drops_the_subscription() {
            let store = store_at_peak().await;
            let api = api(store);

            let peer_live = Bytes32::from([0xC1; 32]);
            let peer_gone = Bytes32::from([0xC2; 32]);
            let _rx_live = api
                .register_for_ph_updates(
                    peer_live,
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![Bytes32::from([0x51; 32])],
                        min_height: 0,
                    },
                )
                .await
                .receiver
                .expect("live rx");
            let mut rx_gone = api
                .register_for_coin_updates(
                    peer_gone,
                    None,
                    RegisterForCoinUpdates {
                        coin_ids: vec![Bytes32::from([0x52; 32])],
                        min_height: 0,
                    },
                )
                .await
                .receiver
                .expect("gone rx");
            assert_eq!(api.wallet.subscriber_count().await, 2);

            let live: std::collections::HashSet<Bytes32> = std::iter::once(peer_live).collect();
            api.wallet.retain_live(&live).await;

            assert_eq!(api.wallet.subscriber_count().await, 1);
            assert!(
                rx_gone.recv().await.is_none(),
                "the disconnected peer's delivery channel closes, ending its forwarder"
            );
        }

        // ---- Wallet-serve bounds ----------------------------------------------

        // The height the synthetic subscription coins are seeded at (any tx-block height works: the
        // register read is a pure coin-store query, blind to the block store).
        const SEED_HEIGHT: u32 = 100;

        // A synthetic unspent coin on `ph`, keyed by `tag` so coin names are distinct.
        fn synth_record(tag: u8, ph: Bytes32, height: u32) -> CoinRecord {
            CoinRecord {
                coin: Coin {
                    parent_coin_info: Bytes32::from([tag; 32]),
                    puzzle_hash: ph,
                    amount: u64::from(tag) + 1,
                },
                confirmed_block_index: height,
                spent_block_index: 0,
                coinbase: false,
                timestamp: 0,
                spent: false,
            }
        }

        async fn store_with(records: &[CoinRecord]) -> Arc<SqliteStore> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "fn_walletcap_{}_{nanos}.sqlite",
                std::process::id()
            ));
            let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
            store
                .apply_block(SEED_HEIGHT, 0, records, &[])
                .await
                .expect("seed coins");
            store
        }

        fn default_sem() -> Arc<LimitedSemaphore> {
            Arc::new(LimitedSemaphore::new(
                WALLET_SYNC_ACTIVE_LIMIT,
                WALLET_SYNC_WAITING_LIMIT,
            ))
        }

        // The RegisterForPhUpdates initial-state read is bounded by
        // `max_subscribe_response_items` (the store query takes the budget as max_items).
        // Truncation is SILENT: logged, and the reply still echoes the REQUESTED puzzle hashes.
        #[tokio::test]
        async fn ph_initial_state_is_bounded_by_the_response_budget() {
            let ph = Bytes32::from([0x42; 32]);
            let records: Vec<CoinRecord> =
                (1..=8).map(|t| synth_record(t, ph, SEED_HEIGHT)).collect();
            let store = store_with(&records).await;
            let api = api_tuned(store, Arc::new(WalletNotifier::new()), 5, default_sem());

            let reg = api
                .register_for_ph_updates(
                    Bytes32::from([0xD1; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![ph],
                        min_height: 0,
                    },
                )
                .await;
            assert_eq!(
                reg.response.puzzle_hashes,
                vec![ph],
                "the reply echoes the REQUESTED hashes even when truncating"
            );
            assert_eq!(
                reg.response.coin_states.len(),
                5,
                "the initial-state set is silently truncated to max_subscribe_response_items"
            );
        }

        // Hint leg: ONE budget is decremented across the puzzle-hash query and then the hint
        // query (`max_items -= len(states)` before the hint-id lookup).
        #[cfg(feature = "hint")]
        #[tokio::test]
        async fn ph_response_budget_is_shared_with_the_hint_query() {
            let ph = Bytes32::from([0x43; 32]);
            let mut records: Vec<CoinRecord> =
                (1..=3).map(|t| synth_record(t, ph, SEED_HEIGHT)).collect();
            // Four coins on OTHER puzzle hashes, each HINTED by the subscribed 32-byte value
            // (the subscribed hashes double as hint keys).
            let hinted: Vec<CoinRecord> = (10..=13)
                .map(|t| synth_record(t, Bytes32::from([t; 32]), SEED_HEIGHT))
                .collect();
            records.extend(hinted.iter().cloned());
            let store = store_with(&records).await;
            let pairs: Vec<(Bytes32, Bytes32)> =
                hinted.iter().map(|r| (ph, r.coin.name())).collect();
            store.apply_hints(&pairs).await.expect("hints");

            // Budget 5: the ph query consumes 3, leaving 2 for the hint side → exactly 5 states.
            let api = api_tuned(
                store.clone(),
                Arc::new(WalletNotifier::new()),
                5,
                default_sem(),
            );
            let reg = api
                .register_for_ph_updates(
                    Bytes32::from([0xD2; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![ph],
                        min_height: 0,
                    },
                )
                .await;
            assert_eq!(
                reg.response.coin_states.len(),
                5,
                "ph states (3) + hint states capped to the remaining budget (2)"
            );

            // Budget 3: fully consumed by the ph query — the hint query gets nothing.
            let api = api_tuned(store, Arc::new(WalletNotifier::new()), 3, default_sem());
            let reg = api
                .register_for_ph_updates(
                    Bytes32::from([0xD3; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![ph],
                        min_height: 0,
                    },
                )
                .await;
            assert_eq!(
                reg.response.coin_states.len(),
                3,
                "an exhausted budget starves the hint query entirely"
            );
        }

        // Dedup half: ONLY add_puzzle_subscriptions' return — the newly-subscribed set —
        // feeds the initial-state query, so re-registering an
        // already-subscribed hash yields NO initial states (and no repeated heavy scan).
        #[tokio::test]
        async fn repeat_ph_registration_yields_no_initial_state() {
            let ph = Bytes32::from([0x44; 32]);
            let records: Vec<CoinRecord> =
                (1..=2).map(|t| synth_record(t, ph, SEED_HEIGHT)).collect();
            let store = store_with(&records).await;
            let api = api(store);
            let peer = Bytes32::from([0xD4; 32]);
            let req = || RegisterForPhUpdates {
                puzzle_hashes: vec![ph],
                min_height: 0,
            };

            let reg = api.register_for_ph_updates(peer, None, req()).await;
            assert_eq!(reg.response.coin_states.len(), 2);

            let reg2 = api.register_for_ph_updates(peer, None, req()).await;
            assert!(
                reg2.response.coin_states.is_empty(),
                "an already-subscribed hash is filtered from the initial-state query"
            );
        }

        // Overflow half: a hash dropped by the per-peer subscription cap is NOT part of
        // add_puzzle_subscriptions' return, so it is never queried — the initial-state read cannot be
        // driven past the subscription cap with hashes that were never subscribed.
        #[tokio::test]
        async fn overflow_dropped_puzzle_hashes_are_not_queried() {
            let (ph_a, ph_b, ph_c) = (
                Bytes32::from([0x51; 32]),
                Bytes32::from([0x52; 32]),
                Bytes32::from([0x53; 32]),
            );
            // Coins exist ONLY on the third hash — the one the cap (2) drops.
            let records: Vec<CoinRecord> = (1..=3)
                .map(|t| synth_record(t, ph_c, SEED_HEIGHT))
                .collect();
            let store = store_with(&records).await;
            let api = api_tuned(
                store,
                Arc::new(WalletNotifier::with_limits(8, 2)),
                MAX_SUBSCRIBE_RESPONSE_ITEMS,
                default_sem(),
            );

            let reg = api
                .register_for_ph_updates(
                    Bytes32::from([0xD5; 32]),
                    None,
                    RegisterForPhUpdates {
                        puzzle_hashes: vec![ph_a, ph_b, ph_c],
                        min_height: 0,
                    },
                )
                .await;
            assert_eq!(
                reg.response.puzzle_hashes,
                vec![ph_a, ph_b, ph_c],
                "the reply echoes the full requested list"
            );
            assert!(
                reg.response.coin_states.is_empty(),
                "the overflow-dropped hash must not feed the initial-state query"
            );
        }

        // Coin leg: the REQUEST list truncates to max_subscriptions; the SLICED list is
        // subscribed, queried, and echoed back (the coin path deliberately keeps in-request
        // duplicates queryable).
        #[tokio::test]
        async fn coin_registration_slices_the_request_to_the_subscription_cap() {
            let records: Vec<CoinRecord> = (1..=3)
                .map(|t| synth_record(t, Bytes32::from([0x60 + t; 32]), SEED_HEIGHT))
                .collect();
            let ids: Vec<Bytes32> = records.iter().map(|r| r.coin.name()).collect();
            let store = store_with(&records).await;
            let api = api_tuned(
                store,
                Arc::new(WalletNotifier::with_limits(8, 2)),
                MAX_SUBSCRIBE_RESPONSE_ITEMS,
                default_sem(),
            );

            let reg = api
                .register_for_coin_updates(
                    Bytes32::from([0xD6; 32]),
                    None,
                    RegisterForCoinUpdates {
                        coin_ids: ids.clone(),
                        min_height: 0,
                    },
                )
                .await;
            assert_eq!(
                reg.response.coin_ids,
                ids[..2].to_vec(),
                "the coin response echoes the SLICED list (unlike the ph response)"
            );
            assert_eq!(
                reg.response.coin_states.len(),
                2,
                "only the sliced ids are queried"
            );
        }

        // Coin leg: the RegisterForCoinUpdates initial read is bounded by the same response
        // budget (get_coin_states_by_ids(max_items=max_items)).
        #[tokio::test]
        async fn coin_initial_state_is_bounded_by_the_response_budget() {
            let records: Vec<CoinRecord> = (1..=4)
                .map(|t| synth_record(t, Bytes32::from([0x70 + t; 32]), SEED_HEIGHT))
                .collect();
            let ids: Vec<Bytes32> = records.iter().map(|r| r.coin.name()).collect();
            let store = store_with(&records).await;
            let api = api_tuned(store, Arc::new(WalletNotifier::new()), 2, default_sem());

            let reg = api
                .register_for_coin_updates(
                    Bytes32::from([0xD7; 32]),
                    None,
                    RegisterForCoinUpdates {
                        coin_ids: ids.clone(),
                        min_height: 0,
                    },
                )
                .await;
            assert_eq!(
                reg.response.coin_ids, ids,
                "under the subscription cap the echo is the full request"
            );
            assert_eq!(
                reg.response.coin_states.len(),
                2,
                "the initial-state set is truncated to the response budget"
            );
        }

        // additions/removals are guarded by the wallet-sync LimitedSemaphore — overflow REJECTS
        // (RejectAdditionsRequest / RejectRemovalsRequest) instead of queueing unbounded
        // concurrent block-delta scans.
        #[tokio::test]
        async fn wallet_serve_rejects_when_the_sync_semaphore_is_full() {
            let store = store_at_peak().await;
            let peak_hash = fixture_peak_record().header_hash;

            // active=0, waiting=0: every acquire overflows immediately.
            let api_full = api_tuned(
                store.clone(),
                Arc::new(WalletNotifier::new()),
                MAX_SUBSCRIBE_RESPONSE_ITEMS,
                Arc::new(LimitedSemaphore::new(0, 0)),
            );
            let additions_req = || RequestAdditions {
                height: PEAK,
                header_hash: None,
                puzzle_hashes: None,
            };
            let removals_req = || RequestRemovals {
                height: PEAK,
                header_hash: peak_hash,
                coin_names: None,
            };
            assert!(
                matches!(
                    api_full.additions(additions_req()).await,
                    AdditionsReply::Reject(_)
                ),
                "additions must reject on wallet-sync semaphore overflow"
            );
            assert!(
                matches!(
                    api_full.removals(removals_req()).await,
                    RemovalsReply::Reject(_)
                ),
                "removals must reject on wallet-sync semaphore overflow"
            );

            // Within bounds, the same requests serve.
            let api_ok = api_tuned(
                store,
                Arc::new(WalletNotifier::new()),
                MAX_SUBSCRIBE_RESPONSE_ITEMS,
                default_sem(),
            );
            assert!(matches!(
                api_ok.additions(additions_req()).await,
                AdditionsReply::Respond(_)
            ));
            assert!(matches!(
                api_ok.removals(removals_req()).await,
                RemovalsReply::Respond(_)
            ));
        }

        // ---- request_puzzle_state / request_coin_state at the api seam, where the response
        // budget and subscription caps are
        // injectable — the production 100k/200k numbers are impractical to seed. The wire-level
        // contract (dispatch, rejects, the Sage sequence) is proven in tests/puzzle_state.rs.

        // A minimal MAIN-CHAIN block record at a height: header_hash [height;32] linked by
        // prev_hash, so add_block_records + set_peak(tip) marks the whole ancestry in-main-chain
        // and height_to_hash resolves every page boundary.
        fn chain_rec(height: u32) -> BlockRecord {
            use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
            BlockRecord {
                header_hash: Bytes32::from([height as u8; 32]),
                prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
                height,
                weight: u128::from(height) * 100,
                total_iters: u128::from(height),
                signage_point_index: 0,
                challenge_vdf_output: ClassgroupElement::get_default_element(),
                infused_challenge_vdf_output: None,
                reward_infusion_new_challenge: Bytes32::default(),
                challenge_block_info_hash: Bytes32::default(),
                sub_slot_iters: MAINNET.sub_slot_iters_starting,
                pool_puzzle_hash: Bytes32::default(),
                farmer_puzzle_hash: Bytes32::default(),
                required_iters: 1,
                deficit: 0,
                overflow: false,
                prev_transaction_block_height: 0,
                timestamp: Some(1_700_000_000),
                prev_transaction_block_hash: None,
                fees: None,
                reward_claims_incorporated: None,
                finished_challenge_slot_hashes: None,
                finished_infused_challenge_slot_hashes: None,
                finished_reward_slot_hashes: None,
                sub_epoch_summary_included: None,
            }
        }

        // A store carrying a synthetic 0..=tip main chain plus `per_height` coins on `ph` at each
        // of `coin_heights` — the paging scenario RequestPuzzleState's height/header_hash contract
        // needs (every page boundary must resolve on the main chain for the NEXT request's
        // reorg-consistency check to pass).
        async fn paging_store(
            ph: Bytes32,
            coin_heights: &[u32],
            per_height: usize,
            tip: u32,
        ) -> Arc<SqliteStore> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("fn_puzstate_{}_{nanos}.sqlite", std::process::id()));
            let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
            let records: Vec<BlockRecord> = (0..=tip).map(chain_rec).collect();
            store.add_block_records(&records).await.expect("records");
            store
                .set_peak(&chain_rec(tip).header_hash)
                .await
                .expect("peak");
            let mut tag = 0u8;
            for h in coin_heights {
                let recs: Vec<CoinRecord> = (0..per_height)
                    .map(|_| {
                        tag += 1;
                        CoinRecord {
                            coin: Coin {
                                parent_coin_info: Bytes32::from([tag; 32]),
                                puzzle_hash: ph,
                                amount: 1_000,
                            },
                            confirmed_block_index: *h,
                            spent_block_index: 0,
                            coinbase: false,
                            timestamp: 0,
                            spent: false,
                        }
                    })
                    .collect();
                store.apply_block(*h, 0, &recs, &[]).await.expect("coins");
            }
            store
        }

        fn all_filters() -> dg_xch_core::protocols::wallet::CoinStateFilters {
            dg_xch_core::protocols::wallet::CoinStateFilters {
                include_spent: true,
                include_unspent: true,
                include_hinted: true,
                min_amount: 0,
            }
        }

        // Sage's sync_puzzle_hashes loop (wallet_sync.rs:169-206) against an injected 4-item
        // budget: each page's (height, header_hash) feeds the NEXT request's reorg-consistency
        // check (which must PASS against our chain), no page splits a height, heights are
        // ordered, and the union over pages is exactly the seeded set. This is the paging
        // contract end-to-end at the api seam.
        #[tokio::test]
        async fn puzzle_state_pages_thread_the_sage_loop_to_convergence() {
            let ph = Bytes32::from([0x5A; 32]);
            // 4 heights x 3 coins, budget 4: boundaries fall inside heights, forcing the
            // whole-height trim to shrink pages.
            let store = paging_store(ph, &[10, 11, 12, 13], 3, 20).await;
            let api = api_tuned(store, Arc::new(WalletNotifier::new()), 4, default_sem());
            let peer = Bytes32::from([0xB1; 32]);

            let mut previous_height: Option<u32> = None;
            let mut header_hash = MAINNET.genesis_challenge;
            let mut names = HashSet::new();
            let mut pages = 0;
            loop {
                let reply = api
                    .puzzle_state(
                        peer,
                        None,
                        RequestPuzzleState {
                            puzzle_hashes: vec![ph],
                            previous_height,
                            header_hash,
                            filters: all_filters(),
                            subscribe_when_finished: true,
                        },
                    )
                    .await;
                let PuzzleStateReply::Respond(resp, _rx) = reply else {
                    panic!("page {pages} must serve, not reject");
                };
                pages += 1;
                assert!(resp.coin_states.len() <= 4, "no page exceeds the budget");
                let heights: Vec<u32> = resp
                    .coin_states
                    .iter()
                    .map(|cs| cs.created_height.unwrap_or(0))
                    .collect();
                let mut sorted = heights.clone();
                sorted.sort_unstable();
                assert_eq!(heights, sorted, "page is height-ordered");
                if let Some(max_h) = heights.last() {
                    assert!(
                        resp.is_finished || *max_h <= resp.height,
                        "no state beyond the page's reported height"
                    );
                }
                for cs in &resp.coin_states {
                    assert!(names.insert(cs.coin.name()), "no duplicates across pages");
                }
                // The page's header_hash IS our main chain at the page height — that is what
                // makes the next request's reorg check pass.
                assert_eq!(resp.header_hash, chain_rec(resp.height).header_hash);
                if resp.is_finished {
                    assert_eq!(resp.height, 20, "the final page reports the peak");
                    break;
                }
                previous_height = Some(resp.height);
                header_hash = resp.header_hash;
                assert!(pages < 20, "the page loop must terminate");
            }
            assert!(pages > 1, "the scenario must actually page");
            assert_eq!(names.len(), 12, "the loop converges to the seeded set");
        }

        // The subscribe side effect and its caps: subscribe_when_finished registers against the
        // SAME per-peer cap the
        // register handlers use — an over-cap request rejects EXCEEDED_SUBSCRIPTION_LIMIT (the
        // exact reject Sage maps to SubscriptionLimitReached), cumulative across requests; a
        // non-subscribing request of the same size still serves.
        #[tokio::test]
        async fn puzzle_and_coin_state_subscribe_against_the_shared_cap() {
            let ph = Bytes32::from([0x5B; 32]);
            let store = paging_store(ph, &[10], 1, 12).await;
            // cap: 4 combined items per peer.
            let wallet = Arc::new(WalletNotifier::with_limits(8, 4));
            let api = api_tuned(
                store,
                wallet.clone(),
                MAX_SUBSCRIBE_RESPONSE_ITEMS,
                default_sem(),
            );
            let peer = Bytes32::from([0xB2; 32]);
            let phs = |tags: std::ops::Range<u8>| -> Vec<Bytes32> {
                tags.map(|t| Bytes32::from([t; 32])).collect()
            };
            let req = |puzzle_hashes: Vec<Bytes32>, subscribe: bool| RequestPuzzleState {
                puzzle_hashes,
                previous_height: None,
                header_hash: MAINNET.genesis_challenge,
                filters: all_filters(),
                subscribe_when_finished: subscribe,
            };

            // Over the cap in one request → EXCEEDED, and NOTHING was subscribed.
            let reply = api.puzzle_state(peer, None, req(phs(1..6), true)).await;
            assert!(
                matches!(
                    reply,
                    PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit)
                ),
                "5 subscriptions against a cap of 4 must reject EXCEEDED"
            );
            assert_eq!(wallet.peer_subscription_count(&peer).await, 0);

            // The same 5 hashes WITHOUT the subscribe flag serve fine (the cap gates only the
            // side effect, `request.subscribe_when_finished and ...`).
            assert!(matches!(
                api.puzzle_state(peer, None, req(phs(1..6), false)).await,
                PuzzleStateReply::Respond(..)
            ));
            assert_eq!(wallet.peer_subscription_count(&peer).await, 0);

            // 3 subscribe, then 2 more blow the cap CUMULATIVELY (3 + 2 > 4)…
            let PuzzleStateReply::Respond(_, rx) =
                api.puzzle_state(peer, None, req(phs(1..4), true)).await
            else {
                panic!("3 subscriptions fit the cap");
            };
            assert!(
                rx.is_some(),
                "first registration yields the delivery receiver"
            );
            assert_eq!(wallet.peer_subscription_count(&peer).await, 3);
            assert!(matches!(
                api.puzzle_state(peer, None, req(phs(4..6), true)).await,
                PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit)
            ));

            // …and the coin leg counts against the SAME combined cap
            // (peer_subscription_count sums both sets): 3 ph + 2 coin ids > 4 → EXCEEDED; 1 fits.
            let coin_req = |coin_ids: Vec<Bytes32>, subscribe: bool| RequestCoinState {
                coin_ids,
                previous_height: None,
                header_hash: MAINNET.genesis_challenge,
                subscribe,
            };
            assert!(matches!(
                api.coin_state(peer, None, coin_req(phs(0x10..0x12), true))
                    .await,
                CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit)
            ));
            let CoinStateReply::Respond(_, rx) = api
                .coin_state(peer, None, coin_req(phs(0x10..0x11), true))
                .await
            else {
                panic!("1 more subscription fits the cap exactly");
            };
            assert!(rx.is_none(), "one delivery channel per peer");
            assert_eq!(wallet.peer_subscription_count(&peer).await, 4);

            // remove-all returns each leg's subscribed set and frees the cap.
            let removed_ph = api.remove_puzzle_subscriptions(peer, None).await;
            assert_eq!(removed_ph.len(), 3);
            let removed_coins = api.remove_coin_subscriptions(peer, None).await;
            assert_eq!(removed_coins, vec![Bytes32::from([0x10; 32])]);
            assert_eq!(wallet.peer_subscription_count(&peer).await, 0);
            assert!(matches!(
                api.puzzle_state(peer, None, req(phs(1..5), true)).await,
                PuzzleStateReply::Respond(..)
            ));
        }

        // request_coin_state truncates the id list to the response budget BEFORE serving and
        // echoes the truncated, deduped list (via the list_limits parse cap +
        // dict.fromkeys) — with the budget injected small enough to see it.
        #[tokio::test]
        async fn coin_state_truncates_the_id_list_to_the_response_budget() {
            let ph = Bytes32::from([0x5C; 32]);
            let store = paging_store(ph, &[10], 3, 12).await;
            let api = api_tuned(store, Arc::new(WalletNotifier::new()), 2, default_sem());
            let peer = Bytes32::from([0xB3; 32]);
            let ids: Vec<Bytes32> = (1u8..=4).map(|t| Bytes32::from([t; 32])).collect();
            let CoinStateReply::Respond(resp, _) = api
                .coin_state(
                    peer,
                    None,
                    RequestCoinState {
                        coin_ids: ids.clone(),
                        previous_height: None,
                        header_hash: MAINNET.genesis_challenge,
                        subscribe: false,
                    },
                )
                .await
            else {
                panic!("must serve");
            };
            assert_eq!(
                resp.coin_ids,
                ids[..2].to_vec(),
                "the echoed list is the budget-truncated request"
            );
        }

        #[tokio::test]
        async fn coin_state_response_budget_is_trusted_for_configured_node_id() {
            let trusted = Bytes32::from([0xaa; 32]);
            let untrusted = Bytes32::from([0xbb; 32]);
            // untrusted response cap 2, trusted response cap 4 (subscription caps irrelevant here).
            let policy = Arc::new(TrustPolicy::with_caps(
                std::collections::HashSet::from([trusted]),
                usize::MAX,
                usize::MAX,
                2,
                4,
            ));
            let store = store_with(&[]).await;
            let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
            let api = api_trust(store, wallet, policy, default_sem());
            let ids: Vec<Bytes32> = (1u8..=4).map(|t| Bytes32::from([t; 32])).collect();
            let req = |coin_ids: Vec<Bytes32>| RequestCoinState {
                coin_ids,
                previous_height: None,
                header_hash: MAINNET.genesis_challenge,
                subscribe: false,
            };

            let CoinStateReply::Respond(untrusted_resp, _) =
                api.coin_state(untrusted, None, req(ids.clone())).await
            else {
                panic!("untrusted must serve");
            };
            assert_eq!(
                untrusted_resp.coin_ids,
                ids[..2].to_vec(),
                "untrusted echoes only the 100k-tier budget (2 here)"
            );

            let CoinStateReply::Respond(trusted_resp, _) =
                api.coin_state(trusted, None, req(ids.clone())).await
            else {
                panic!("trusted must serve");
            };
            assert_eq!(
                trusted_resp.coin_ids, ids,
                "trusted echoes the whole 500k-tier budget (4 here)"
            );
        }

        #[tokio::test]
        async fn on_transaction_gives_trusted_peer_high_priority() {
            let trusted = Bytes32::from([0xaa; 32]);
            let untrusted = Bytes32::from([0xbb; 32]);
            let policy = Arc::new(TrustPolicy::new(std::collections::HashSet::from([trusted])));
            let store = store_with(&[]).await;
            let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
            let api = api_trust(store, wallet, policy, default_sem());

            let bundle = |b: u8| SpendBundle {
                coin_spends: vec![],
                aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::from([b; 96]),
            };
            let untrusted_tx = bundle(0x01);
            let trusted_tx = bundle(0x02);
            let untrusted_name = untrusted_tx.name().expect("name");
            let trusted_name = trusted_tx.name().expect("name");
            // Pre-solicit both ids so on_transaction accepts the bodies.
            {
                let mut req = api.tx_requested.lock().await;
                req.insert(
                    untrusted_name,
                    PendingTx {
                        at: Instant::now(),
                        advertised_fee: 0,
                        advertised_cost: 1,
                    },
                );
                req.insert(
                    trusted_name,
                    PendingTx {
                        at: Instant::now(),
                        advertised_fee: 0,
                        advertised_cost: 1,
                    },
                );
            }

            // Untrusted arrives FIRST, trusted SECOND.
            api.on_respond_transaction(untrusted, None, untrusted_tx)
                .await;
            api.on_respond_transaction(trusted, None, trusted_tx).await;

            let batch = api.tx_inbox.lock().await.drain_batch();
            assert_eq!(batch.len(), 2);
            assert_eq!(
                batch[0].0, trusted,
                "trusted bundle drains first (high-priority lane)"
            );
            assert_eq!(batch[1].0, untrusted, "untrusted bundle follows");
        }

        #[tokio::test]
        async fn coin_state_response_budget_is_trusted_for_localhost_host() {
            let peer = Bytes32::from([0xcc; 32]);
            // EMPTY trusted node-id set; response caps untrusted 2 / trusted 4.
            let policy = Arc::new(TrustPolicy::with_caps(
                std::collections::HashSet::new(),
                usize::MAX,
                usize::MAX,
                2,
                4,
            ));
            let store = store_with(&[]).await;
            let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
            let api = api_trust(store, wallet, policy, default_sem());
            let ids: Vec<Bytes32> = (1u8..=4).map(|t| Bytes32::from([t; 32])).collect();
            let req = |coin_ids: Vec<Bytes32>| RequestCoinState {
                coin_ids,
                previous_height: None,
                header_hash: MAINNET.genesis_challenge,
                subscribe: false,
            };

            // A remote host (not localhost, not in any CIDR) → the untrusted 2-item budget.
            let CoinStateReply::Respond(remote_resp, _) = api
                .coin_state(peer, Some("203.0.113.7".parse().unwrap()), req(ids.clone()))
                .await
            else {
                panic!("remote must serve");
            };
            assert_eq!(
                remote_resp.coin_ids,
                ids[..2].to_vec(),
                "remote peer echoes only the untrusted budget (2 here)"
            );

            // The SAME peer id from 127.0.0.1 → the trusted 4-item budget, with no config change.
            let CoinStateReply::Respond(local_resp, _) = api
                .coin_state(peer, Some("127.0.0.1".parse().unwrap()), req(ids.clone()))
                .await
            else {
                panic!("localhost must serve");
            };
            assert_eq!(
                local_resp.coin_ids, ids,
                "localhost peer echoes the whole trusted budget (4 here)"
            );
        }

        #[tokio::test]
        async fn on_transaction_gives_localhost_peer_high_priority() {
            let local_peer = Bytes32::from([0xcc; 32]);
            let remote_peer = Bytes32::from([0xdd; 32]);
            // Empty policy: trust comes purely from the host being localhost.
            let policy = Arc::new(TrustPolicy::default());
            let store = store_with(&[]).await;
            let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
            let api = api_trust(store, wallet, policy, default_sem());
            let bundle = |b: u8| SpendBundle {
                coin_spends: vec![],
                aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::from([b; 96]),
            };
            let remote_tx = bundle(0x01);
            let local_tx = bundle(0x02);
            let remote_name = remote_tx.name().expect("name");
            let local_name = local_tx.name().expect("name");
            {
                let mut req = api.tx_requested.lock().await;
                req.insert(
                    remote_name,
                    PendingTx {
                        at: Instant::now(),
                        advertised_fee: 0,
                        advertised_cost: 1,
                    },
                );
                req.insert(
                    local_name,
                    PendingTx {
                        at: Instant::now(),
                        advertised_fee: 0,
                        advertised_cost: 1,
                    },
                );
            }

            // Remote (untrusted) arrives FIRST, localhost SECOND.
            api.on_respond_transaction(
                remote_peer,
                Some("198.51.100.9".parse().unwrap()),
                remote_tx,
            )
            .await;
            api.on_respond_transaction(local_peer, Some("127.0.0.1".parse().unwrap()), local_tx)
                .await;

            let batch = api.tx_inbox.lock().await.drain_batch();
            assert_eq!(batch.len(), 2);
            assert_eq!(
                batch[0].0, local_peer,
                "localhost bundle drains first (high-priority lane)"
            );
            assert_eq!(batch[1].0, remote_peer, "remote bundle follows");
        }
    }
}

// Compact-VDF SOLICITATION send path. The scan's
// per-block plan/dedup is proven purely in full-node/tests/compact_vdf.rs (against a real mainnet
// block); here we prove the peer-conditional fan-out — the half that turns a solicitation list into
// RequestCompactProofOfTime messages on connected TIMELORD links — against a recording mock, because
// a live SocketPeer wraps a websocket sink that cannot be constructed offline.
#[cfg(test)]
mod uncompact_solicit_tests {
    use super::*;
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use std::io::Cursor;
    use tokio::sync::Mutex as TokioMutex;

    // A SolicitTarget that records every delivered message and reports a fixed node type — the
    // offline stand-in for a live bluebox link.
    struct RecordingTarget {
        node_type: NodeType,
        sent: Arc<TokioMutex<Vec<dg_xch_core::protocols::ChiaMessage>>>,
    }

    #[async_trait]
    impl SolicitTarget for RecordingTarget {
        async fn is_timelord(&self) -> bool {
            self.node_type == NodeType::Timelord
        }
        async fn negotiated_version(&self) -> ChiaProtocolVersion {
            ChiaProtocolVersion::default()
        }
        async fn deliver(&self, msg: dg_xch_core::protocols::ChiaMessage) -> Result<(), Error> {
            self.sent.lock().await.push(msg);
            Ok(())
        }
    }

    fn a_request(field_vdf: u8, tag: u8) -> RequestCompactProofOfTime {
        RequestCompactProofOfTime {
            new_proof_of_time: VdfInfo {
                challenge: Bytes32::from([tag; 32]),
                number_of_iterations: u64::from(tag) * 1000,
                output: ClassgroupElement::get_default_element(),
            },
            header_hash: Bytes32::from([tag ^ 0xFF; 32]),
            height: 5_000_000 + u32::from(tag),
            field_vdf,
        }
    }

    fn target(node_type: NodeType) -> RecordingTarget {
        RecordingTarget {
            node_type,
            sent: Arc::new(TokioMutex::new(Vec::new())),
        }
    }

    // A bulky field plus a connected TIMELORD peer means a RequestCompactProofOfTime is sent for
    // that field, and it round-trips back to the exact request that was planned. A scan that only
    // counted and logged would send nothing.
    #[tokio::test]
    async fn a_bulky_field_is_solicited_from_a_connected_timelord() {
        let reqs = vec![a_request(3 /* CC_SP */, 7)];
        let peers = vec![target(NodeType::Timelord)];
        let net = NetCounters::default();

        let sent = solicit_uncompact_from_timelords(&reqs, &peers, &net).await;
        assert_eq!(
            sent, 1,
            "the one bulky field is solicited from the timelord"
        );

        let recorded = peers[0].sent.lock().await;
        assert_eq!(recorded.len(), 1, "exactly one request message delivered");
        let msg = &recorded[0];
        assert_eq!(
            msg.msg_type,
            dg_xch_core::protocols::ProtocolMessageTypes::RequestCompactProofOfTime,
        );
        let decoded = RequestCompactProofOfTime::from_bytes(
            &mut Cursor::new(msg.data.as_slice()),
            ChiaProtocolVersion::default(),
        )
        .expect("wire round-trips");
        assert_eq!(decoded, reqs[0], "the sent request matches the planned one");
    }

    // The network-infused case: NO timelord peer connected, only a full-node peer. The scan runs,
    // nothing is sent, nothing panics.
    #[tokio::test]
    async fn no_timelord_peer_sends_nothing_and_does_not_panic() {
        let reqs = vec![a_request(4 /* CC_IP */, 9)];
        let peers = vec![target(NodeType::FullNode)];
        let net = NetCounters::default();

        let sent = solicit_uncompact_from_timelords(&reqs, &peers, &net).await;
        assert_eq!(sent, 0, "no timelord target ⇒ no solicitation sent");
        assert!(
            peers[0].sent.lock().await.is_empty(),
            "the full-node peer is never sent a bluebox request"
        );

        // And an empty peer set is equally a no-op.
        let empty: Vec<RecordingTarget> = Vec::new();
        assert_eq!(
            solicit_uncompact_from_timelords(&reqs, &empty, &net).await,
            0,
        );
    }

    // An empty solicitation list is a no-op regardless of peers (the scan found nothing bulky).
    #[tokio::test]
    async fn an_empty_solicitation_list_sends_nothing() {
        let peers = vec![target(NodeType::Timelord)];
        let net = NetCounters::default();
        assert_eq!(solicit_uncompact_from_timelords(&[], &peers, &net).await, 0,);
        assert!(peers[0].sent.lock().await.is_empty());
    }
}

#[cfg(test)]
mod ub_relay_gate_tests {
    use super::*;
    use dg_xch_core::blockchain::header_block::HeaderBlock;
    use dg_xch_core::blockchain::weight_proof::RecentChainData;
    use dg_xch_core::clvm::program::SerializedProgram;
    use dg_xch_core::consensus::block_header_validation::validate_pospace_and_get_required_iters;
    use dg_xch_core::consensus::deficit::calculate_deficit;
    use dg_xch_core::consensus::full_block_to_block_record::header_block_to_sub_block_record;
    use dg_xch_core::consensus::get_block_challenge::pre_sp_tx_block_height;
    use dg_xch_core::consensus::pot_iterations::is_overflow_block;
    use dg_xch_node::PrimitiveVerifier;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Golden ssi/difficulty for the slice's epoch (no epoch turn inside 9,054,336..=9,054,620 —
    // the next is at 9,054,720) — the same reference pair the header-validation test validates against.
    const SSI: u64 = 574_619_648;
    const DIFF: u64 = 2608;

    fn load_chain() -> Vec<HeaderBlock> {
        let bytes =
            include_bytes!("../../node/tests/fixtures/recent_chain_mainnet_9054336_9054620.bin");
        RecentChainData::from_bytes(
            &mut std::io::Cursor::new(&bytes[..]),
            ChiaProtocolVersion::default(),
        )
        .expect("recent chain slice deserializes")
        .recent_chain_data
    }

    // Light-path (proof-of-space only, no VDF) required_iters, to seed ancestor records with a
    // correct pb.ip_iters — the header-validation seeding recipe
    // (node/tests/header_validation.rs).
    fn light_required_iters(
        ancestors: &HashMap<Bytes32, BlockRecord>,
        block: &HeaderBlock,
        challenge: Bytes32,
        prev_challenge: Bytes32,
        overflow: bool,
    ) -> u64 {
        let rcb = &block.reward_chain_block;
        let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
            None => challenge,
            Some(v) => v.output.hash().expect("cc sp hash"),
        };
        let pre = pre_sp_tx_block_height(
            &MAINNET,
            ancestors,
            block.prev_header_hash(),
            rcb.signage_point_index,
            block.finished_sub_slots.len(),
        )
        .expect("pre_sp_tx_block_height");
        validate_pospace_and_get_required_iters(
            &PrimitiveVerifier(&NativePrimitives),
            &MAINNET,
            &rcb.proof_of_space,
            if overflow { prev_challenge } else { challenge },
            cc_sp_hash,
            block.height(),
            DIFF,
            pre,
        )
        .expect("pospace")
        .expect("valid pospace")
    }

    fn build_records(chain: &[HeaderBlock]) -> Vec<BlockRecord> {
        let c = &MAINNET;
        let mut ancestors: HashMap<Bytes32, BlockRecord> = HashMap::new();
        let mut out: Vec<BlockRecord> = Vec::with_capacity(chain.len());
        let mut challenge = Some(chain[0].reward_chain_block.pos_ss_cc_challenge_hash);
        let mut prev_challenge: Option<Bytes32> = None;
        let mut prev_rec: Option<BlockRecord> = None;
        let mut deficit: u8 = 0;
        let mut tx_blocks: u32 = 0;
        let mut prev_tx_height: u32 = 0;

        for block in chain {
            let rcb = &block.reward_chain_block;
            let h = block.height();
            let mut overflow = false;
            let mut ses: Option<SubEpochSummary> = None;
            for ss in &block.finished_sub_slots {
                prev_challenge = Some(ss.challenge_chain.challenge_chain_end_of_slot_vdf.challenge);
                challenge = Some(ss.challenge_chain.hash().expect("cc hash"));
                deficit = ss.reward_chain.deficit;
                if let Some(ses_hash) = ss.challenge_chain.subepoch_summary_hash {
                    ses = Some(SubEpochSummary {
                        prev_subepoch_summary_hash: ses_hash,
                        reward_chain_hash: ses_hash,
                        num_blocks_overflow: 0,
                        new_difficulty: None,
                        new_sub_slot_iters: None,
                    });
                }
            }
            let mut required_iters = 0u64;
            if let (Some(ch), Some(pc)) = (challenge, prev_challenge)
                && tx_blocks > 2
            {
                overflow = is_overflow_block(c, rcb.signage_point_index).expect("overflow");
                deficit = calculate_deficit(
                    c,
                    h,
                    prev_rec.as_ref(),
                    overflow,
                    block.finished_sub_slots.len(),
                );
                required_iters = light_required_iters(&ancestors, block, ch, pc, overflow);
            }
            let rec = header_block_to_sub_block_record(
                c,
                required_iters,
                block,
                SSI,
                overflow,
                deficit,
                prev_tx_height,
                ses,
            )
            .expect("record");
            ancestors.insert(rec.header_hash, rec.clone());
            out.push(rec.clone());
            if rcb.is_transaction_block {
                tx_blocks += 1;
                prev_tx_height = h;
            }
            prev_rec = Some(rec);
        }
        out
    }

    // Project a finished mainnet header block back to the UnfinishedBlock a peer would have
    // relayed for it: strip the infusion-point VDFs, keep everything the farmer signed.
    fn unfinished_from_header(hb: &HeaderBlock) -> UnfinishedBlock {
        UnfinishedBlock {
            finished_sub_slots: hb.finished_sub_slots.clone(),
            reward_chain_block: hb.reward_chain_block.get_unfinished(),
            challenge_chain_sp_proof: hb.challenge_chain_sp_proof.clone(),
            reward_chain_sp_proof: hb.reward_chain_sp_proof.clone(),
            foliage: hb.foliage,
            foliage_transaction_block: hb.foliage_transaction_block,
            transactions_info: hb.transactions_info.clone(),
            transactions_generator: None,
            transactions_generator_ref_list: Vec::new(),
        }
    }

    // The deepest slice block matching `want_tx` at a plain (index > 0, non-overflow,
    // mid-sub-slot) signage point — deep ancestry below it, none of the first-in-slot special
    // cases in the way.
    fn pick_target(chain: &[HeaderBlock], want_tx: bool) -> usize {
        for i in (16..chain.len()).rev() {
            let b = &chain[i];
            let sp = b.reward_chain_block.signage_point_index;
            if b.foliage_transaction_block.is_some() == want_tx
                && b.finished_sub_slots.is_empty()
                && sp > 0
                && !is_overflow_block(&MAINNET, sp).expect("overflow")
            {
                return i;
            }
        }
        panic!("no suitable target block in the slice");
    }

    async fn node_with_slice_records() -> (Arc<Node<SqliteStore>>, Vec<HeaderBlock>) {
        let chain = load_chain();
        assert!(chain.len() > 280, "full slice present");
        let records = build_records(&chain);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db =
            std::env::temp_dir().join(format!("fn_ubgate_{}_{nanos}.sqlite", std::process::id()));
        let store = open_backend(&Backend::Sqlite(db)).await.expect("store");
        store
            .add_block_records(&records)
            .await
            .expect("seed records");
        let node = Arc::new(
            Node::boot_with_store(
                Config {
                    p2p: P2pSettings::default(),
                    listen: "127.0.0.1:0".parse().unwrap(),
                    rpc: "127.0.0.1:0".parse().unwrap(),
                    introducer: None,
                    manual_peers: Vec::new(),
                    advertise: None,
                    backend: Backend::Sqlite(std::path::PathBuf::from("unused")),
                    network_id: "mainnet".to_string(),
                    metrics: None,
                    capture_dir: None,
                    genesis_sync: false,
                    sync_from: 0,
                    uncompact: false,
                    prefetch_memory_mb: None,
                    prefetch_max_inflight: None,
                    trusted_peers: Vec::new(),
                    trusted_cidrs: Vec::new(),
                    rpc_tls: crate::config::RpcTlsMode::Local,
                    debug_endpoints: false,
                },
                store,
            )
            .expect("boot node"),
        );
        (node, chain)
    }

    async fn drive_one(node: &Arc<Node<SqliteStore>>, ub: UnfinishedBlock) {
        node.ub_inbox.lock().await.push(ub);
        process_ub_inbox(node).await;
    }

    async fn cached(node: &Arc<Node<SqliteStore>>, ub: &UnfinishedBlock) -> bool {
        let partial = ub.reward_chain_block.hash().expect("partial hash");
        let foliage = ub.foliage.foliage_transaction_block_hash;
        node.unfinished
            .lock()
            .await
            .get_block2(&partial, foliage.as_ref())
            .0
            .is_some()
    }

    #[tokio::test]
    async fn header_valid_ub_with_bogus_generator_is_dropped_not_cached_not_relayed() {
        let (node, chain) = node_with_slice_records().await;
        let target = &chain[pick_target(&chain, false)];
        let mut ub = unfinished_from_header(target);
        // A generator that is not even deserializable CLVM — the block is rejected without
        // ever reaching a peer.
        ub.transactions_generator = Some(SerializedProgram::from_hex("fffefd").expect("hex"));
        let poisoned = ub.clone();
        drive_one(&node, ub).await;

        assert!(
            !cached(&node, &poisoned).await,
            "a generator-invalid unfinished block must NOT enter the served cache"
        );
        assert_eq!(
            node.ub_announce.lock().await.len(),
            0,
            "a generator-invalid unfinished block must NOT be queued for relay"
        );
        assert_eq!(
            node.producer.dropped_count("ub_body_fail"),
            1,
            "the drop must be counted by the transactions gate (proves the header stage passed)"
        );
        assert_eq!(
            node.producer.dropped_count("ub_validation_fail"),
            0,
            "the header stage must not be the rejector (vacuity guard)"
        );
    }

    #[tokio::test]
    async fn tx_ub_with_generator_root_mismatch_is_dropped_not_relayed() {
        let (node, chain) = node_with_slice_records().await;
        let target = &chain[pick_target(&chain, true)];
        assert!(
            target.transactions_info.is_some(),
            "fixture shape: the recent-chain slice carries transactions_info for tx blocks"
        );
        let mut ub = unfinished_from_header(target);
        ub.transactions_generator = Some(SerializedProgram::from_hex("ff0880").expect("hex"));
        let poisoned = ub.clone();
        drive_one(&node, ub).await;

        assert!(
            !cached(&node, &poisoned).await,
            "must not enter the served cache"
        );
        assert_eq!(
            node.ub_announce.lock().await.len(),
            0,
            "must not be queued for relay"
        );
        assert_eq!(node.producer.dropped_count("ub_body_fail"), 1);
        assert_eq!(node.producer.dropped_count("ub_validation_fail"), 0);
    }

    #[tokio::test]
    async fn honest_ub_without_generator_still_validates_caches_and_announces() {
        let (node, chain) = node_with_slice_records().await;
        let target = &chain[pick_target(&chain, false)];
        let ub = unfinished_from_header(target);
        let honest = ub.clone();
        drive_one(&node, ub).await;

        assert!(
            cached(&node, &honest).await,
            "an honest unfinished block must still be cached (no false positive)"
        );
        let announces = node.ub_announce.lock().await;
        assert_eq!(announces.len(), 1, "exactly one relay announce queued");
        assert_eq!(
            announces[0].unfinished_reward_hash,
            honest.reward_chain_block.hash().expect("partial hash"),
        );
        drop(announces);
        for reason in ["ub_body_fail", "ub_generator_fail", "ub_cost_mismatch"] {
            assert_eq!(
                node.producer.dropped_count(reason),
                0,
                "no transactions-gate drop for an honest block ({reason})"
            );
        }
    }

    // The dedup ladder (seen set + get_unfinished_block2) sits in front of the
    // validators: an exact duplicate in the same drain is dropped by the seen set, and a NEW
    // serialization at an already-cached (reward, foliage) — e.g. the same block with a
    // generator spliced in — is dropped by the cache check BEFORE any validation runs. One
    // announce total: a burst of duplicates costs one header validation and one generator run
    // (the DoS bound).
    #[tokio::test]
    async fn duplicate_ubs_are_deduped_and_validated_once() {
        let (node, chain) = node_with_slice_records().await;
        let target = &chain[pick_target(&chain, false)];
        let ub = unfinished_from_header(target);
        let honest = ub.clone();
        // Two exact copies in one drain: the second dies on the seen set.
        node.ub_inbox.lock().await.push(ub.clone());
        node.ub_inbox.lock().await.push(ub);
        process_ub_inbox(&node).await;
        assert!(cached(&node, &honest).await);
        assert_eq!(
            node.ub_announce.lock().await.len(),
            1,
            "one announce, not two"
        );
        assert_eq!(node.producer.dropped_count("ub_duplicate"), 1);

        // A DIFFERENT serialization of the same (reward, foliage) — the cached block with a
        // bogus generator spliced in — dies on the already-cached check, before the header or
        // generator validators run (and without evicting the honest cached entry).
        let mut poisoned = honest.clone();
        poisoned.transactions_generator = Some(SerializedProgram::from_hex("fffefd").expect("hex"));
        drive_one(&node, poisoned).await;
        assert_eq!(node.producer.dropped_count("ub_already_cached"), 1);
        assert_eq!(node.producer.dropped_count("ub_body_fail"), 0);
        assert!(cached(&node, &honest).await, "the honest entry survives");
        assert_eq!(node.ub_announce.lock().await.len(), 1, "still one announce");
    }
}
