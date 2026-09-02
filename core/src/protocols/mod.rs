pub mod ban;
pub mod error;
pub mod farmer;
pub mod full_node;
pub mod harvester;
pub mod introducer;
pub mod outbound_limiter;
pub mod pool;
pub mod rate_limits;
pub mod rate_limits_v3;
pub mod shared;
pub mod timelord;
pub mod wallet;

use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::utils::await_termination;
use async_trait::async_trait;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::ChiaProtocolVersion;
use dg_xch_serialize::ChiaSerialize;
use futures_util::SinkExt;
use futures_util::stream::{FusedStream, SplitSink, SplitStream};
use futures_util::{Sink, Stream, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::fmt;
use std::io::{Cursor, Error};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::select;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

#[repr(u8)]
#[derive(ChiaSerial, Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProtocolMessageTypes {
    Unknown = 0,
    //Shared protocol (all services)
    Handshake = 1,

    //Harvester protocol (harvester < -> farmer)
    HarvesterHandshake = 3,
    //NewSignagePointHarvester = 4 Changed to 66 in new protocol
    NewProofOfSpace = 5,
    RequestSignatures = 6,
    RespondSignatures = 7,

    //Farmer protocol (farmer < -> full_node)
    NewSignagePoint = 8,
    DeclareProofOfSpace = 9,
    RequestSignedValues = 10,
    SignedValues = 11,
    FarmingInfo = 12,

    //Timelord protocol (timelord < -> full_node)
    NewPeakTimelord = 13,
    NewUnfinishedBlockTimelord = 14,
    NewInfusionPointVdf = 15,
    NewSignagePointVdf = 16,
    NewEndOfSubSlotVdf = 17,
    RequestCompactProofOfTime = 18,
    RespondCompactProofOfTime = 19,

    //Full node protocol (full_node < -> full_node)
    NewPeak = 20,
    NewTransaction = 21,
    RequestTransaction = 22,
    RespondTransaction = 23,
    RequestProofOfWeight = 24,
    RespondProofOfWeight = 25,
    RequestBlock = 26,
    RespondBlock = 27,
    RejectBlock = 28,
    RequestBlocks = 29,
    RespondBlocks = 30,
    RejectBlocks = 31,
    NewUnfinishedBlock = 32,
    RequestUnfinishedBlock = 33,
    RespondUnfinishedBlock = 34,
    NewSignagePointOrEndOfSubSlot = 35,
    RequestSignagePointOrEndOfSubSlot = 36,
    RespondSignagePoint = 37,
    RespondEndOfSubSlot = 38,
    RequestMempoolTransactions = 39,
    RequestCompactVdf = 40,
    RespondCompactVdf = 41,
    NewCompactVdf = 42,
    RequestPeers = 43,
    RespondPeers = 44,
    NoneResponse = 91,

    //Wallet protocol (wallet < -> full_node)
    RequestPuzzleSolution = 45,
    RespondPuzzleSolution = 46,
    RejectPuzzleSolution = 47,
    SendTransaction = 48,
    TransactionAck = 49,
    NewPeakWallet = 50,
    RequestBlockHeader = 51,
    RespondBlockHeader = 52,
    RejectHeaderRequest = 53,
    RequestRemovals = 54,
    RespondRemovals = 55,
    RejectRemovalsRequest = 56,
    RequestAdditions = 57,
    RespondAdditions = 58,
    RejectAdditionsRequest = 59,
    RequestHeaderBlocks = 60,
    RejectHeaderBlocks = 61,
    RespondHeaderBlocks = 62,

    //Introducer protocol (introducer < -> full_node)
    RequestPeersIntroducer = 63,
    RespondPeersIntroducer = 64,

    //Simulator protocol
    FarmNewBlock = 65,

    //New harvester protocol
    NewSignagePointHarvester = 66,
    RequestPlots = 67,
    RespondPlots = 68,
    PlotSyncStart = 78,
    PlotSyncLoaded = 79,
    PlotSyncRemoved = 80,
    PlotSyncInvalid = 81,
    PlotSyncKeysMissing = 82,
    PlotSyncDuplicates = 83,
    PlotSyncDone = 84,
    PlotSyncResponse = 85,

    //More wallet protocol
    CoinStateUpdate = 69,
    RegisterInterestInPuzzleHash = 70,
    RespondToPhUpdate = 71,
    RegisterInterestInCoin = 72,
    RespondToCoinUpdate = 73,
    RequestChildren = 74,
    RespondChildren = 75,
    RequestSesHashes = 76,
    RespondSesHashes = 77,
    RequestBlockHeaders = 86,
    RejectBlockHeaders = 87,
    RespondBlockHeaders = 88,
    RequestFeeEstimates = 89,
    RespondFeeEstimates = 90,

    //new Full Node protocol messages
    NewUnfinishedBlock2 = 92,
    RequestUnfinishedBlock2 = 93,

    //New wallet sync protocol
    RequestRemovePuzzleSubscriptions = 94,
    RespondRemovePuzzleSubscriptions = 95,
    RequestRemoveCoinSubscriptions = 96,
    RespondRemoveCoinSubscriptions = 97,
    RequestPuzzleState = 98,
    RespondPuzzleState = 99,
    RejectPuzzleState = 100,
    RequestCoinState = 101,
    RespondCoinState = 102,
    RejectCoinState = 103,

    //Wallet protocol mempool updates
    MempoolItemsAdded = 104,
    MempoolItemsRemoved = 105,
    RequestCostInfo = 106,
    RespondCostInfo = 107,

    //New farmer protocol messages (solution_response = 108)
    SolutionResponse = 108,
    //Solver protocol (solve = 109)
    Solve = 109,
    //Harvester partial proofs (partial_proofs = 110)
    PartialProofs = 110,
    //Rate-limits-v3 handshake follow-up (configure_window_sizes = 111)
    ConfigureWindowSizes = 111,
    //The error protocol message (error = 255) — see shared::ErrorMessage
    Error = 255,
}
impl From<u8> for ProtocolMessageTypes {
    #[allow(clippy::too_many_lines)]
    fn from(byte: u8) -> Self {
        match byte {
            i if i == ProtocolMessageTypes::Handshake as u8 => ProtocolMessageTypes::Handshake,
            i if i == ProtocolMessageTypes::HarvesterHandshake as u8 => {
                ProtocolMessageTypes::HarvesterHandshake
            }
            i if i == ProtocolMessageTypes::NewProofOfSpace as u8 => {
                ProtocolMessageTypes::NewProofOfSpace
            }
            i if i == ProtocolMessageTypes::RequestSignatures as u8 => {
                ProtocolMessageTypes::RequestSignatures
            }
            i if i == ProtocolMessageTypes::RespondSignatures as u8 => {
                ProtocolMessageTypes::RespondSignatures
            }
            i if i == ProtocolMessageTypes::NewSignagePoint as u8 => {
                ProtocolMessageTypes::NewSignagePoint
            }
            i if i == ProtocolMessageTypes::DeclareProofOfSpace as u8 => {
                ProtocolMessageTypes::DeclareProofOfSpace
            }
            i if i == ProtocolMessageTypes::RequestSignedValues as u8 => {
                ProtocolMessageTypes::RequestSignedValues
            }
            i if i == ProtocolMessageTypes::SignedValues as u8 => {
                ProtocolMessageTypes::SignedValues
            }
            i if i == ProtocolMessageTypes::FarmingInfo as u8 => ProtocolMessageTypes::FarmingInfo,
            i if i == ProtocolMessageTypes::NewPeakTimelord as u8 => {
                ProtocolMessageTypes::NewPeakTimelord
            }
            i if i == ProtocolMessageTypes::NewUnfinishedBlockTimelord as u8 => {
                ProtocolMessageTypes::NewUnfinishedBlockTimelord
            }
            i if i == ProtocolMessageTypes::NewInfusionPointVdf as u8 => {
                ProtocolMessageTypes::NewInfusionPointVdf
            }
            i if i == ProtocolMessageTypes::NewSignagePointVdf as u8 => {
                ProtocolMessageTypes::NewSignagePointVdf
            }
            i if i == ProtocolMessageTypes::NewEndOfSubSlotVdf as u8 => {
                ProtocolMessageTypes::NewEndOfSubSlotVdf
            }
            i if i == ProtocolMessageTypes::RequestCompactProofOfTime as u8 => {
                ProtocolMessageTypes::RequestCompactProofOfTime
            }
            i if i == ProtocolMessageTypes::RespondCompactProofOfTime as u8 => {
                ProtocolMessageTypes::RespondCompactProofOfTime
            }
            i if i == ProtocolMessageTypes::NewPeak as u8 => ProtocolMessageTypes::NewPeak,
            i if i == ProtocolMessageTypes::NewTransaction as u8 => {
                ProtocolMessageTypes::NewTransaction
            }
            i if i == ProtocolMessageTypes::RequestTransaction as u8 => {
                ProtocolMessageTypes::RequestTransaction
            }
            i if i == ProtocolMessageTypes::RespondTransaction as u8 => {
                ProtocolMessageTypes::RespondTransaction
            }
            i if i == ProtocolMessageTypes::RequestProofOfWeight as u8 => {
                ProtocolMessageTypes::RequestProofOfWeight
            }
            i if i == ProtocolMessageTypes::RespondProofOfWeight as u8 => {
                ProtocolMessageTypes::RespondProofOfWeight
            }
            i if i == ProtocolMessageTypes::RequestBlock as u8 => {
                ProtocolMessageTypes::RequestBlock
            }
            i if i == ProtocolMessageTypes::RespondBlock as u8 => {
                ProtocolMessageTypes::RespondBlock
            }
            i if i == ProtocolMessageTypes::RejectBlock as u8 => ProtocolMessageTypes::RejectBlock,
            i if i == ProtocolMessageTypes::RequestBlocks as u8 => {
                ProtocolMessageTypes::RequestBlocks
            }
            i if i == ProtocolMessageTypes::RespondBlocks as u8 => {
                ProtocolMessageTypes::RespondBlocks
            }
            i if i == ProtocolMessageTypes::RejectBlocks as u8 => {
                ProtocolMessageTypes::RejectBlocks
            }
            i if i == ProtocolMessageTypes::NewUnfinishedBlock as u8 => {
                ProtocolMessageTypes::NewUnfinishedBlock
            }
            i if i == ProtocolMessageTypes::RequestUnfinishedBlock as u8 => {
                ProtocolMessageTypes::RequestUnfinishedBlock
            }
            i if i == ProtocolMessageTypes::RespondUnfinishedBlock as u8 => {
                ProtocolMessageTypes::RespondUnfinishedBlock
            }
            i if i == ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot as u8 => {
                ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot
            }
            i if i == ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot as u8 => {
                ProtocolMessageTypes::RequestSignagePointOrEndOfSubSlot
            }
            i if i == ProtocolMessageTypes::RespondSignagePoint as u8 => {
                ProtocolMessageTypes::RespondSignagePoint
            }
            i if i == ProtocolMessageTypes::RespondEndOfSubSlot as u8 => {
                ProtocolMessageTypes::RespondEndOfSubSlot
            }
            i if i == ProtocolMessageTypes::RequestMempoolTransactions as u8 => {
                ProtocolMessageTypes::RequestMempoolTransactions
            }
            i if i == ProtocolMessageTypes::RequestCompactVdf as u8 => {
                ProtocolMessageTypes::RequestCompactVdf
            }
            i if i == ProtocolMessageTypes::RespondCompactVdf as u8 => {
                ProtocolMessageTypes::RespondCompactVdf
            }
            i if i == ProtocolMessageTypes::NewCompactVdf as u8 => {
                ProtocolMessageTypes::NewCompactVdf
            }
            i if i == ProtocolMessageTypes::RequestPeers as u8 => {
                ProtocolMessageTypes::RequestPeers
            }
            i if i == ProtocolMessageTypes::RespondPeers as u8 => {
                ProtocolMessageTypes::RespondPeers
            }
            i if i == ProtocolMessageTypes::NoneResponse as u8 => {
                ProtocolMessageTypes::NoneResponse
            }
            i if i == ProtocolMessageTypes::RequestPuzzleSolution as u8 => {
                ProtocolMessageTypes::RequestPuzzleSolution
            }
            i if i == ProtocolMessageTypes::RespondPuzzleSolution as u8 => {
                ProtocolMessageTypes::RespondPuzzleSolution
            }
            i if i == ProtocolMessageTypes::RejectPuzzleSolution as u8 => {
                ProtocolMessageTypes::RejectPuzzleSolution
            }
            i if i == ProtocolMessageTypes::SendTransaction as u8 => {
                ProtocolMessageTypes::SendTransaction
            }
            i if i == ProtocolMessageTypes::TransactionAck as u8 => {
                ProtocolMessageTypes::TransactionAck
            }
            i if i == ProtocolMessageTypes::NewPeakWallet as u8 => {
                ProtocolMessageTypes::NewPeakWallet
            }
            i if i == ProtocolMessageTypes::RequestBlockHeader as u8 => {
                ProtocolMessageTypes::RequestBlockHeader
            }
            i if i == ProtocolMessageTypes::RespondBlockHeader as u8 => {
                ProtocolMessageTypes::RespondBlockHeader
            }
            i if i == ProtocolMessageTypes::RejectHeaderRequest as u8 => {
                ProtocolMessageTypes::RejectHeaderRequest
            }
            i if i == ProtocolMessageTypes::RequestRemovals as u8 => {
                ProtocolMessageTypes::RequestRemovals
            }
            i if i == ProtocolMessageTypes::RespondRemovals as u8 => {
                ProtocolMessageTypes::RespondRemovals
            }
            i if i == ProtocolMessageTypes::RejectRemovalsRequest as u8 => {
                ProtocolMessageTypes::RejectRemovalsRequest
            }
            i if i == ProtocolMessageTypes::RequestAdditions as u8 => {
                ProtocolMessageTypes::RequestAdditions
            }
            i if i == ProtocolMessageTypes::RespondAdditions as u8 => {
                ProtocolMessageTypes::RespondAdditions
            }
            i if i == ProtocolMessageTypes::RejectAdditionsRequest as u8 => {
                ProtocolMessageTypes::RejectAdditionsRequest
            }
            i if i == ProtocolMessageTypes::RequestHeaderBlocks as u8 => {
                ProtocolMessageTypes::RequestHeaderBlocks
            }
            i if i == ProtocolMessageTypes::RejectHeaderBlocks as u8 => {
                ProtocolMessageTypes::RejectHeaderBlocks
            }
            i if i == ProtocolMessageTypes::RespondHeaderBlocks as u8 => {
                ProtocolMessageTypes::RespondHeaderBlocks
            }
            i if i == ProtocolMessageTypes::RequestPeersIntroducer as u8 => {
                ProtocolMessageTypes::RequestPeersIntroducer
            }
            i if i == ProtocolMessageTypes::RespondPeersIntroducer as u8 => {
                ProtocolMessageTypes::RespondPeersIntroducer
            }
            i if i == ProtocolMessageTypes::FarmNewBlock as u8 => {
                ProtocolMessageTypes::FarmNewBlock
            }
            i if i == ProtocolMessageTypes::NewSignagePointHarvester as u8 => {
                ProtocolMessageTypes::NewSignagePointHarvester
            }
            i if i == ProtocolMessageTypes::RequestPlots as u8 => {
                ProtocolMessageTypes::RequestPlots
            }
            i if i == ProtocolMessageTypes::RespondPlots as u8 => {
                ProtocolMessageTypes::RespondPlots
            }
            i if i == ProtocolMessageTypes::PlotSyncStart as u8 => {
                ProtocolMessageTypes::PlotSyncStart
            }
            i if i == ProtocolMessageTypes::PlotSyncLoaded as u8 => {
                ProtocolMessageTypes::PlotSyncLoaded
            }
            i if i == ProtocolMessageTypes::PlotSyncRemoved as u8 => {
                ProtocolMessageTypes::PlotSyncRemoved
            }
            i if i == ProtocolMessageTypes::PlotSyncInvalid as u8 => {
                ProtocolMessageTypes::PlotSyncInvalid
            }
            i if i == ProtocolMessageTypes::PlotSyncKeysMissing as u8 => {
                ProtocolMessageTypes::PlotSyncKeysMissing
            }
            i if i == ProtocolMessageTypes::PlotSyncDuplicates as u8 => {
                ProtocolMessageTypes::PlotSyncDuplicates
            }
            i if i == ProtocolMessageTypes::PlotSyncDone as u8 => {
                ProtocolMessageTypes::PlotSyncDone
            }
            i if i == ProtocolMessageTypes::PlotSyncResponse as u8 => {
                ProtocolMessageTypes::PlotSyncResponse
            }
            i if i == ProtocolMessageTypes::CoinStateUpdate as u8 => {
                ProtocolMessageTypes::CoinStateUpdate
            }
            i if i == ProtocolMessageTypes::RegisterInterestInPuzzleHash as u8 => {
                ProtocolMessageTypes::RegisterInterestInPuzzleHash
            }
            i if i == ProtocolMessageTypes::RespondToPhUpdate as u8 => {
                ProtocolMessageTypes::RespondToPhUpdate
            }
            i if i == ProtocolMessageTypes::RegisterInterestInCoin as u8 => {
                ProtocolMessageTypes::RegisterInterestInCoin
            }
            i if i == ProtocolMessageTypes::RespondToCoinUpdate as u8 => {
                ProtocolMessageTypes::RespondToCoinUpdate
            }
            i if i == ProtocolMessageTypes::RequestChildren as u8 => {
                ProtocolMessageTypes::RequestChildren
            }
            i if i == ProtocolMessageTypes::RespondChildren as u8 => {
                ProtocolMessageTypes::RespondChildren
            }
            i if i == ProtocolMessageTypes::RequestSesHashes as u8 => {
                ProtocolMessageTypes::RequestSesHashes
            }
            i if i == ProtocolMessageTypes::RespondSesHashes as u8 => {
                ProtocolMessageTypes::RespondSesHashes
            }
            i if i == ProtocolMessageTypes::RequestBlockHeaders as u8 => {
                ProtocolMessageTypes::RequestBlockHeaders
            }
            i if i == ProtocolMessageTypes::RejectBlockHeaders as u8 => {
                ProtocolMessageTypes::RejectBlockHeaders
            }
            i if i == ProtocolMessageTypes::RespondBlockHeaders as u8 => {
                ProtocolMessageTypes::RespondBlockHeaders
            }
            i if i == ProtocolMessageTypes::RequestFeeEstimates as u8 => {
                ProtocolMessageTypes::RequestFeeEstimates
            }
            i if i == ProtocolMessageTypes::RespondFeeEstimates as u8 => {
                ProtocolMessageTypes::RespondFeeEstimates
            }
            i if i == ProtocolMessageTypes::NewUnfinishedBlock2 as u8 => {
                ProtocolMessageTypes::NewUnfinishedBlock2
            }
            i if i == ProtocolMessageTypes::RequestUnfinishedBlock2 as u8 => {
                ProtocolMessageTypes::RequestUnfinishedBlock2
            }
            i if i == ProtocolMessageTypes::RequestRemovePuzzleSubscriptions as u8 => {
                ProtocolMessageTypes::RequestRemovePuzzleSubscriptions
            }
            i if i == ProtocolMessageTypes::RespondRemovePuzzleSubscriptions as u8 => {
                ProtocolMessageTypes::RespondRemovePuzzleSubscriptions
            }
            i if i == ProtocolMessageTypes::RequestRemoveCoinSubscriptions as u8 => {
                ProtocolMessageTypes::RequestRemoveCoinSubscriptions
            }
            i if i == ProtocolMessageTypes::RespondRemoveCoinSubscriptions as u8 => {
                ProtocolMessageTypes::RespondRemoveCoinSubscriptions
            }
            i if i == ProtocolMessageTypes::RequestPuzzleState as u8 => {
                ProtocolMessageTypes::RequestPuzzleState
            }
            i if i == ProtocolMessageTypes::RespondPuzzleState as u8 => {
                ProtocolMessageTypes::RespondPuzzleState
            }
            i if i == ProtocolMessageTypes::RejectPuzzleState as u8 => {
                ProtocolMessageTypes::RejectPuzzleState
            }
            i if i == ProtocolMessageTypes::RequestCoinState as u8 => {
                ProtocolMessageTypes::RequestCoinState
            }
            i if i == ProtocolMessageTypes::RespondCoinState as u8 => {
                ProtocolMessageTypes::RespondCoinState
            }
            i if i == ProtocolMessageTypes::RejectCoinState as u8 => {
                ProtocolMessageTypes::RejectCoinState
            }
            i if i == ProtocolMessageTypes::MempoolItemsAdded as u8 => {
                ProtocolMessageTypes::MempoolItemsAdded
            }
            i if i == ProtocolMessageTypes::MempoolItemsRemoved as u8 => {
                ProtocolMessageTypes::MempoolItemsRemoved
            }
            i if i == ProtocolMessageTypes::RequestCostInfo as u8 => {
                ProtocolMessageTypes::RequestCostInfo
            }
            i if i == ProtocolMessageTypes::RespondCostInfo as u8 => {
                ProtocolMessageTypes::RespondCostInfo
            }
            i if i == ProtocolMessageTypes::SolutionResponse as u8 => {
                ProtocolMessageTypes::SolutionResponse
            }
            i if i == ProtocolMessageTypes::Solve as u8 => ProtocolMessageTypes::Solve,
            i if i == ProtocolMessageTypes::PartialProofs as u8 => {
                ProtocolMessageTypes::PartialProofs
            }
            i if i == ProtocolMessageTypes::ConfigureWindowSizes as u8 => {
                ProtocolMessageTypes::ConfigureWindowSizes
            }
            i if i == ProtocolMessageTypes::Error as u8 => ProtocolMessageTypes::Error,
            _ => ProtocolMessageTypes::Unknown,
        }
    }
}

