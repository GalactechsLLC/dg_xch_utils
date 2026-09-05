use async_trait::async_trait;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::protocols::ban::BanCause;
use dg_xch_core::protocols::farmer::{DeclareProofOfSpace, RequestSignedValues, SignedValues};
use dg_xch_core::protocols::full_node::{
    NewCompactVDF, NewPeak, NewSignagePointOrEndOfSubSlot, NewTransaction, NewUnfinishedBlock,
    NewUnfinishedBlock2, RejectBlock, RejectBlocks, RequestBlock, RequestBlocks, RequestCompactVDF,
    RequestMempoolTransactions, RequestPeers, RequestProofOfWeight,
    RequestSignagePointOrEndOfSubSlot, RequestTransaction, RequestUnfinishedBlock,
    RequestUnfinishedBlock2, RespondBlock, RespondBlocks, RespondCompactVDF, RespondEndOfSubSlot,
    RespondPeers, RespondSignagePoint, RespondTransaction, RespondUnfinishedBlock,
};
use dg_xch_core::protocols::rate_limits_v3::{
    configure_message, peer_supports_v3, settings_from_configure,
};
use dg_xch_core::protocols::shared::{
    CAPABILITIES, Capability, ConfigureWindowSizes, ErrorMessage, Handshake,
};
use dg_xch_core::protocols::timelord::{
    NewEndOfSubSlotVDF, NewInfusionPointVDF, NewPeakTimelord, NewSignagePointVDF,
    RespondCompactProofOfTime,
};
use dg_xch_core::protocols::wallet::{
    CoinState, CoinStateUpdate, FeeEstimate, FeeEstimateGroup, FeeRate, NewPeakWallet,
    PuzzleSolutionResponse, RegisterForCoinUpdates, RegisterForPhUpdates, RejectAdditionsRequest,
    RejectBlockHeaders, RejectCoinState, RejectHeaderBlocks, RejectHeaderRequest,
    RejectPuzzleSolution, RejectPuzzleState, RejectRemovalsRequest, RejectStateReason,
    RequestAdditions, RequestBlockHeader, RequestBlockHeaders, RequestChildren, RequestCoinState,
    RequestFeeEstimates, RequestHeaderBlocks, RequestPuzzleSolution, RequestPuzzleState,
    RequestRemovals, RequestRemoveCoinSubscriptions, RequestRemovePuzzleSubscriptions,
    RespondAdditions, RespondBlockHeader, RespondBlockHeaders, RespondChildren, RespondCoinState,
    RespondFeeEstimates, RespondHeaderBlocks, RespondPuzzleSolution, RespondPuzzleState,
    RespondRemovals, RespondRemoveCoinSubscriptions, RespondRemovePuzzleSubscriptions,
    RespondToCoinUpdates, RespondToPhUpdates, SendTransaction, TransactionAck,
};
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::{Cursor, Error};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

/// What the dispatch layer should do with a `NewTransaction` announcement: ignore it, pull the
/// bundle, or ban the peer. The ban is a return value (not a side effect) so the handler stays
/// store-blind and loopback tests can assert the outcome without a live socket.
pub enum TransactionAnnounceAction {
    /// Not synced, already-seen-and-consistent, fee too low, or a duplicate in-flight pull.
    Ignore,
    /// New to us and worth fetching: send this `RequestTransaction` back to the announcer.
    Request(RequestTransaction),
    /// A protocol violation: a zero-cost announcement, or an already-seen tx whose advertised
    /// cost/fee disagrees with our validated mempool item. Close the connection.
    Ban,
}

