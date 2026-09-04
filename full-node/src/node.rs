use crate::config::{Backend, Config};
use crate::metrics::{HealthState, MetricsSources, ProducerMetrics};
use crate::rpc::Node;
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

mod candidate_builder;
mod listeners;
mod notifications;
mod peer_api;
mod runtime;
mod startup;
mod sync;
mod workers;
use candidate_builder::*;
pub use peer_api::outbound_on_connect;
use peer_api::*;
pub use startup::open_backend;
#[cfg(test)]
use sync::{
    ConfirmedPeak, RECOVERY_CHANNEL_CAP, RESET_REPLY_TIMEOUT, RecoveryRequest, await_reset,
    emit_confirmed_peak, follow_fill_claimed, frozen_frontier_is_wedge,
};
pub(crate) use sync::{reap_wallet_subscriptions_once, sync_driver, tip_follower};
use workers::*;

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

// Select consensus constants by network id. A fork's own constants would enter here (a later
// constants/genesis swap in core), touching no other crate boundary.
fn constants_for(network_id: &str) -> ConsensusConstants {
    match network_id {
        "testnet11" => TESTNET_11,
        _ => MAINNET,
    }
}

// The running node process: one shared store fanned out to the engine, Portfu state, and peer
// server, plus the mempool, wallet notifier, and sync state updated by the new-peak path.
pub struct FullNode<S = SqliteStore> {
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
    pub state: Arc<Node>,
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
    // Bounded one-shot index maintenance jobs. The Portfu-owned node handle aborts and drains
    // this set during shutdown, so no detached maintenance future outlives the server.
    maintenance_tasks: Mutex<tokio::task::JoinSet<()>>,
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
    // The INBOUND peer sessions map, owned by the FullNode so the confirm path can reach wallet-type
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
    // invariant (an in-engine capacity cap could evict a ref mid-window) - so the server caches
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

fn in_near_tip_band(local: u32, claimed: u32, has_peak: bool) -> bool {
    if !has_peak {
        return false;
    }
    let gap = claimed.saturating_sub(local);
    gap > 0 && gap <= SHORT_SYNC_BLOCKS_BEHIND_THRESHOLD
}

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
#[path = "../tests/unit/node/mod.rs"]
mod test_suites;