impl fmt::Display for ProtocolMessageTypes {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

pub const INVALID_PROTOCOL_BAN_SECONDS: u8 = 10;
pub const API_EXCEPTION_BAN_SECONDS: u8 = 10;
pub const INTERNAL_PROTOCOL_ERROR_BAN_SECONDS: u8 = 10;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NodeType {
    Unknown = 0,
    FullNode = 1,
    Harvester = 2,
    Farmer = 3,
    Timelord = 4,
    Introducer = 5,
    Wallet = 6,
    DataLayer = 7,
}
impl From<u8> for NodeType {
    fn from(byte: u8) -> Self {
        match byte {
            i if i == NodeType::Unknown as u8 => NodeType::Unknown,
            i if i == NodeType::FullNode as u8 => NodeType::FullNode,
            i if i == NodeType::Harvester as u8 => NodeType::Harvester,
            i if i == NodeType::Farmer as u8 => NodeType::Farmer,
            i if i == NodeType::Timelord as u8 => NodeType::Timelord,
            i if i == NodeType::Introducer as u8 => NodeType::Introducer,
            i if i == NodeType::Wallet as u8 => NodeType::Wallet,
            i if i == NodeType::DataLayer as u8 => NodeType::DataLayer,
            _ => NodeType::Unknown,
        }
    }
}

#[async_trait]
pub trait MessageHandler {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error>;
}

#[derive(ChiaSerial, Debug, Clone)]
pub struct ChiaMessage {
    pub msg_type: ProtocolMessageTypes,
    pub id: Option<u16>,
    pub data: UnsizedBytes,
}
impl ChiaMessage {
    pub fn new<T: ChiaSerialize>(
        msg_type: ProtocolMessageTypes,
        version: ChiaProtocolVersion,
        msg: &T,
        id: Option<u16>,
    ) -> Result<Self, Error> {
        Ok(ChiaMessage {
            msg_type,
            id,
            data: UnsizedBytes::new(msg.to_bytes(version)?),
        })
    }
}
impl From<ChiaMessage> for Message {
    fn from(val: ChiaMessage) -> Self {
        Message::Binary(
            val.to_bytes(ChiaProtocolVersion::default())
                .expect("Chia Message has Safe to Bytes")
                .into(),
        )
    }
}

pub type FilterFunction = Box<dyn Fn(&ChiaMessage) -> bool + Sync + Send + 'static>;

pub struct ChiaMessageFilter {
    pub msg_type: Option<ProtocolMessageTypes>,
    pub id: Option<u16>,
    pub custom_fn: Option<FilterFunction>,
}
impl ChiaMessageFilter {
    #[must_use]
    pub fn matches(&self, msg: &ChiaMessage) -> bool {
        if self.id.is_some() && self.id != msg.id {
            return false;
        }
        if let Some(s) = &self.msg_type
            && *s != msg.msg_type
        {
            return false;
        }
        if let Some(func) = &self.custom_fn {
            func(msg)
        } else {
            true
        }
    }
}

pub struct ChiaMessageHandler {
    pub filter: Arc<ChiaMessageFilter>,
    pub handle: Arc<dyn MessageHandler + Send + Sync>,
}
impl ChiaMessageHandler {
    pub fn new(
        filter: Arc<ChiaMessageFilter>,
        handle: Arc<dyn MessageHandler + Send + Sync>,
    ) -> Self {
        ChiaMessageHandler { filter, handle }
    }
}

/// Connection-scoped request/reply correlation table: an O(1) pending-request map routed by
/// the read loop.
///
/// The table owns id allocation *on the connection* (monotone, never reset, skipping any id
/// currently
/// in flight) and routes each reply to the single waiter that owns its id. Unsolicited gossip (no id)
/// and inbound requests to answer (an id that is not one of ours) fall through to the handler scan.
#[derive(Default)]
pub struct PendingRequests {
    inner: std::sync::Mutex<PendingInner>,
}

/// How long a cancelled request's id excuses its late reply. A reply older than this is
/// genuinely suspect and takes the handler scan (and its unsolicited-reply ban) as before.
const LATE_REPLY_GRACE: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Default)]
struct PendingInner {
    /// Last id handed out; the next allocation is `wrapping_add(1)`, skipping `0` and any live id.
    last_id: u16,
    waiters: HashMap<u16, tokio::sync::oneshot::Sender<Arc<ChiaMessage>>>,
    /// Tombstones for cancelled requests (timeout / failed send): the reply may still arrive,
    /// and it is OURS — the read loop consumes it instead of handler-scanning it, which would
    /// ban an honest-but-slow peer for our own timeout. Ids here are not re-allocated while
    /// fresh, so a tombstone can never swallow a new request's reply.
    expired: HashMap<u16, std::time::Instant>,
}