// The full-node protocol surface, blind to the store and consensus: the node layer
// implements it, the handlers call it. Loopback tests back it with an in-memory map.
#[async_trait]
pub trait FullNodeApi: Send + Sync {
    async fn block_by_height(
        &self,
        height: u32,
    ) -> Option<Box<dg_xch_core::blockchain::full_block::FullBlock>>;
    // The RequestBlocks serving cap. The range check is over inclusive ends with the size compared
    // before the +1 bump, so a conforming node serves at most cap+1 = 33 blocks. The server
    // overrides this with its network constants.
    fn max_block_count_per_requests(&self) -> u32 {
        32
    }
    // The per-peer combined subscription cap. The store-blind default is the UNTRUSTED
    // config default (200,000); the server overrides it with the trusted-tier policy (200,000
    // untrusted / 2,000,000 trusted). The cap is applied DURING decode of the two register arms,
    // so it must be resolvable before the message body is parsed.
    fn max_subscriptions(&self, _peer: &Bytes32, _host: Option<IpAddr>) -> u32 {
        200_000
    }
    // `max_subscribe_response_items(peer)` — 100,000 untrusted, 500,000 trusted. The
    // decode-time list cap for `RequestCoinState.coin_ids`.
    fn max_subscribe_response_items(&self, _peer: &Bytes32, _host: Option<IpAddr>) -> u32 {
        100_000
    }
    // An inbound TIMELORD connection is accepted only from localhost or an exempt peer network.
    // The exempt network set ships empty, so this default is localhost only; the server widens
    // it with the trusted-CIDR list. An unresolved host cannot be localhost — refuse.
    fn accept_inbound_timelord(&self, host: Option<IpAddr>) -> bool {
        matches!(host, Some(h) if h.is_loopback())
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo>;
    async fn on_new_peak(&self, _peer: Bytes32, _peak: NewPeak) {}
    // A NewTransaction announcement; return the action the dispatch layer takes (pull the bundle,
    // ban the peer, or ignore).
    async fn on_new_transaction(
        &self,
        _peer: Bytes32,
        _tx: NewTransaction,
    ) -> TransactionAnnounceAction {
        TransactionAnnounceAction::Ignore
    }
    async fn transaction(&self, _id: Bytes32) -> Option<SpendBundle> {
        None
    }
    // `host` is the peer's remote IP; the node impl resolves the trusted tx-queue tier from it
    // (localhost / trusted-CIDR / trusted node-id).
    async fn on_respond_transaction(
        &self,
        _peer: Bytes32,
        _host: Option<IpAddr>,
        _tx: SpendBundle,
    ) {
    }
    // A signage-point / end-of-sub-slot announcement; return the request to pull it when the
    // node's slot state does not hold it yet, or None to ignore it.
    async fn on_new_signage_point_or_eos(
        &self,
        _peer: Bytes32,
        _ann: NewSignagePointOrEndOfSubSlot,
    ) -> Option<RequestSignagePointOrEndOfSubSlot> {
        None
    }
    // Serve a pull from the slot state: an EOS bundle when `index_from_challenge` is 0, a
    // signage point otherwise.
    async fn signage_point_or_eos(
        &self,
        _req: RequestSignagePointOrEndOfSubSlot,
    ) -> Option<SignagePointResponse> {
        None
    }
    async fn on_respond_signage_point(&self, _peer: Bytes32, _sp: RespondSignagePoint) {}
    async fn on_respond_end_of_sub_slot(&self, _peer: Bytes32, _eos: RespondEndOfSubSlot) {}
    // An unfinished-block announcement (v1: reward hash only / v2: + foliage hash); return the
    // matching request when the cache wants the block, or None to ignore it.
    async fn on_new_unfinished_block(
        &self,
        _peer: Bytes32,
        _ann: NewUnfinishedBlock,
    ) -> Option<RequestUnfinishedBlock> {
        None
    }
    async fn on_new_unfinished_block2(
        &self,
        _peer: Bytes32,
        _ann: NewUnfinishedBlock2,
    ) -> Option<RequestUnfinishedBlock2> {
        None
    }
    // Serve a cached unfinished block: v1 returns the best variant for the reward hash, v2 the
    // exact (reward hash, foliage hash) variant.
    async fn unfinished_block(&self, _reward_hash: Bytes32) -> Option<Box<UnfinishedBlock>> {
        None
    }
    async fn unfinished_block2(
        &self,
        _reward_hash: Bytes32,
        _foliage_hash: Option<Bytes32>,
    ) -> Option<Box<UnfinishedBlock>> {
        None
    }
    async fn on_respond_unfinished_block(&self, _block: Box<UnfinishedBlock>) {}
    // SERVE: answer a peer's RequestCompactVDF with OUR stored proof for the field only when we
    // already hold it compact; None stays silent.
    async fn compact_vdf(&self, _req: RequestCompactVDF) -> Option<RespondCompactVDF> {
        None
    }
    // CONSUME step 1: a peer announces it holds a compact proof for a block field; return the
    // RequestCompactVDF to pull it when we still hold that field bulky, or None to ignore
    // (already compact / too recent / not ours).
    async fn on_new_compact_vdf(
        &self,
        _peer: Bytes32,
        _ann: NewCompactVDF,
    ) -> Option<RequestCompactVDF> {
        None
    }
    // CONSUME step 2: the pulled compact proof. The implementation validates + swaps it into the
    // stored block off the read path (never validate a VDF on the websocket loop) and re-gossips
    // NewCompactVDF.
    async fn on_respond_compact_vdf(&self, _peer: Bytes32, _resp: RespondCompactVDF) {}
    // CONSUME the bluebox return: a timelord we solicited with RequestCompactProofOfTime returns
    // the compact proof for the field. Same fields as RespondCompactVDF, so the implementation
    // validates + swaps + re-gossips through the identical consume path — off the read loop.
    // Default no-op for nodes that never solicit.
    async fn on_respond_compact_proof_of_time(
        &self,
        _peer: Bytes32,
        _resp: RespondCompactProofOfTime,
    ) {
    }
    // Serve a RequestMempoolTransactions: up to 100 resident items as NewTransaction
    // announcements (the peer pulls what it lacks through the normal announce path).
    async fn mempool_items(&self, _filter: Vec<u8>) -> Vec<NewTransaction> {
        Vec::new()
    }
    // A RequestProofOfWeight: building the proof walks sub-epochs of store history, so the
    // implementation must only QUEUE {peer, tip, id} for a worker and return — never build here.
    // The worker builds (single-flight per tip) and responds through the handed peer map with the
    // request id, so the requester's oneshot on RespondProofOfWeight matches. Refusals (unknown
    // tip / short chain) also live in the worker: it simply sends nothing.
    async fn on_request_proof_of_weight(
        &self,
        _peer: Bytes32,
        _req: RequestProofOfWeight,
        _id: Option<u16>,
        _peers: PeerMap,
    ) {
    }
    async fn on_respond_peers(&self, _peers: Vec<TimestampedPeerInfo>) {}
    // FARMER→NODE: a harvester found a proof for a signage point we announced. The implementation
    // gates on sync, validates the proof, assembles the candidate unfinished block, and returns
    // the `RequestSignedValues` for the farmer to sign the two foliage hashes. Returns `None` when
    // the proof is rejected or the candidate cannot yet be assembled.
    async fn on_declare_proof_of_space(
        &self,
        _peer: Bytes32,
        _declare: DeclareProofOfSpace,
    ) -> Option<RequestSignedValues> {
        None
    }
    // FARMER→NODE: the farmer's signatures over the foliage we asked it to sign. Consumed during
    // block assembly.
    async fn on_signed_values(&self, _peer: Bytes32, _signed: SignedValues) {}
    // TIMELORD→NODE: the infusion-point VDFs that finish one of OUR cached unfinished blocks into
    // a `FullBlock`. The implementation only QUEUES the request for the driver — a VDF-infused
    // block is never assembled on the websocket read loop. Gated on sync by the implementation.
    async fn on_new_infusion_point_vdf(&self, _peer: Bytes32, _req: NewInfusionPointVDF) {}
    // TIMELORD→NODE: a signage-point VDF the timelord produced; routed to the same slot-state
    // validation inbox `on_respond_signage_point` feeds. Gated on sync.
    async fn on_new_signage_point_vdf(&self, _peer: Bytes32, _req: NewSignagePointVDF) {}
    // TIMELORD→NODE: an end-of-sub-slot the timelord produced; routed to the same slot-state
    // validation inbox `on_respond_end_of_sub_slot` feeds. Gated on sync.
    async fn on_new_end_of_sub_slot_vdf(&self, _peer: Bytes32, _req: NewEndOfSubSlotVDF) {}

    // ---- light-wallet query surface (served against the coin/block store) ----------------------

    // WALLET→NODE: the (puzzle, solution) of a coin spent at `height`, recovered by re-running
    // that block's generator. `None` maps to a `RejectPuzzleSolution` on the wire (unknown coin /
    // wrong height / no generator).
    async fn puzzle_solution(
        &self,
        _coin_name: Bytes32,
        _height: u32,
    ) -> Option<PuzzleSolutionResponse> {
        None
    }

    // WALLET→NODE (code 48): a spend bundle submitted for mempool admission. ALWAYS answered with
    // a TransactionAck — a submit is never silently dropped (wallets block on this ack). The
    // implementation gates on sync, shares the push_tx admission seam (validate → admit →
    // announce), and maps the outcome to SUCCESS(1)/PENDING(2)/FAILED(3). The store-blind default
    // acks the not-synced reject — a node that cannot validate must not claim success.
    async fn send_transaction(&self, _peer: Bytes32, tx: SendTransaction) -> TransactionAck {
        TransactionAck {
            txid: tx.transaction.name().unwrap_or_default(),
            status: TXStatus::FAILED,
            error: Some("NO_TRANSACTIONS_WHILE_SYNCING".to_string()),
        }
    }

    // WALLET→NODE: the HeaderBlock at a height.
    async fn block_header(&self, _height: u32) -> BlockHeaderReply {
        BlockHeaderReply::Silent
    }

    // WALLET→NODE (code 60): header blocks in [start, end].
    async fn header_blocks(&self, _start_height: u32, _end_height: u32) -> HeaderBlocksReply {
        HeaderBlocksReply::Silent
    }

    // WALLET→NODE (code 86): header blocks in [start, end]. The default rejects (a store-blind
    // impl serves nothing); the real node overrides it.
    async fn block_headers(
        &self,
        start_height: u32,
        end_height: u32,
        _return_filter: bool,
    ) -> BlockHeadersReply {
        BlockHeadersReply::Reject(RejectBlockHeaders {
            start_height,
            end_height,
        })
    }

    // WALLET→NODE: coins created at a block, grouped by puzzle hash. The default rejects; the
    // real node overrides it.
    async fn additions(&self, req: RequestAdditions) -> AdditionsReply {
        AdditionsReply::Reject(RejectAdditionsRequest {
            height: req.height,
            header_hash: req.header_hash.unwrap_or_default(),
        })
    }

    // WALLET→NODE: coins spent at a block. The default rejects; the real node overrides it.
    async fn removals(&self, req: RequestRemovals) -> RemovalsReply {
        RemovalsReply::Reject(RejectRemovalsRequest {
            height: req.height,
            header_hash: req.header_hash,
        })
    }

    // WALLET→NODE: the coin states of every child of a coin (spent and unspent). An empty vec is
    // a valid answer (no children yet); always responds.
    async fn children(&self, _coin_name: Bytes32) -> Vec<CoinState> {
        Vec::new()
    }

    // WALLET→NODE: subscribe the peer to puzzle-hash coin updates AND return the initial matching
    // CoinState set. The default is store-blind (empty state, no subscription); the real node
    // overrides it.
    async fn register_for_ph_updates(
        &self,
        _peer: Bytes32,
        _host: Option<IpAddr>,
        req: RegisterForPhUpdates,
    ) -> PhRegistration {
        PhRegistration {
            response: RespondToPhUpdates {
                puzzle_hashes: req.puzzle_hashes,
                min_height: req.min_height,
                coin_states: Vec::new(),
            },
            receiver: None,
        }
    }

    // WALLET→NODE: subscribe the peer to coin-id updates AND return the initial matching
    // CoinState set. Default store-blind.
    async fn register_for_coin_updates(
        &self,
        _peer: Bytes32,
        _host: Option<IpAddr>,
        req: RegisterForCoinUpdates,
    ) -> CoinRegistration {
        CoinRegistration {
            response: RespondToCoinUpdates {
                coin_ids: req.coin_ids,
                min_height: req.min_height,
                coin_states: Vec::new(),
            },
            receiver: None,
        }
    }

    // ---- the modern wallet-sync surface (codes 94-103) ----------------------------------------

    // WALLET→NODE (code 98): the PAGED spent+unspent coin history of a set of puzzle hashes (plus
    // hinted coins), with a reorg-consistency check against the requester's previous peak and an
    // optional subscribe-on-finish side effect. The store-blind default rejects REORG — a node
    // that cannot resolve heights against a chain must not claim a consistent answer.
    async fn puzzle_state(
        &self,
        _peer: Bytes32,
        _host: Option<IpAddr>,
        _req: RequestPuzzleState,
    ) -> PuzzleStateReply {
        PuzzleStateReply::Reject(RejectStateReason::REORG)
    }

    // WALLET→NODE (code 101): the coin states of a set of coin ids above the requester's previous
    // peak, same reorg-consistency check, optional subscribe side effect. Store-blind default
    // rejects REORG.
    async fn coin_state(
        &self,
        _peer: Bytes32,
        _host: Option<IpAddr>,
        _req: RequestCoinState,
    ) -> CoinStateReply {
        CoinStateReply::Reject(RejectStateReason::REORG)
    }

    // WALLET→NODE (code 89): a fee-rate estimate for each requested epoch timestamp. ALWAYS
    // answers one FeeEstimate per requested time — a node with no history returns rate 0, never
    // an error group. The store-blind default returns the floor (rate 0) for every requested
    // time; the real node reads the mempool's fee estimator.
    async fn fee_estimates(&self, req: RequestFeeEstimates) -> FeeEstimateGroup {
        FeeEstimateGroup {
            error: None,
            estimates: req
                .time_targets
                .iter()
                .map(|&t| FeeEstimate {
                    error: None,
                    time_target: t,
                    estimated_fee_rate: FeeRate {
                        mojos_per_clvm_cost: 0,
                    },
                })
                .collect(),
        }
    }

    // WALLET→NODE (code 94): drop the peer's puzzle-hash subscriptions — `None` = ALL (returning
    // the prior set), `Some` = the listed subset (returning what was actually removed).
    async fn remove_puzzle_subscriptions(
        &self,
        _peer: Bytes32,
        _puzzle_hashes: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        Vec::new()
    }

    // WALLET→NODE (code 96): the coin-id counterpart.
    async fn remove_coin_subscriptions(
        &self,
        _peer: Bytes32,
        _coin_ids: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        Vec::new()
    }

    // NODE→WALLET greeting: the current peak, sent to a WALLET-type peer the moment its handshake
    // completes — fork_point_with_previous_peak is the peak height itself on connect. `None`
    // (store-blind default / no peak yet) sends nothing. Sage drops any peer that stays silent
    // for 2s after the handshake, so this greeting is what keeps a wallet connection alive at all.
    async fn wallet_peak(&self) -> Option<NewPeakWallet> {
        None
    }
    // NODE→FULL_NODE greeting: the current peak sent to a new FULL_NODE peer, fork point = the
    // peak height itself on connect (same convention as the wallet greeting). `None` (no peak /
    // blind api) sends nothing.
    async fn full_node_peak(&self) -> Option<NewPeak> {
        None
    }
    // NODE→TIMELORD greeting: a fresh timelord starts infusing on top of our peak immediately
    // instead of idling until the next peak advance. `None` (no peak, or the peak's ancestry
    // cannot support the difficulty/challenge walks) sends nothing. Boxed: NewPeakTimelord
    // carries a full RewardChainBlock and would bloat every blind-api vtable copy otherwise.
    async fn timelord_peak(&self) -> Option<Box<NewPeakTimelord>> {
        None
    }
    // Mempool sync on connect: when WE are synced, a new FULL_NODE peer is sent
    // `RequestMempoolTransactions` carrying the BIP158 filter over OUR mempool item ids — the
    // peer answers with the transactions we are missing (new peers announce via NewTransaction,
    // pre-2.6.0 peers push RespondTransaction directly; both land in the normal admission seam).
    // `None` = not synced — nothing is requested.
    async fn mempool_sync_filter(&self) -> Option<Vec<u8>> {
        None
    }
}

// Bridge a subscribed peer's bounded `CoinStateUpdate` receiver to the wire: one task per peer,
// spawned only on the FIRST registration. Ends on its own when the channel closes (disconnect
// reconciliation drops the `Sender`) or on a socket send error; bounded by the subscriber cap.
fn spawn_coin_state_forwarder(
    counters: Arc<NetCounters>,
    peers: PeerMap,
    peer_id: Bytes32,
    mut rx: tokio::sync::mpsc::Receiver<CoinStateUpdate>,
    version: ChiaProtocolVersion,
) {
    tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            if send(
                &counters,
                &peers,
                &peer_id,
                ProtocolMessageTypes::CoinStateUpdate,
                &update,
                None,
                version,
            )
            .await
            .is_err()
            {
                break;
            }
        }
    });
}