impl PendingInner {
    fn prune_expired(&mut self) {
        let now = std::time::Instant::now();
        self.expired
            .retain(|_, cancelled| now.duration_since(*cancelled) < LATE_REPLY_GRACE);
    }
}

impl PendingRequests {
    /// Reserve a connection-unique, non-zero correlation id and the one-shot receiver for its reply.
    /// The id skips any id currently in flight, so reuse can never alias a live waiter.
    #[must_use]
    pub fn register(&self) -> (u16, tokio::sync::oneshot::Receiver<Arc<ChiaMessage>>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.prune_expired();
        let id = loop {
            let cand = guard.last_id.wrapping_add(1);
            guard.last_id = cand;
            if cand != 0 && !guard.waiters.contains_key(&cand) && !guard.expired.contains_key(&cand)
            {
                break cand;
            }
        };
        guard.waiters.insert(id, tx);
        (id, rx)
    }

    /// Drop a waiter without delivery (its request timed out or the send failed) so the table never
    /// leaks an entry for a request no reply will ever satisfy. The id is tombstoned for
    /// [`LATE_REPLY_GRACE`] so the reply, should it still arrive, is consumed rather than
    /// handler-scanned as unsolicited.
    pub fn cancel(&self, id: u16) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = guard.waiters.remove(&id);
        guard.prune_expired();
        guard.expired.insert(id, std::time::Instant::now());
    }

    /// Route `msg` to the single waiter that owns `id`. Returns `true` when a waiter was found (the
    /// read loop then skips the handler scan for this frame); `false` when `id` is not one of ours —
    /// an inbound request we must answer, or a stale/duplicate reply after the waiter already left.
    #[must_use]
    pub fn deliver(&self, id: u16, msg: Arc<ChiaMessage>) -> bool {
        let (waiter, late) = {
            let mut guard = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.prune_expired();
            let waiter = guard.waiters.remove(&id);
            let late = waiter.is_none() && guard.expired.remove(&id).is_some();
            (waiter, late)
        };
        if let Some(tx) = waiter {
            // The receiver may already be gone (its own timeout won the race); dropping the send is
            // then correct — the caller has moved on.
            let _ = tx.send(msg);
            true
        } else if late {
            // A reply to our own cancelled request: consumed and discarded — never scanned.
            debug!(
                "late reply for cancelled request id={id} type={:?}",
                msg.msg_type
            );
            true
        } else {
            false
        }
    }
}

pub type PeerMap = Arc<RwLock<HashMap<Bytes32, Arc<SocketPeer>>>>;

pub struct SocketPeer {
    pub node_type: Arc<RwLock<NodeType>>,
    pub protocol_version: Arc<RwLock<ChiaProtocolVersion>>,
    /// The capabilities the peer advertised in its handshake — the input to the per-connection rate
    /// limiter's v1/v2 selection. Empty until the handshake is
    /// processed; a message seen before then is charged under the stricter v1 numbers, which is safe.
    pub capabilities: Arc<RwLock<shared::Capabilities>>,
    pub websocket: Arc<RwLock<WebsocketConnection>>,
    /// The peer's REMOTE host (IP), captured at accept/dial time. This is the ban key (the host,
    /// not the cert-hash peer id used as the map key) — so the close path can
    /// enter this peer's IP into [`bans`](Self::bans). `None` when the remote addr was unavailable
    /// (e.g. an outbound dial to a hostname that was not resolved to an `IpAddr`), which simply means
    /// this peer cannot be host-banned — a fail-open we accept over guessing an IP.
    pub host: Option<std::net::IpAddr>,
    /// The shared, server-wide timed ban list. `Some` on inbound
    /// server links (the full-node listener injects its registry so a rate-limit/consensus close both
    /// evicts the peer AND enters its host into the list the accept path consults); `None` on
    /// outbound client links, which do not maintain a ban list of their own.
    pub bans: Option<Arc<ban::BanRegistry>>,
    /// Optional per-connection OUTBOUND self-throttle. When present
    /// (full-node links), a frequency-capped message is paced against the PEER's budget before it is
    /// written, so a re-gossip burst cannot get US banned. `None` on non-full-node links, and on
    /// `None` [`SocketPeer::send`] writes directly — identical to the pre-throttle behaviour.
    pub outbound_limiter: Option<Arc<outbound_limiter::OutboundLimiter>>,
    /// Per-connection RATE_LIMITS_V3 state — the same instance the link's
    /// [`WebsocketConnection`] and [`ReadStream`] hold, so the handshake arm (negotiation), the
    /// read loop (receive windows), and the request senders (outbound windows) all see one
    /// truth. Inert (`!is_active()`) unless the capability was negotiated.
    pub v3: Arc<rate_limits_v3::V3Link>,
}
impl SocketPeer {
    /// Send `msg` to this peer, self-throttling first when an outbound limiter is installed. The
    /// throttle wait runs in THIS task holding NO connection lock; only once the message is
    /// admitted do we take the
    /// write lock and write it. An `Unlimited` serve type (RespondBlocks, …) is admitted instantly, so
    /// our sync/serve path is never delayed. A dropped message (exempt over budget, or the bounded
    /// attempt cap reached) is logged and swallowed.
    pub async fn send(&self, msg: ChiaMessage) -> Result<(), Error> {
        if let Some(limiter) = &self.outbound_limiter {
            let caps = self.capabilities.read().await.clone();
            let size = msg.data.as_slice().len();
            match limiter.admit(msg.msg_type, size, &caps).await {
                outbound_limiter::ThrottleOutcome::Admit => {}
                outbound_limiter::ThrottleOutcome::Drop(reason) => {
                    warn!(
                        "Self-rate-limiting outbound {:?} to peer: {reason:?}",
                        msg.msg_type
                    );
                    return Ok(());
                }
            }
        }
        self.websocket.write().await.send(msg.into()).await
    }
}