// What `signage_point_or_eos` serves back — the two message types a
// RequestSignagePointOrEndOfSubSlot can be answered with.
pub enum SignagePointResponse {
    SignagePoint(Box<RespondSignagePoint>),
    EndOfSubSlot(Box<RespondEndOfSubSlot>),
}

// ---- light-wallet query replies ------------------------------------------------------------------------
// Each wallet request is answered with EXACTLY ONE of a Respond* / Reject* message, or (for the
// two handlers that stay quiet on a missing body / bad range) no reply at all. Modeling the choice
// as an enum keeps the FullNodeApi implementation store-blind and lets the dispatch layer own the
// wire send — the same shape as `SignagePointResponse` above.

/// `request_block_header`: the `HeaderBlock` at a height, a `RejectHeaderRequest` when the height
/// is not in the main chain, or silence when the record exists but the block body does not.
pub enum BlockHeaderReply {
    Respond(Box<HeaderBlock>),
    Reject(u32),
    Silent,
}

/// `request_header_blocks` (the DEPRECATED shape, code 60): the header blocks in `[start, end]`,
/// a `RejectHeaderBlocks` when a height in the range is unknown, or silence on a bad range.
pub enum HeaderBlocksReply {
    Respond(Box<RespondHeaderBlocks>),
    Reject(RejectHeaderBlocks),
    Silent,
}

/// `request_block_headers` (the streamed shape, code 86): the header blocks in `[start, end]` or
/// a `RejectBlockHeaders` (bad range / missing body). This handler never stays silent — a bad
/// range is a `Reject`.
pub enum BlockHeadersReply {
    Respond(Box<RespondBlockHeaders>),
    Reject(RejectBlockHeaders),
}

/// `request_additions`: coins created at a block grouped by puzzle hash, or a
/// `RejectAdditionsRequest` (fork / too many hashes / unknown height).
pub enum AdditionsReply {
    Respond(Box<RespondAdditions>),
    Reject(RejectAdditionsRequest),
}

/// `request_removals`: coins spent at a block, or a `RejectRemovalsRequest` (not a tx block /
/// fork / height mismatch).
pub enum RemovalsReply {
    Respond(Box<RespondRemovals>),
    Reject(RejectRemovalsRequest),
}

/// The result of a `RegisterForPhUpdates`: the initial matching
/// `CoinState` set the wallet gets synchronously, plus — on the peer's FIRST registration — the
/// bounded delivery receiver the dispatch layer bridges to the socket as a live `CoinStateUpdate`
/// forwarder. `receiver` is `None` on a repeat registration (one channel per peer).
pub struct PhRegistration {
    pub response: RespondToPhUpdates,
    pub receiver: Option<tokio::sync::mpsc::Receiver<CoinStateUpdate>>,
}

/// The result of a `RegisterForCoinUpdates`. See [`PhRegistration`].
pub struct CoinRegistration {
    pub response: RespondToCoinUpdates,
    pub receiver: Option<tokio::sync::mpsc::Receiver<CoinStateUpdate>>,
}

/// `request_puzzle_state` (code 98): one page of puzzle-hash coin
/// states, or a `RejectPuzzleState` carrying the `RejectStateReason` (REORG on a
/// previous-peak mismatch / unresolvable chain, EXCEEDED_SUBSCRIPTION_LIMIT on an over-cap
/// subscribe). The `Respond` arm carries the peer's delivery receiver on its FIRST
/// subscribe-on-finish (the dispatch layer bridges it to the socket, exactly like
/// [`PhRegistration`]).
pub enum PuzzleStateReply {
    Respond(
        Box<RespondPuzzleState>,
        Option<tokio::sync::mpsc::Receiver<CoinStateUpdate>>,
    ),
    Reject(RejectStateReason),
}

/// `request_coin_state` (code 101). See [`PuzzleStateReply`].
pub enum CoinStateReply {
    Respond(
        Box<RespondCoinState>,
        Option<tokio::sync::mpsc::Receiver<CoinStateUpdate>>,
    ),
    Reject(RejectStateReason),
}

// Peer-link traffic counters, shared by every handler map and the server's broadcast paths (one
// per node). Message counts are per-type; byte totals cover the whole link. Mutex-per-count is
// fine at protocol rates.
#[derive(Default)]
pub struct NetCounters {
    pub messages_in: std::sync::Mutex<HashMap<&'static str, u64>>,
    pub messages_out: std::sync::Mutex<HashMap<&'static str, u64>>,
    pub bytes_in: std::sync::atomic::AtomicU64,
    pub bytes_out: std::sync::atomic::AtomicU64,
}