pub enum WebsocketMsgStream {
    TokioIo(Box<WebSocketStream<TokioIo<Upgraded>>>),
    Tls(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}
impl Stream for WebsocketMsgStream {
    type Item = Result<Message, tokio_tungstenite::tungstenite::error::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(s) => Pin::new(s).poll_next(cx),
            WebsocketMsgStream::Tls(s) => Pin::new(s).poll_next(cx),
        }
    }
}
impl FusedStream for WebsocketMsgStream {
    fn is_terminated(&self) -> bool {
        match self {
            WebsocketMsgStream::TokioIo(s) => s.is_terminated(),
            WebsocketMsgStream::Tls(s) => s.is_terminated(),
        }
    }
}
impl Sink<Message> for WebsocketMsgStream {
    type Error = tokio_tungstenite::tungstenite::error::Error;
    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(s) => Pin::new(s).poll_ready(cx),
            WebsocketMsgStream::Tls(s) => Pin::new(s).poll_ready(cx),
        }
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(s) => Pin::new(s).start_send(item),
            WebsocketMsgStream::Tls(s) => Pin::new(s).start_send(item),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(s) => Pin::new(s).poll_flush(cx),
            WebsocketMsgStream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(s) => Pin::new(s).poll_close(cx),
            WebsocketMsgStream::Tls(s) => Pin::new(s).poll_close(cx),
        }
    }
}

pub struct WebsocketConnection {
    write: SplitSink<WebsocketMsgStream, Message>,
    message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    /// Correlation table for request/reply on this connection; shared with the [`ReadStream`] that
    /// routes replies back to their waiters (see [`PendingRequests`]).
    pending: Arc<PendingRequests>,
    /// Per-connection RATE_LIMITS_V3 state, shared with the [`ReadStream`] (and the
    /// [`SocketPeer`] callers wire up). See [`rate_limits_v3::V3Link`].
    v3: Arc<rate_limits_v3::V3Link>,
}
/// Upper bound on a single websocket write. A peer that stops draining (full TCP receive window
/// → Sink backpressure) must never wedge the sender: the caller holds the connection write lock
/// across `send`, so an unbounded write there stalls every other sender on that peer. 30s
/// matches the request timeout: past it the peer is dead and the caller should fail over.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Write `msg` into `sink`, bounding the write with `timeout` so a non-draining peer cannot block it
/// forever. Returns an error (not a hang) when the bound trips.
async fn timeout_send<S>(sink: &mut S, msg: Message, timeout: Duration) -> Result<(), Error>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Debug,
{
    match tokio::time::timeout(timeout, sink.send(msg)).await {
        Err(_) => Err(Error::other("websocket send timed out")),
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(Error::other(format!("{e:?}"))),
    }
}

impl WebsocketConnection {
    pub fn new(
        websocket: WebsocketMsgStream,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
        limiter: Option<Arc<rate_limits::RateLimiter>>,
    ) -> (Self, ReadStream) {
        let (write, read) = websocket.split();
        let pending = Arc::new(PendingRequests::default());
        let v3 = Arc::new(rate_limits_v3::V3Link::default());
        let websocket = WebsocketConnection {
            write,
            message_handlers: message_handlers.clone(),
            pending: pending.clone(),
            v3: v3.clone(),
        };
        let stream = ReadStream {
            read,
            message_handlers,
            peer_id,
            peers,
            pending,
            limiter,
            v3,
        };
        (websocket, stream)
    }

    /// This link's shared RATE_LIMITS_V3 state — callers thread it onto the [`SocketPeer`], and
    /// the request senders consult it for outbound windows.
    #[must_use]
    pub fn v3(&self) -> Arc<rate_limits_v3::V3Link> {
        self.v3.clone()
    }
    pub async fn send(&mut self, msg: Message) -> Result<(), Error> {
        timeout_send(&mut self.write, msg, SEND_TIMEOUT).await
    }

    /// Reserve a connection-unique correlation id and the receiver its reply will be routed to. The
    /// caller stamps the id onto the outgoing [`ChiaMessage`] and awaits the receiver; the read loop
    /// delivers the matching reply to exactly this waiter. Takes `&self` (only the pending table is
    /// touched), so it composes under a read lock without contending the write half.
    #[must_use]
    pub fn register_request(&self) -> (u16, tokio::sync::oneshot::Receiver<Arc<ChiaMessage>>) {
        self.pending.register()
    }

    /// Release a reserved correlation id whose reply never arrived (timeout / send failure).
    /// Frees any RATE_LIMITS_V3 outbound-window slot the request occupied; without this a
    /// timed-out request leaks its window slot forever.
    pub fn cancel_request(&self, id: u16) {
        self.pending.cancel(id);
        self.v3.out_release(id);
    }

    pub async fn subscribe(&self, uuid: Uuid, handle: Arc<ChiaMessageHandler>) {
        self.message_handlers.write().await.insert(uuid, handle);
    }

    pub async fn unsubscribe(&self, uuid: Uuid) -> Option<Arc<ChiaMessageHandler>> {
        self.message_handlers.write().await.remove(&uuid)
    }

    pub async fn close(&mut self, msg: Option<Message>) -> Result<(), Error> {
        if let Some(msg) = msg {
            let _ = self.write.send(msg).await.map_err(Error::other);
            self.write.close().await.map_err(Error::other)
        } else {
            self.write.close().await.map_err(Error::other)
        }
    }
    pub async fn clear(&self) {
        self.message_handlers.write().await.clear();
    }
    pub async fn shutdown(&mut self) -> Result<(), Error> {
        self.close(None).await
    }
}