// The stable label for a message type (lowercase snake case, the protocol's own names).
fn msg_label(t: ProtocolMessageTypes) -> &'static str {
    match t {
        ProtocolMessageTypes::Handshake => "handshake",
        ProtocolMessageTypes::NewPeak => "new_peak",
        ProtocolMessageTypes::NewTransaction => "new_transaction",
        ProtocolMessageTypes::RequestTransaction => "request_transaction",
        ProtocolMessageTypes::RespondTransaction => "respond_transaction",
        ProtocolMessageTypes::RequestBlock => "request_block",
        ProtocolMessageTypes::RespondBlock => "respond_block",
        ProtocolMessageTypes::RequestBlocks => "request_blocks",
        ProtocolMessageTypes::RespondBlocks => "respond_blocks",
        ProtocolMessageTypes::RejectBlock => "reject_block",
        ProtocolMessageTypes::RejectBlocks => "reject_blocks",
        ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot => {
            "new_signage_point_or_end_of_sub_slot"
        }
        ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot => {
            "request_signage_point_or_end_of_sub_slot"
        }
        ProtocolMessageTypes::RespondSignagePoint => "respond_signage_point",
        ProtocolMessageTypes::RespondEndOfSubSlot => "respond_end_of_sub_slot",
        ProtocolMessageTypes::NewUnfinishedBlock => "new_unfinished_block",
        ProtocolMessageTypes::NewUnfinishedBlock2 => "new_unfinished_block2",
        // Every type the node sends or serves must carry its own label.
        ProtocolMessageTypes::RequestUnfinishedBlock => "request_unfinished_block",
        ProtocolMessageTypes::RequestUnfinishedBlock2 => "request_unfinished_block2",
        ProtocolMessageTypes::RespondUnfinishedBlock => "respond_unfinished_block",
        ProtocolMessageTypes::NewCompactVdf => "new_compact_vdf",
        ProtocolMessageTypes::RequestCompactVdf => "request_compact_vdf",
        ProtocolMessageTypes::RespondCompactVdf => "respond_compact_vdf",
        ProtocolMessageTypes::RequestPeers => "request_peers",
        ProtocolMessageTypes::RespondPeers => "respond_peers",
        ProtocolMessageTypes::RequestMempoolTransactions => "request_mempool_transactions",
        ProtocolMessageTypes::RequestProofOfWeight => "request_proof_of_weight",
        ProtocolMessageTypes::RespondProofOfWeight => "respond_proof_of_weight",
        ProtocolMessageTypes::NewSignagePoint => "new_signage_point",
        ProtocolMessageTypes::DeclareProofOfSpace => "declare_proof_of_space",
        ProtocolMessageTypes::RequestSignedValues => "request_signed_values",
        ProtocolMessageTypes::SignedValues => "signed_values",
        ProtocolMessageTypes::NewInfusionPointVdf => "new_infusion_point_vdf",
        ProtocolMessageTypes::NewSignagePointVdf => "new_signage_point_vdf",
        ProtocolMessageTypes::NewEndOfSubSlotVdf => "new_end_of_sub_slot_vdf",
        ProtocolMessageTypes::RequestCompactProofOfTime => "request_compact_proof_of_time",
        ProtocolMessageTypes::RespondCompactProofOfTime => "respond_compact_proof_of_time",
        // Light-wallet query surface (in + out labels for the gossip-health signal).
        ProtocolMessageTypes::SendTransaction => "send_transaction",
        ProtocolMessageTypes::TransactionAck => "transaction_ack",
        ProtocolMessageTypes::RequestPuzzleSolution => "request_puzzle_solution",
        ProtocolMessageTypes::RespondPuzzleSolution => "respond_puzzle_solution",
        ProtocolMessageTypes::RejectPuzzleSolution => "reject_puzzle_solution",
        ProtocolMessageTypes::RequestBlockHeader => "request_block_header",
        ProtocolMessageTypes::RespondBlockHeader => "respond_block_header",
        ProtocolMessageTypes::RejectHeaderRequest => "reject_header_request",
        ProtocolMessageTypes::RequestHeaderBlocks => "request_header_blocks",
        ProtocolMessageTypes::RespondHeaderBlocks => "respond_header_blocks",
        ProtocolMessageTypes::RejectHeaderBlocks => "reject_header_blocks",
        ProtocolMessageTypes::RequestBlockHeaders => "request_block_headers",
        ProtocolMessageTypes::RespondBlockHeaders => "respond_block_headers",
        ProtocolMessageTypes::RejectBlockHeaders => "reject_block_headers",
        ProtocolMessageTypes::RequestAdditions => "request_additions",
        ProtocolMessageTypes::RespondAdditions => "respond_additions",
        ProtocolMessageTypes::RejectAdditionsRequest => "reject_additions_request",
        ProtocolMessageTypes::RequestRemovals => "request_removals",
        ProtocolMessageTypes::RespondRemovals => "respond_removals",
        ProtocolMessageTypes::RejectRemovalsRequest => "reject_removals_request",
        ProtocolMessageTypes::RequestChildren => "request_children",
        ProtocolMessageTypes::RespondChildren => "respond_children",
        ProtocolMessageTypes::RegisterInterestInPuzzleHash => "register_interest_in_puzzle_hash",
        ProtocolMessageTypes::RespondToPhUpdate => "respond_to_ph_update",
        ProtocolMessageTypes::RegisterInterestInCoin => "register_interest_in_coin",
        ProtocolMessageTypes::RespondToCoinUpdate => "respond_to_coin_update",
        ProtocolMessageTypes::CoinStateUpdate => "coin_state_update",
        ProtocolMessageTypes::NewPeakWallet => "new_peak_wallet",
        ProtocolMessageTypes::RequestPuzzleState => "request_puzzle_state",
        ProtocolMessageTypes::RespondPuzzleState => "respond_puzzle_state",
        ProtocolMessageTypes::RejectPuzzleState => "reject_puzzle_state",
        ProtocolMessageTypes::RequestCoinState => "request_coin_state",
        ProtocolMessageTypes::RespondCoinState => "respond_coin_state",
        ProtocolMessageTypes::RejectCoinState => "reject_coin_state",
        ProtocolMessageTypes::RequestRemovePuzzleSubscriptions => {
            "request_remove_puzzle_subscriptions"
        }
        ProtocolMessageTypes::RespondRemovePuzzleSubscriptions => {
            "respond_remove_puzzle_subscriptions"
        }
        ProtocolMessageTypes::RequestRemoveCoinSubscriptions => "request_remove_coin_subscriptions",
        ProtocolMessageTypes::RespondRemoveCoinSubscriptions => "respond_remove_coin_subscriptions",
        ProtocolMessageTypes::RequestFeeEstimates => "request_fee_estimates",
        ProtocolMessageTypes::RespondFeeEstimates => "respond_fee_estimates",
        _ => "other",
    }
}