pub struct ReadStream {
    read: SplitStream<WebsocketMsgStream>,
    message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    peer_id: Arc<Bytes32>,
    peers: PeerMap,
    /// Shared with the owning [`WebsocketConnection`]; a reply whose id is registered here is routed
    /// to its single waiter and does not fan out to the handler scan (see [`PendingRequests`]).
    pending: Arc<PendingRequests>,
    /// Optional per-connection inbound rate limiter. When present,
    /// EVERY inbound message is charged against the composed limits BEFORE the correlation fast-path
    /// or the handler scan runs — so solicited replies (RespondBlocks bursts) count too.
    /// A violation closes the connection
    /// and evicts the peer. `None` on non-full-node links
    /// (harvester/farmer/wallet clients), which are left unpoliced.
    limiter: Option<Arc<rate_limits::RateLimiter>>,
    /// Per-connection RATE_LIMITS_V3 state (shared with the [`WebsocketConnection`]): when the
    /// capability was negotiated, v3-tabled types bypass the time-based limiter and bounded
    /// request types are admitted through in-flight receive windows instead.
    v3: Arc<rate_limits_v3::V3Link>,
}
impl ReadStream {
    pub async fn run(&mut self, run: Arc<AtomicBool>) {
        loop {
            let peer_self = self.peers.read().await.get(&self.peer_id).cloned();
            let protocol_version = if let Some(peer) = peer_self.as_ref() {
                *peer.protocol_version.read().await
            } else {
                ChiaProtocolVersion::default()
            };
            // Snapshot the peer's negotiated capabilities for the rate limiter's v1/v2 selection.
            // Only paid for when a limiter is installed (full-node links).
            let peer_caps: shared::Capabilities = if self.limiter.is_some() {
                if let Some(peer) = peer_self.as_ref() {
                    peer.capabilities.read().await.clone()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            select! {
                msg = self.read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Binary(bin_data) => {
                                    let mut cursor = Cursor::new(bin_data.as_ref());
                                    match ChiaMessage::from_bytes(&mut cursor, protocol_version) {
                                        Ok(chia_msg) => {
                                            let msg_arc: Arc<ChiaMessage> = Arc::new(chia_msg);
                                            // An undefined message type disconnects the peer with
                                            // the short INTERNAL_PROTOCOL_ERROR ban and a
                                            // PROTOCOL_ERROR (1002) close, BEFORE the rate limiter
                                            // or any dispatch sees it. The recognized-code set is
                                            // pinned by core/tests/protocol_message_codes.rs, so
                                            // this never fires on a conforming peer. Links without
                                            // a ban registry or resolved host still close, they
                                            // just cannot host-ban.
                                            if msg_arc.msg_type == ProtocolMessageTypes::Unknown {
                                                error!(
                                                    "Disconnecting peer {} for unknown message type",
                                                    self.peer_id
                                                );
                                                if let Some(peer) = self
                                                    .peers
                                                    .write()
                                                    .await
                                                    .remove(self.peer_id.as_ref())
                                                {
                                                    if let (Some(bans), Some(host)) =
                                                        (peer.bans.as_ref(), peer.host)
                                                    {
                                                        bans.ban(
                                                            host,
                                                            ban::BanCause::InternalProtocolError,
                                                        );
                                                    }
                                                    let close_frame = Message::Close(Some(
                                                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                                            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Protocol,
                                                            reason: "INVALID_PROTOCOL_MESSAGE".into(),
                                                        },
                                                    ));
                                                    let _ = peer
                                                        .websocket
                                                        .write()
                                                        .await
                                                        .close(Some(close_frame))
                                                        .await;
                                                }
                                                return;
                                            }
                                            // Once RATE_LIMITS_V3 is negotiated, v3-tabled types
                                            // are NOT subject to the time-based limiter; bounded
                                            // request types are admitted through an in-flight
                                            // receive window instead, and over-window closes with
                                            // the RATE_LIMITER ban like a v2 violation. Localhost
                                            // and exempt peers bypass window enforcement but still
                                            // bypass v2 for v3 types.
                                            let v3_typed = self.v3.is_active()
                                                && rate_limits_v3::v3_setting(msg_arc.msg_type)
                                                    .is_some();
                                            let mut recv_guard: Option<
                                                Arc<rate_limits_v3::RecvGuard>,
                                            > = None;
                                            if v3_typed {
                                                let is_local = peer_self
                                                    .as_ref()
                                                    .and_then(|p| p.host)
                                                    .is_some_and(|h| h.is_loopback());
                                                if !is_local {
                                                    match self.v3.recv_acquire(msg_arc.msg_type) {
                                                        Ok(true) => {
                                                            recv_guard = Some(Arc::new(
                                                                rate_limits_v3::RecvGuard::new(
                                                                    self.v3.clone(),
                                                                    msg_arc.msg_type,
                                                                ),
                                                            ));
                                                        }
                                                        Ok(false) => {}
                                                        Err(()) => {
                                                            warn!(
                                                                "Peer {} exceeded the v3 receive window for {:?}; closing connection",
                                                                self.peer_id, msg_arc.msg_type
                                                            );
                                                            if let Some(peer) = self
                                                                .peers
                                                                .write()
                                                                .await
                                                                .remove(self.peer_id.as_ref())
                                                            {
                                                                if let (Some(bans), Some(host)) =
                                                                    (peer.bans.as_ref(), peer.host)
                                                                {
                                                                    bans.ban(
                                                                        host,
                                                                        ban::BanCause::RateLimit,
                                                                    );
                                                                }
                                                                let _ = peer
                                                                    .websocket
                                                                    .write()
                                                                    .await
                                                                    .close(None)
                                                                    .await;
                                                            }
                                                            return;
                                                        }
                                                    }
                                                }
                                            }
                                            // Inbound rate limit: charge EVERY message against
                                            // the composed per-connection limits BEFORE the
                                            // correlation fast-path or the handler scan, so
                                            // solicited replies count too. A violation closes the
                                            // connection, evicts the peer, and applies the timed
                                            // rate-limiter ban below.
                                            if let Some(limiter) = &self.limiter
                                                && !v3_typed
                                            {
                                                let size = msg_arc.data.as_slice().len();
                                                if let Some(reason) = limiter.process_and_check(
                                                    msg_arc.msg_type,
                                                    size,
                                                    &peer_caps,
                                                ) {
                                                    warn!(
                                                        "Rate limit exceeded by peer {}: {reason}; closing connection",
                                                        self.peer_id
                                                    );
                                                    if let Some(peer) = self
                                                        .peers
                                                        .write()
                                                        .await
                                                        .remove(self.peer_id.as_ref())
                                                    {
                                                        // Enter this peer's REMOTE host into the
                                                        // timed ban list so a reconnect within the
                                                        // window is refused at the accept path — not
                                                        // just this connection closed. No-op on links
                                                        // without a registry (outbound client) or an
                                                        // unknown host.
                                                        if let (Some(bans), Some(host)) =
                                                            (peer.bans.as_ref(), peer.host)
                                                        {
                                                            bans.ban(host, ban::BanCause::RateLimit);
                                                        }
                                                        let _ = peer
                                                            .websocket
                                                            .write()
                                                            .await
                                                            .close(None)
                                                            .await;
                                                    }
                                                    return;
                                                }
                                            }
                                            // Correlation-id fast path: a reply carrying an id we have
                                            // a pending waiter for is routed to that ONE waiter and
                                            // consumed here — it must never also fan out to the handler
                                            // scan (the ambiguity that produced the 27 s stall). An id
                                            // that is not ours (an inbound request to answer) falls
                                            // through to the handler path below unchanged.
                                            if let Some(id) = msg_arc.id
                                                && self.pending.deliver(id, msg_arc.clone())
                                            {
                                                // A solicited reply frees any v3 outbound-window
                                                // slot its request occupied.
                                                self.v3.out_release(id);
                                                debug!(
                                                    "Routed reply id={id}: {:?}",
                                                    msg_arc.msg_type
                                                );
                                                continue;
                                            }
                                            let mut matched = false;
                                            for v in self.message_handlers.read().await.values()
                                                .cloned().collect::<Vec<Arc<ChiaMessageHandler>>>() {
                                                if v.filter.matches(msg_arc.as_ref()) {
                                                    let msg_arc_c = msg_arc.clone();
                                                    let peer_id = self.peer_id.clone();
                                                    let peers = self.peers.clone();
                                                    let v_arc_c = v.handle.clone();
                                                    // The v3 receive-window slot stays occupied
                                                    // until every handler task for this message
                                                    // finishes (the guard's last clone drops).
                                                    let guard = recv_guard.clone();
                                                    tokio::spawn(async move {
                                                        let _guard = guard;
                                                        if let Err(e) = v_arc_c.handle(msg_arc_c.clone(), peer_id, peers).await {
                                                            error!("Error Handling Message({:#?}): {e:?}", msg_arc_c.msg_type);
                                                        }
                                                    });
                                                    matched = true;
                                                }
                                            }
                                            if !matched{
                                                error!("No Matches for Message: {msg_arc:?}");
                                            }
                                            debug!("Processed Message: {:?}", msg_arc.msg_type);
                                        }
                                        Err(e) => {
                                            error!("Invalid Message: {e:?}");
                                        }
                                    }
                                }
                                Message::Close(e) => {
                                    debug!("Got Close Message: {e:?}");
                                    return;
                                },
                                _ => {
                                    error!("Invalid Message: {msg:?}");
                                }
                            }
                        }
                        Some(Err(msg)) => {
                            match msg {
                                tokio_tungstenite::tungstenite::Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
                                    warn!("Server Stream Closed without Handshake");
                                },
                                tokio_tungstenite::tungstenite::Error::Io(e) => {
                                    warn!("Server Stream Closed: {e}");
                                },
                                // Same class at the TLS layer: a peer dropping the socket
                                // without sending close_notify is routine on a public
                                // listener. Debounced to one WARN per window (DEBUG carries
                                // every instance) so a real TLS fault still leaves a trace
                                // without an ERROR line per abandoned handshake.
                                tokio_tungstenite::tungstenite::Error::Tls(e) => {
                                    use std::sync::atomic::AtomicU64;
                                    static LAST_TLS_WARN_UNIX: AtomicU64 = AtomicU64::new(0);
                                    const TLS_WARN_DEBOUNCE_SECS: u64 = 600;
                                    let now = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_or(0, |d| d.as_secs());
                                    let last = LAST_TLS_WARN_UNIX.load(Ordering::Relaxed);
                                    if now.saturating_sub(last) >= TLS_WARN_DEBOUNCE_SECS
                                        && LAST_TLS_WARN_UNIX
                                            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                                            .is_ok()
                                    {
                                        warn!("Server Stream TLS close without close_notify (debounced; routine peer behavior): {e}");
                                    } else {
                                        debug!("Server Stream TLS close without close_notify: {e}");
                                    }
                                },
                                others => {
                                    error!("Server Stream Error: {others:?}");
                                }
                            }
                            return;
                        }
                        None => {
                            info!("End of server read Stream");
                            return;
                        }
                    }
                }
                _ = await_termination() => {
                    info!("Got Termination Signal for WS");
                    return;
                }
                () = async {
                    loop {
                        if !run.load(Ordering::Relaxed){
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } => {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod send_timeout_tests {
    use super::{SEND_TIMEOUT, timeout_send};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    // A ready sink (a peer draining normally): the write completes and round-trips Ok.
    #[tokio::test]
    async fn send_round_trips_on_a_ready_sink() {
        let mut sink = futures_util::sink::drain::<Message>();
        let msg = Message::Binary(vec![1, 2, 3].into());
        let out = timeout_send(&mut sink, msg, SEND_TIMEOUT).await;
        assert!(out.is_ok(), "a draining sink must accept the write");
    }

    // A never-ready sink models a peer whose TCP receive window is full — the exact backpressure that
    // used to wedge the sender under the connection write lock. The bounded write must resolve to a
    // timeout error, never hang.
    #[tokio::test]
    async fn send_times_out_on_a_stalled_sink() {
        struct StalledSink;
        impl futures_util::Sink<Message> for StalledSink {
            type Error = std::io::Error;
            fn poll_ready(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Pending
            }
            fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
                Ok(())
            }
            fn poll_flush(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Pending
            }
            fn poll_close(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Pending
            }
        }
        let mut sink = StalledSink;
        let msg = Message::Binary(vec![].into());
        let out = timeout_send(&mut sink, msg, Duration::from_millis(50)).await;
        assert!(
            out.is_err(),
            "a stalled sink must time out, not hang the sender"
        );
    }
}

#[cfg(test)]
mod pending_request_tests {
    use super::{ChiaMessage, PendingRequests, ProtocolMessageTypes};
    use crate::blockchain::unsized_bytes::UnsizedBytes;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn msg(id: Option<u16>, t: ProtocolMessageTypes) -> Arc<ChiaMessage> {
        Arc::new(ChiaMessage {
            msg_type: t,
            id,
            data: UnsizedBytes::new(vec![]),
        })
    }

    // Allocation is connection-unique and non-zero: a run of registrations (all still in flight)
    // hands out strictly distinct, non-zero ids. This is the property whose *absence* — the per-source
    // counter reset to 1 — let two concurrent requests share id 1 and produced the 27 s stall.
    #[tokio::test]
    async fn register_hands_out_distinct_nonzero_ids() {
        let pending = PendingRequests::default();
        let mut ids = HashSet::new();
        let mut _keep = Vec::new();
        for _ in 0..1000 {
            let (id, rx) = pending.register();
            assert_ne!(id, 0, "id 0 is reserved (id-less gossip / handshake)");
            assert!(
                ids.insert(id),
                "id {id} was handed out twice while still in flight"
            );
            _keep.push(rx); // hold the receivers so their ids stay live and cannot be reused
        }
    }

    // A live id is never re-handed even as the u16 counter advances: with two waiters outstanding, a
    // third allocation differs from both.
    #[tokio::test]
    async fn register_skips_live_ids() {
        let pending = PendingRequests::default();
        let (a, _ra) = pending.register();
        let (b, _rb) = pending.register();
        let (c, _rc) = pending.register();
        assert!(a != b && b != c && a != c, "live ids {a},{b},{c} collided");
    }

    // A reply that arrives AFTER its request timed out is still OURS: the read loop must consume
    // it (deliver returns true) instead of letting it fall to the handler scan, where the
    // unsolicited-reply arm closes and bans the peer — banning an honest-but-slow peer for our
    // own timeout, and burning the peer pool down under load.
    #[tokio::test]
    async fn a_late_reply_for_a_cancelled_request_is_consumed_not_scanned() {
        let pending = PendingRequests::default();
        let (id, rx) = pending.register();
        drop(rx);
        pending.cancel(id);
        assert!(
            pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
            "a late reply to our own timed-out request fell through to the handler scan"
        );
    }

    // A cancelled id must not be re-handed while its tombstone is fresh: a new request allocated
    // the same id would have its reply swallowed by the late-reply grace.
    #[tokio::test]
    async fn a_fresh_tombstone_blocks_id_reuse() {
        let pending = PendingRequests::default();
        let (id, rx) = pending.register();
        drop(rx);
        pending.cancel(id);
        let mut _keep = Vec::new();
        for _ in 0..u32::from(u16::MAX) {
            let (fresh, rx) = pending.register();
            assert_ne!(fresh, id, "a fresh tombstone's id was re-allocated");
            _keep.push(rx);
            if _keep.len() >= 65_000 {
                break;
            }
        }
    }

    // A reply is routed to the ONE waiter that owns its id, and only that waiter — the other waiter's
    // receiver is untouched. This is the demux invariant: no fan-out to every matching handler.
    #[tokio::test]
    async fn deliver_routes_to_exactly_the_owning_waiter() {
        let pending = PendingRequests::default();
        let (id_a, rx_a) = pending.register();
        let (id_b, rx_b) = pending.register();

        // Deliver B first, then A — out-of-order, as concurrent replies arrive.
        assert!(pending.deliver(id_b, msg(Some(id_b), ProtocolMessageTypes::RespondBlocks)));
        assert!(pending.deliver(id_a, msg(Some(id_a), ProtocolMessageTypes::RejectBlocks)));

        let got_a = rx_a.await.expect("waiter A received its reply");
        let got_b = rx_b.await.expect("waiter B received its reply");
        assert_eq!(got_a.id, Some(id_a), "waiter A got another request's reply");
        assert_eq!(got_a.msg_type, ProtocolMessageTypes::RejectBlocks);
        assert_eq!(got_b.id, Some(id_b), "waiter B got another request's reply");
        assert_eq!(got_b.msg_type, ProtocolMessageTypes::RespondBlocks);
    }

    // An id nobody is waiting on (an inbound request to answer, or a stale/late reply) reports
    // `false`, so the read loop falls through to the gossip/handler scan instead of dropping it.
    #[tokio::test]
    async fn deliver_unknown_id_is_not_consumed() {
        let pending = PendingRequests::default();
        assert!(!pending.deliver(4242, msg(Some(4242), ProtocolMessageTypes::NewPeak)));
    }

    // Delivery consumes the waiter: a duplicate/late second reply for the same id is dropped (returns
    // `false`), never routed into an already-satisfied — and now closed — channel. That closed-channel
    // re-delivery was precisely how the true reply got lost under id aliasing.
    #[tokio::test]
    async fn deliver_is_idempotent_after_the_first() {
        let pending = PendingRequests::default();
        let (id, rx) = pending.register();
        assert!(pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)));
        assert!(
            !pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
            "a second reply for a consumed id must not be re-delivered"
        );
        assert!(rx.await.is_ok(), "the one delivery reached the waiter");
    }

    // Cancel (timeout / send failure) frees the waiter slot so the table never leaks; the reply
    // that then shows up is consumed once through the tombstone (never re-delivered after that,
    // and never fanned out to a waiter).
    #[tokio::test]
    async fn cancel_frees_the_slot() {
        let pending = PendingRequests::default();
        let (id, rx) = pending.register();
        pending.cancel(id);
        assert!(
            pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
            "the late reply is consumed"
        );
        assert!(rx.await.is_err(), "the cancelled waiter receives nothing");
        assert!(
            !pending.deliver(id, msg(Some(id), ProtocolMessageTypes::RespondBlocks)),
            "a tombstone excuses exactly one late reply"
        );
    }
}