impl NetCounters {
    pub fn count_in(&self, t: ProtocolMessageTypes, bytes: usize) {
        *self
            .messages_in
            .lock()
            .expect("net counter lock")
            .entry(msg_label(t))
            .or_insert(0) += 1;
        self.bytes_in
            .fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn count_out(&self, t: ProtocolMessageTypes, bytes: usize) {
        *self
            .messages_out
            .lock()
            .expect("net counter lock")
            .entry(msg_label(t))
            .or_insert(0) += 1;
        self.bytes_out
            .fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

async fn peer_version(peers: &PeerMap, peer_id: &Bytes32) -> ChiaProtocolVersion {
    if let Some(peer) = peers.read().await.get(peer_id).cloned() {
        *peer.protocol_version.read().await
    } else {
        ChiaProtocolVersion::default()
    }
}

// The peer's captured remote IP. `None` when the peer is gone from the map or its remote addr was
// never resolved; a `None` host simply cannot grant host-based trust (node-id trust still applies).
async fn peer_host(peers: &PeerMap, peer_id: &Bytes32) -> Option<IpAddr> {
    peers.read().await.get(peer_id).and_then(|peer| peer.host)
}

// The peer's handshake-recorded node type. `Unknown`
// when the peer is gone from the map or never completed a handshake.
async fn peer_node_type(peers: &PeerMap, peer_id: &Bytes32) -> NodeType {
    if let Some(peer) = peers.read().await.get(peer_id) {
        *peer.node_type.read().await
    } else {
        NodeType::Unknown
    }
}

/// Enforce the sender-type rule for the timelord-class inbound messages: new_infusion_point_vdf
/// / new_signage_point_vdf / new_end_of_sub_slot_vdf / respond_compact_proof_of_time are sent
/// only by a TIMELORD. A mismatch closes the connection with the short (10 s) protocol ban. The
/// check runs BEFORE the body is decoded, so a mismatched sender never buys decode CPU.
/// Returns `true` when the sender is a timelord and dispatch may proceed.
async fn require_timelord_sender(
    peers: &PeerMap,
    peer_id: &Bytes32,
    msg_type: ProtocolMessageTypes,
) -> Result<bool, Error> {
    if peer_node_type(peers, peer_id).await == NodeType::Timelord {
        return Ok(true);
    }
    log::warn!(
        "API call type mismatch: peer {peer_id} is not a TIMELORD but sent {msg_type:?}; \
         closing with the short protocol ban (chia allowed_senders check)"
    );
    close_peer(peers, peer_id, Some(BanCause::InvalidProtocol)).await?;
    Ok(false)
}

async fn send<T: ChiaSerialize>(
    counters: &NetCounters,
    peers: &PeerMap,
    peer_id: &Bytes32,
    msg_type: ProtocolMessageTypes,
    body: &T,
    id: Option<u16>,
    version: ChiaProtocolVersion,
) -> Result<(), Error> {
    if let Some(peer) = peers.read().await.get(peer_id).cloned() {
        let bytes = ChiaMessage::new(msg_type, version, body, id)?.to_bytes(version)?;
        counters.count_out(msg_type, bytes.len());
        peer.websocket
            .write()
            .await
            .send(Message::Binary(bytes.into()))
            .await?;
    }
    Ok(())
}

fn decode<T: ChiaSerialize>(msg: &ChiaMessage, version: ChiaProtocolVersion) -> Result<T, Error> {
    T::from_bytes(&mut Cursor::new(msg.data.as_slice()), version)
}

pub const MAX_PUZZLE_HASH_BATCH_SIZE: u32 = 32_700 - 10;

/// Close a misbehaving peer's connection. All three halves of the close: (1) enter the peer's
/// REMOTE host into the timed ban list for `cause`'s duration, so a
/// reconnect within the window is refused at the accept path — read off the peer's injected
/// registry + captured host (`None` on a link without either is a no-op, e.g. an outbound client);
/// (2) evict the peer from the shared map NOW, so the ban is immediate and the map stays bounded
/// even if the socket teardown lags; (3) send a WebSocket Close frame. A missing peer (already
/// gone) is a no-op success. `cause == None` closes without banning.
async fn close_peer(
    peers: &PeerMap,
    peer_id: &Bytes32,
    cause: Option<BanCause>,
) -> Result<(), Error> {
    let peer = peers.write().await.remove(peer_id);
    if let Some(peer) = peer {
        if let (Some(cause), Some(bans), Some(host)) = (cause, peer.bans.as_ref(), peer.host) {
            bans.ban(host, cause);
        }
        peer.websocket.write().await.close(None).await?;
    }
    Ok(())
}

pub struct FullNodeHandler {
    pub api: Arc<dyn FullNodeApi>,
    pub network_id: String,
    pub server_port: u16,
    // Reply to an inbound Handshake with our own (server role). False on an outbound link where WE
    // initiated the handshake via WsClient::perform_handshake — receiving the peer's reply must record
    // the negotiated version but NOT emit a second handshake (which the peer would reject/close on).
    pub respond_handshake: bool,
    // Per-link traffic counters; the server shares one instance across every handler map so
    // /metrics sees the whole node's I/O.
    pub counters: Arc<NetCounters>,
}

impl FullNodeHandler {
    async fn dispatch(
        &self,
        msg: &ChiaMessage,
        peer_id: &Bytes32,
        peers: &PeerMap,
        version: ChiaProtocolVersion,
    ) -> Result<(), Error> {
        match msg.msg_type {
            ProtocolMessageTypes::Handshake => {
                let hs = decode::<Handshake>(msg, version)?;
                let negotiated = ChiaProtocolVersion::from_str(&hs.protocol_version)
                    .expect("ChiaProtocolVersion::from_str is Infallible");
                if let Some(peer) = peers.read().await.get(peer_id).cloned() {
                    *peer.node_type.write().await = NodeType::from(hs.node_type);
                    *peer.protocol_version.write().await = negotiated;
                    // Record the peer's advertised capabilities so the read loop's inbound rate
                    // limiter selects the v1/v2 numbers this peer negotiated. Server-role links
                    // learn caps here; outbound links record them in `WsClient::build` after the
                    // oneshot handshake.
                    *peer.capabilities.write().await = hs.capabilities.clone();
                }
                if !self.respond_handshake {
                    // Outbound link: we already initiated the handshake; only record the negotiated
                    // version above. Emitting a reply here would be a duplicate mid-stream handshake.
                    return Ok(());
                }
                let peer_is_v3 = peer_supports_v3(&hs.capabilities);
                let mut reply_caps: dg_xch_core::protocols::shared::Capabilities = CAPABILITIES
                    .iter()
                    .map(|e| (e.0, e.1.to_string()))
                    .collect();
                if peer_is_v3 {
                    reply_caps.push((Capability::RateLimitsV3 as u16, "1".to_string()));
                }
                let reply = Handshake {
                    network_id: self.network_id.clone(),
                    protocol_version: negotiated.to_string(),
                    software_version: dg_xch_clients::websocket::version(),
                    server_port: self.server_port,
                    node_type: NodeType::FullNode as u8,
                    capabilities: reply_caps,
                };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::Handshake,
                    &reply,
                    msg.id,
                    negotiated,
                )
                .await?;
                if peer_is_v3 {
                    // Immediately after the handshake: our ConfigureWindowSizes — each side
                    // sends its settings and validates the peer's. Activation completes
                    // when the peer's configure arrives (the dispatch arm below).
                    if let Some(peer) = peers.read().await.get(peer_id) {
                        peer.v3.offer();
                    }
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::ConfigureWindowSizes,
                        &configure_message(),
                        None,
                        negotiated,
                    )
                    .await?;
                }
                // An inbound TIMELORD is accepted only from localhost or an exempt network. The
                // refusal happens right after the handshake completes and BEFORE any greeting,
                // and closes WITHOUT banning.
                if NodeType::from(hs.node_type) == NodeType::Timelord {
                    let host = peer_host(peers, peer_id).await;
                    if !self.api.accept_inbound_timelord(host) {
                        log::info!(
                            "Not accepting inbound TIMELORD connection from {host:?}: \
                             localhost/exempt networks only"
                        );
                        return close_peer(peers, peer_id, None).await;
                    }
                }
                // Greet the new peer by type the moment its handshake completes. No peak
                // (empty store / blind api) sends nothing.
                match NodeType::from(hs.node_type) {
                    // NewPeak of the current peak, then (when synced) the mempool-sync request
                    // carrying OUR BIP158 filter; the peer answers with the transactions we are
                    // missing.
                    NodeType::FullNode => {
                        if let Some(peak) = self.api.full_node_peak().await {
                            send(
                                &self.counters,
                                peers,
                                peer_id,
                                ProtocolMessageTypes::NewPeak,
                                &peak,
                                None,
                                negotiated,
                            )
                            .await?;
                        }
                        if let Some(filter) = self.api.mempool_sync_filter().await {
                            send(
                                &self.counters,
                                peers,
                                peer_id,
                                ProtocolMessageTypes::RequestMempoolTransactions,
                                &RequestMempoolTransactions { filter },
                                None,
                                negotiated,
                            )
                            .await?;
                        }
                    }
                    // A WALLET peer is greeted with the current peak as NewPeakWallet. Sage drops
                    // any peer that has not produced exactly this message within 2s of connecting.
                    NodeType::Wallet => {
                        if let Some(peak) = self.api.wallet_peak().await {
                            send(
                                &self.counters,
                                peers,
                                peer_id,
                                ProtocolMessageTypes::NewPeakWallet,
                                &peak,
                                None,
                                negotiated,
                            )
                            .await?;
                        }
                    }
                    // A fresh timelord starts infusing on top of our peak immediately.
                    NodeType::Timelord => {
                        if let Some(peak) = self.api.timelord_peak().await {
                            send(
                                &self.counters,
                                peers,
                                peer_id,
                                ProtocolMessageTypes::NewPeakTimelord,
                                &*peak,
                                None,
                                negotiated,
                            )
                            .await?;
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            ProtocolMessageTypes::NewPeak => {
                self.api.on_new_peak(*peer_id, decode(msg, version)?).await;
                Ok(())
            }
            ProtocolMessageTypes::RequestBlock => {
                let req = decode::<RequestBlock>(msg, version)?;
                match self.api.block_by_height(req.height).await {
                    Some(mut block) => {
                        // A headers-only pull (include_transaction_block=false) strips ONLY
                        // transactions_generator; transactions_info and
                        // transactions_generator_ref_list are served untouched.
                        if !req.include_transaction_block {
                            block.transactions_generator = None;
                        }
                        let resp = RespondBlock { block: *block };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondBlock,
                            &resp,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    None => {
                        let rej = RejectBlock { height: req.height };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectBlock,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestBlocks => {
                let req = decode::<RequestBlocks>(msg, version)?;
                // An inverted range (end < start) or one wider than the cap rejects BEFORE the
                // store is touched — without this cap a hostile peer requests the whole chain
                // into one RespondBlocks. `end - start > cap` is compared on the INCLUSIVE range
                // (a cap-of-32 node serves up to 33 blocks); the short-circuit `end < start`
                // guard keeps the u32 subtraction safe.
                if req.end_height < req.start_height
                    || req.end_height - req.start_height > self.api.max_block_count_per_requests()
                {
                    let rej = RejectBlocks {
                        start_height: req.start_height,
                        end_height: req.end_height,
                    };
                    return send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RejectBlocks,
                        &rej,
                        msg.id,
                        version,
                    )
                    .await;
                }
                let mut blocks = Vec::new();
                for h in req.start_height..=req.end_height {
                    if let Some(mut b) = self.api.block_by_height(h).await {
                        // Headers-only range pulls strip ONLY transactions_generator per block,
                        // exactly like the single-block arm.
                        if !req.include_transaction_block {
                            b.transactions_generator = None;
                        }
                        blocks.push(*b);
                    } else {
                        let rej = RejectBlocks {
                            start_height: req.start_height,
                            end_height: req.end_height,
                        };
                        return send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectBlocks,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await;
                    }
                }
                let resp = RespondBlocks {
                    start_height: req.start_height,
                    end_height: req.end_height,
                    blocks,
                };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondBlocks,
                    &resp,
                    msg.id,
                    version,
                )
                .await
            }
            // A genuinely unsolicited block reply bans the sender — a full node never volunteers
            // these. Solicited replies and replies to recently timed-out requests are consumed by
            // the read loop's correlation-id fast path before the handler scan. Close + evict +
            // timed host ban via the peer's injected ban registry.
            ProtocolMessageTypes::RespondBlock
            | ProtocolMessageTypes::RespondBlocks
            | ProtocolMessageTypes::RejectBlock
            | ProtocolMessageTypes::RejectBlocks => {
                log::warn!(
                    "unsolicited {:?} from peer {peer_id}: closing + banning the connection",
                    msg.msg_type
                );
                close_peer(peers, peer_id, Some(BanCause::RateLimit)).await
            }
            ProtocolMessageTypes::NewTransaction => {
                match self
                    .api
                    .on_new_transaction(*peer_id, decode(msg, version)?)
                    .await
                {
                    TransactionAnnounceAction::Request(req) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RequestTransaction,
                            &req,
                            None,
                            version,
                        )
                        .await
                    }
                    // A zero-cost or already-seen-mismatched tx announcement bans the sender.
                    // Close + evict + timed host ban via the peer's injected registry.
                    TransactionAnnounceAction::Ban => {
                        close_peer(peers, peer_id, Some(BanCause::ConsensusError)).await
                    }
                    TransactionAnnounceAction::Ignore => Ok(()),
                }
            }
            ProtocolMessageTypes::RequestTransaction => {
                let req = decode::<RequestTransaction>(msg, version)?;
                if let Some(tx) = self.api.transaction(req.transaction_id).await {
                    let resp = RespondTransaction { transaction: tx };
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RespondTransaction,
                        &resp,
                        msg.id,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::RespondTransaction => {
                let resp = decode::<RespondTransaction>(msg, version)?;
                let host = peer_host(peers, peer_id).await;
                self.api
                    .on_respond_transaction(*peer_id, host, resp.transaction)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot => {
                if let Some(req) = self
                    .api
                    .on_new_signage_point_or_eos(*peer_id, decode(msg, version)?)
                    .await
                {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot,
                        &req,
                        None,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot => {
                let req = decode::<RequestSignagePointOrEndOfSubSlot>(msg, version)?;
                match self.api.signage_point_or_eos(req).await {
                    Some(SignagePointResponse::SignagePoint(sp)) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondSignagePoint,
                            sp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    Some(SignagePointResponse::EndOfSubSlot(eos)) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondEndOfSubSlot,
                            eos.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    None => Ok(()),
                }
            }
            ProtocolMessageTypes::RespondSignagePoint => {
                self.api
                    .on_respond_signage_point(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::RespondEndOfSubSlot => {
                self.api
                    .on_respond_end_of_sub_slot(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::NewUnfinishedBlock => {
                if let Some(req) = self
                    .api
                    .on_new_unfinished_block(*peer_id, decode(msg, version)?)
                    .await
                {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RequestUnfinishedBlock,
                        &req,
                        None,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::NewUnfinishedBlock2 => {
                if let Some(req) = self
                    .api
                    .on_new_unfinished_block2(*peer_id, decode(msg, version)?)
                    .await
                {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RequestUnfinishedBlock2,
                        &req,
                        None,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::RequestUnfinishedBlock => {
                let req = decode::<RequestUnfinishedBlock>(msg, version)?;
                match self.api.unfinished_block(req.unfinished_reward_hash).await {
                    Some(block) => {
                        let resp = RespondUnfinishedBlock {
                            unfinished_block: *block,
                        };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondUnfinishedBlock,
                            &resp,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    None => Ok(()),
                }
            }
            ProtocolMessageTypes::RequestUnfinishedBlock2 => {
                let req = decode::<RequestUnfinishedBlock2>(msg, version)?;
                match self
                    .api
                    .unfinished_block2(req.unfinished_reward_hash, req.foliage_hash)
                    .await
                {
                    Some(block) => {
                        let resp = RespondUnfinishedBlock {
                            unfinished_block: *block,
                        };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondUnfinishedBlock,
                            &resp,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    None => Ok(()),
                }
            }
            ProtocolMessageTypes::RespondUnfinishedBlock => {
                let resp = decode::<RespondUnfinishedBlock>(msg, version)?;
                self.api
                    .on_respond_unfinished_block(Box::new(resp.unfinished_block))
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::RequestCompactVdf => {
                let req = decode::<RequestCompactVDF>(msg, version)?;
                if let Some(resp) = self.api.compact_vdf(req).await {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RespondCompactVdf,
                        &resp,
                        msg.id,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::NewCompactVdf => {
                // CONSUME: a peer holds a compact proof for one of our stored blocks' VDF fields.
                // Pull it when ours is still bulky (the api decides + guards); the RespondCompactVDF
                // arm below finishes the swap.
                if let Some(req) = self
                    .api
                    .on_new_compact_vdf(*peer_id, decode(msg, version)?)
                    .await
                {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RequestCompactVdf,
                        &req,
                        None,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::RespondCompactVdf => {
                // The pulled compact proof; queue it for the driver to validate + swap + re-gossip
                // (off the read path — a VDF verify never runs on the websocket loop).
                self.api
                    .on_respond_compact_vdf(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::RespondCompactProofOfTime => {
                // The bluebox timelord's answer to our RequestCompactProofOfTime solicitation; queue
                // it for the same off-read-path validate + swap + re-gossip as RespondCompactVdf.
                // Timelord-sender-only.
                if !require_timelord_sender(peers, peer_id, msg.msg_type).await? {
                    return Ok(());
                }
                self.api
                    .on_respond_compact_proof_of_time(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::RequestProofOfWeight => {
                // Queue-only: the api implementation hands {peer, tip, id, peer-map} to its worker
                // (see FullNodeApi::on_request_proof_of_weight). Nothing is sent from the read path.
                let req = decode::<RequestProofOfWeight>(msg, version)?;
                self.api
                    .on_request_proof_of_weight(*peer_id, req, msg.id, peers.clone())
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::RequestPeers => {
                let _ = decode::<RequestPeers>(msg, version)?;
                let resp = RespondPeers {
                    peer_list: self.api.gossip_peers().await,
                };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondPeers,
                    &resp,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::RespondPeers => {
                let resp = decode::<RespondPeers>(msg, version)?;
                self.api.on_respond_peers(resp.peer_list).await;
                Ok(())
            }
            ProtocolMessageTypes::DeclareProofOfSpace => {
                // FARMER→NODE: a harvester's proof for a signage point we announced. On accept the
                // api returns the RequestSignedValues to send BACK to this same farmer peer —
                // the NewTransaction→RequestTransaction reply shape above.
                if let Some(req) = self
                    .api
                    .on_declare_proof_of_space(*peer_id, decode(msg, version)?)
                    .await
                {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::RequestSignedValues,
                        &req,
                        None,
                        version,
                    )
                    .await
                } else {
                    Ok(())
                }
            }
            ProtocolMessageTypes::SignedValues => {
                // FARMER→NODE: signatures over the foliage we asked the farmer to sign. Matched
                // here so the read loop routes it instead of logging an unhandled-message ERROR.
                self.api
                    .on_signed_values(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            // Decode the peer's BIP158 filter and push each mempool item it lacks back as a
            // NewTransaction on this connection (the peer pulls what it wants through the normal
            // announce path). The filter decode + limit live in the api implementation.
            ProtocolMessageTypes::RequestMempoolTransactions => {
                let req = decode::<RequestMempoolTransactions>(msg, version)?;
                for tx in self.api.mempool_items(req.filter).await {
                    send(
                        &self.counters,
                        peers,
                        peer_id,
                        ProtocolMessageTypes::NewTransaction,
                        &tx,
                        None,
                        version,
                    )
                    .await?;
                }
                Ok(())
            }
            // TIMELORD→NODE infusion-return surface. Queue-only: each api implementation gates on
            // sync and hands off to the driver — assembly + slot-state validation never run on the read loop.
            // All three are timelord-sender-only:
            // a non-timelord peer feeding VDF inboxes is a sender-type violation, not free CPU.
            ProtocolMessageTypes::NewInfusionPointVdf => {
                if !require_timelord_sender(peers, peer_id, msg.msg_type).await? {
                    return Ok(());
                }
                self.api
                    .on_new_infusion_point_vdf(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::NewSignagePointVdf => {
                if !require_timelord_sender(peers, peer_id, msg.msg_type).await? {
                    return Ok(());
                }
                self.api
                    .on_new_signage_point_vdf(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            ProtocolMessageTypes::NewEndOfSubSlotVdf => {
                if !require_timelord_sender(peers, peer_id, msg.msg_type).await? {
                    return Ok(());
                }
                self.api
                    .on_new_end_of_sub_slot_vdf(*peer_id, decode(msg, version)?)
                    .await;
                Ok(())
            }
            // ---- light-wallet query surface -----------------------------------------------------------
            ProtocolMessageTypes::RequestPuzzleSolution => {
                let req = decode::<RequestPuzzleSolution>(msg, version)?;
                match self.api.puzzle_solution(req.coin_name, req.height).await {
                    Some(response) => {
                        let resp = RespondPuzzleSolution { response };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondPuzzleSolution,
                            &resp,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    None => {
                        let rej = RejectPuzzleSolution {
                            coin_name: req.coin_name,
                            height: req.height,
                        };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectPuzzleSolution,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestBlockHeader => {
                let req = decode::<RequestBlockHeader>(msg, version)?;
                match self.api.block_header(req.height).await {
                    BlockHeaderReply::Respond(header_block) => {
                        let resp = RespondBlockHeader {
                            header_block: *header_block,
                        };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondBlockHeader,
                            &resp,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    BlockHeaderReply::Reject(height) => {
                        let rej = RejectHeaderRequest { height };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectHeaderRequest,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    BlockHeaderReply::Silent => Ok(()),
                }
            }
            ProtocolMessageTypes::RequestHeaderBlocks => {
                let req = decode::<RequestHeaderBlocks>(msg, version)?;
                match self
                    .api
                    .header_blocks(req.start_height, req.end_height)
                    .await
                {
                    HeaderBlocksReply::Respond(resp) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondHeaderBlocks,
                            resp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    HeaderBlocksReply::Reject(rej) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectHeaderBlocks,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                    HeaderBlocksReply::Silent => Ok(()),
                }
            }
            ProtocolMessageTypes::RequestBlockHeaders => {
                let req = decode::<RequestBlockHeaders>(msg, version)?;
                match self
                    .api
                    .block_headers(req.start_height, req.end_height, req.return_filter)
                    .await
                {
                    BlockHeadersReply::Respond(resp) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondBlockHeaders,
                            resp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    BlockHeadersReply::Reject(rej) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectBlockHeaders,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestAdditions => {
                let req = decode::<RequestAdditions>(msg, version)?;
                match self.api.additions(req).await {
                    AdditionsReply::Respond(resp) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondAdditions,
                            resp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    AdditionsReply::Reject(rej) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectAdditionsRequest,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestRemovals => {
                let req = decode::<RequestRemovals>(msg, version)?;
                match self.api.removals(req).await {
                    RemovalsReply::Respond(resp) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondRemovals,
                            resp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    RemovalsReply::Reject(rej) => {
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectRemovalsRequest,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestChildren => {
                let req = decode::<RequestChildren>(msg, version)?;
                let coin_states = self.api.children(req.coin_name).await;
                let resp = RespondChildren { coin_states };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondChildren,
                    &resp,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::RegisterInterestInPuzzleHash => {
                // The subscription cap is applied DURING decode: puzzle hashes past
                // max_subscriptions(peer) are skipped in O(1), never parsed.
                let host = peer_host(peers, peer_id).await;
                let req = RegisterForPhUpdates::from_bytes_limited(
                    &mut Cursor::new(msg.data.as_slice()),
                    version,
                    self.api.max_subscriptions(peer_id, host),
                )?;
                let reg = self.api.register_for_ph_updates(*peer_id, host, req).await;
                // On the peer's first registration, stand up the per-peer CoinStateUpdate forwarder.
                if let Some(rx) = reg.receiver {
                    spawn_coin_state_forwarder(
                        self.counters.clone(),
                        peers.clone(),
                        *peer_id,
                        rx,
                        version,
                    );
                }
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondToPhUpdate,
                    &reg.response,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::RegisterInterestInCoin => {
                // coin_ids past max_subscriptions(peer) are skipped at decode time.
                let host = peer_host(peers, peer_id).await;
                let req = RegisterForCoinUpdates::from_bytes_limited(
                    &mut Cursor::new(msg.data.as_slice()),
                    version,
                    self.api.max_subscriptions(peer_id, host),
                )?;
                let reg = self
                    .api
                    .register_for_coin_updates(*peer_id, host, req)
                    .await;
                if let Some(rx) = reg.receiver {
                    spawn_coin_state_forwarder(
                        self.counters.clone(),
                        peers.clone(),
                        *peer_id,
                        rx,
                        version,
                    );
                }
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondToCoinUpdate,
                    &reg.response,
                    msg.id,
                    version,
                )
                .await
            }
            // ---- the modern wallet-sync surface (codes 94-103). Every request is answered with
            // exactly one Respond*/Reject* echoing the request id.
            ProtocolMessageTypes::RequestPuzzleState => {
                // puzzle_hashes past MAX_PUZZLE_HASH_BATCH_SIZE are skipped at decode time.
                let req = RequestPuzzleState::from_bytes_limited(
                    &mut Cursor::new(msg.data.as_slice()),
                    version,
                    MAX_PUZZLE_HASH_BATCH_SIZE,
                )?;
                let host = peer_host(peers, peer_id).await;
                match self.api.puzzle_state(*peer_id, host, req).await {
                    PuzzleStateReply::Respond(resp, receiver) => {
                        // First subscribe-on-finish: stand up the per-peer CoinStateUpdate
                        // forwarder, exactly like RegisterInterestInPuzzleHash.
                        if let Some(rx) = receiver {
                            spawn_coin_state_forwarder(
                                self.counters.clone(),
                                peers.clone(),
                                *peer_id,
                                rx,
                                version,
                            );
                        }
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondPuzzleState,
                            resp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    PuzzleStateReply::Reject(reason) => {
                        // The reason is streamed as uint8.
                        let rej = RejectPuzzleState {
                            reason: reason as u8,
                        };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectPuzzleState,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestCoinState => {
                // A RequestCoinState claiming 1.2M coin_ids costs seconds of parse CPU on a
                // small node if the handler truncates only after parsing; the cap bounds the
                // parse itself.
                let host = peer_host(peers, peer_id).await;
                let req = RequestCoinState::from_bytes_limited(
                    &mut Cursor::new(msg.data.as_slice()),
                    version,
                    self.api.max_subscribe_response_items(peer_id, host),
                )?;
                match self.api.coin_state(*peer_id, host, req).await {
                    CoinStateReply::Respond(resp, receiver) => {
                        if let Some(rx) = receiver {
                            spawn_coin_state_forwarder(
                                self.counters.clone(),
                                peers.clone(),
                                *peer_id,
                                rx,
                                version,
                            );
                        }
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RespondCoinState,
                            resp.as_ref(),
                            msg.id,
                            version,
                        )
                        .await
                    }
                    CoinStateReply::Reject(reason) => {
                        let rej = RejectCoinState { reason };
                        send(
                            &self.counters,
                            peers,
                            peer_id,
                            ProtocolMessageTypes::RejectCoinState,
                            &rej,
                            msg.id,
                            version,
                        )
                        .await
                    }
                }
            }
            ProtocolMessageTypes::RequestFeeEstimates => {
                let req = decode::<RequestFeeEstimates>(msg, version)?;
                let estimates = self.api.fee_estimates(req).await;
                let resp = RespondFeeEstimates { estimates };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondFeeEstimates,
                    &resp,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::RequestRemovePuzzleSubscriptions => {
                let req = decode::<RequestRemovePuzzleSubscriptions>(msg, version)?;
                let puzzle_hashes = self
                    .api
                    .remove_puzzle_subscriptions(*peer_id, req.puzzle_hashes)
                    .await;
                let resp = RespondRemovePuzzleSubscriptions { puzzle_hashes };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondRemovePuzzleSubscriptions,
                    &resp,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::RequestRemoveCoinSubscriptions => {
                let req = decode::<RequestRemoveCoinSubscriptions>(msg, version)?;
                let coin_ids = self
                    .api
                    .remove_coin_subscriptions(*peer_id, req.coin_ids)
                    .await;
                let resp = RespondRemoveCoinSubscriptions { coin_ids };
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::RespondRemoveCoinSubscriptions,
                    &resp,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::SendTransaction => {
                // The wallet's spend submit. The api returns the ack (never None):
                // SUCCESS/PENDING/FAILED. The reply echoes the request id, so a wallet's
                // request/response correlation resolves on it.
                let req = decode::<SendTransaction>(msg, version)?;
                let ack = self.api.send_transaction(*peer_id, req).await;
                send(
                    &self.counters,
                    peers,
                    peer_id,
                    ProtocolMessageTypes::TransactionAck,
                    &ack,
                    msg.id,
                    version,
                )
                .await
            }
            ProtocolMessageTypes::ConfigureWindowSizes => {
                // The peer's RATE_LIMITS_V3 settings. Only legitimate on a link where the
                // capability was negotiated; otherwise, and on any validation failure (empty /
                // oversized / bounding one of OUR unlimited types), this is an invalid handshake
                // and the link closes with the short protocol ban.
                let Some(peer) = peers.read().await.get(peer_id).cloned() else {
                    return Ok(());
                };
                if !peer.v3.is_offered() {
                    log::warn!(
                        "Peer {peer_id} sent ConfigureWindowSizes without negotiating \
                         RATE_LIMITS_V3; closing"
                    );
                    return close_peer(peers, peer_id, Some(BanCause::InvalidProtocol)).await;
                }
                let cfg = decode::<ConfigureWindowSizes>(msg, version)?;
                match settings_from_configure(&cfg) {
                    Ok(settings) => {
                        peer.v3.activate(settings);
                        Ok(())
                    }
                    Err(e) => {
                        log::warn!("Peer {peer_id} sent an invalid ConfigureWindowSizes: {e}");
                        close_peer(peers, peer_id, Some(BanCause::InvalidProtocol)).await
                    }
                }
            }
            ProtocolMessageTypes::Error => {
                // The `error` protocol message (code 255): a peer reports a handler-side error in
                // place of a typed reject. It is logged and tolerated — no ban, no disconnect.
                // Tolerant parse only; we do not emit Error frames.
                match decode::<ErrorMessage>(msg, version) {
                    Ok(err) => {
                        log::warn!(
                            "Peer {peer_id} sent protocol Error: code={} message={:?}",
                            err.code,
                            err.message
                        );
                    }
                    Err(e) => {
                        log::warn!("Peer {peer_id} sent an undecodable protocol Error: {e:?}");
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl MessageHandler for FullNodeHandler {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        let size = msg.data.as_slice().len();
        self.counters.count_in(msg.msg_type, size);
        let version = peer_version(&peers, &peer_id).await;
        self.dispatch(&msg, &peer_id, &peers, version).await
    }
}

fn served(msg_type: ProtocolMessageTypes) -> bool {
    matches!(
        msg_type,
        ProtocolMessageTypes::Handshake
            | ProtocolMessageTypes::NewPeak
            | ProtocolMessageTypes::RequestBlock
            | ProtocolMessageTypes::RequestBlocks
            // The four block replies: solicited and recently timed-out ones are consumed by the
            // read loop's correlation-id fast path before the handler scan ever runs, so matching
            // them here catches genuinely unsolicited ones and dispatches them to the close arm.
            | ProtocolMessageTypes::RespondBlock
            | ProtocolMessageTypes::RespondBlocks
            | ProtocolMessageTypes::RejectBlock
            | ProtocolMessageTypes::RejectBlocks
            | ProtocolMessageTypes::NewTransaction
            | ProtocolMessageTypes::RequestTransaction
            | ProtocolMessageTypes::RespondTransaction
            | ProtocolMessageTypes::RespondUnfinishedBlock
            | ProtocolMessageTypes::NewUnfinishedBlock
            | ProtocolMessageTypes::NewUnfinishedBlock2
            | ProtocolMessageTypes::RequestUnfinishedBlock
            | ProtocolMessageTypes::RequestUnfinishedBlock2
            | ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot
            | ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot
            | ProtocolMessageTypes::RespondSignagePoint
            | ProtocolMessageTypes::RespondEndOfSubSlot
            | ProtocolMessageTypes::RequestCompactVdf
            // The compact-VDF consume flow: announce in, pulled proof in (dispatched above).
            | ProtocolMessageTypes::NewCompactVdf
            | ProtocolMessageTypes::RespondCompactVdf
            | ProtocolMessageTypes::RequestPeers
            | ProtocolMessageTypes::RespondPeers
            | ProtocolMessageTypes::RequestProofOfWeight
            // Gossip broadcast we match only to graceful-ignore (see the dispatch arm). Kept in the
            // filter so it never surfaces as an unhandled "No Matches" ERROR on a peer link.
            | ProtocolMessageTypes::RequestMempoolTransactions
            // Farmer interface: declared proofs in, signed foliage values in. NewSignagePoint /
            // RequestSignedValues are node→farmer sends, not inbound-served here.
            | ProtocolMessageTypes::DeclareProofOfSpace
            | ProtocolMessageTypes::SignedValues
            // Timelord infusion-return surface: infusion-point / signage-point / end-of-sub-slot VDFs the
            // timelord sends back so the node can finish its own farmed unfinished block into a peak.
            | ProtocolMessageTypes::NewInfusionPointVdf
            | ProtocolMessageTypes::NewSignagePointVdf
            | ProtocolMessageTypes::NewEndOfSubSlotVdf
            // The bluebox return: a timelord's compact proof answering our RequestCompactProofOfTime
            // solicitation (dispatched to the consume path). Kept here so it never surfaces as an
            // unhandled "No Matches" ERROR on the timelord link.
            | ProtocolMessageTypes::RespondCompactProofOfTime
            // Light-wallet query surface: spend submit (acked), puzzle/solution, header blocks,
            // additions/removals, and coin children — each answered with a Respond*/Reject*/Ack
            // body.
            | ProtocolMessageTypes::SendTransaction
            | ProtocolMessageTypes::RequestPuzzleSolution
            | ProtocolMessageTypes::RequestBlockHeader
            | ProtocolMessageTypes::RequestHeaderBlocks
            | ProtocolMessageTypes::RequestBlockHeaders
            | ProtocolMessageTypes::RequestAdditions
            | ProtocolMessageTypes::RequestRemovals
            | ProtocolMessageTypes::RequestChildren
            // Coin-state subscriptions: register interest (initial state reply + a live CoinStateUpdate
            // forwarder). CoinStateUpdate itself is a node->wallet send, not inbound-served.
            | ProtocolMessageTypes::RegisterInterestInPuzzleHash
            | ProtocolMessageTypes::RegisterInterestInCoin
            // The modern wallet-sync surface: paged puzzle-hash state, coin-id state, and
            // subscription removal. NewPeakWallet is a node->wallet send.
            | ProtocolMessageTypes::RequestPuzzleState
            | ProtocolMessageTypes::RequestCoinState
            | ProtocolMessageTypes::RequestRemovePuzzleSubscriptions
            | ProtocolMessageTypes::RequestRemoveCoinSubscriptions
            // Fee estimation (code 89): the wallet asks for fee-rate estimates at a set of
            // target times; answered with a RespondFeeEstimates group.
            | ProtocolMessageTypes::RequestFeeEstimates
            // The `error` protocol message (code 255): tolerated + logged, never banned. This
            // keeps a conforming peer's error report off the unknown-type close path.
            | ProtocolMessageTypes::Error
            // RATE_LIMITS_V3 configure exchange (code 111): the peer's window settings,
            // validated + stored by the dispatch arm.
            | ProtocolMessageTypes::ConfigureWindowSizes
    )
}

fn build_handlers(
    api: Arc<dyn FullNodeApi>,
    network_id: String,
    server_port: u16,
    respond_handshake: bool,
    counters: Arc<NetCounters>,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    let handler = Arc::new(FullNodeHandler {
        api,
        network_id,
        server_port,
        respond_handshake,
        counters,
    });
    let mut map = HashMap::new();
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: None,
                id: None,
                custom_fn: Some(Box::new(|m| served(m.msg_type))),
            }),
            handler,
        )),
    );
    map
}

// Register the full-node handler set on a `WebsocketServer`'s handler map:
// one dispatcher keyed by a single Uuid (O(1) register/unregister). Server role — replies to an inbound
// peer's Handshake.
#[must_use]
pub fn full_node_handlers(
    api: Arc<dyn FullNodeApi>,
    network_id: String,
    server_port: u16,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    build_handlers(api, network_id, server_port, true, Arc::default())
}

// Server-role handlers sharing the node's traffic counters (the server constructor; the
// plain variant keeps a private instance for tests/tools).
#[must_use]
pub fn full_node_handlers_counted(
    api: Arc<dyn FullNodeApi>,
    network_id: String,
    server_port: u16,
    counters: Arc<NetCounters>,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    build_handlers(api, network_id, server_port, true, counters)
}

// The same dispatcher for an OUTBOUND peer link (we dialed the peer): tip announcements (NewPeak), block
// serving, and graceful-ignore of gossip. Does NOT reply to Handshake — the outbound `WsClient` already
// performed the handshake, so a reply here would be a duplicate.
#[must_use]
pub fn full_node_handlers_client(
    api: Arc<dyn FullNodeApi>,
    network_id: String,
    server_port: u16,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    build_handlers(api, network_id, server_port, false, Arc::default())
}

#[must_use]
pub fn full_node_handlers_client_counted(
    api: Arc<dyn FullNodeApi>,
    network_id: String,
    server_port: u16,
    counters: Arc<NetCounters>,
) -> HashMap<Uuid, Arc<ChiaMessageHandler>> {
    build_handlers(api, network_id, server_port, false, counters)
}

#[cfg(test)]
#[path = "../tests/unit/handlers/tests.rs"]
mod tests;
