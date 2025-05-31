pub mod error;
pub mod farmer;
pub mod full_node;
pub mod harvester;
pub mod introducer;
pub mod pool;
pub mod shared;
pub mod timelord;
pub mod wallet;

use crate::blockchain::sized_bytes::Bytes32;
use crate::utils::await_termination;
use async_trait::async_trait;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::ChiaProtocolVersion;
use dg_xch_serialize::ChiaSerialize;
use futures_util::stream::{FusedStream, SplitSink, SplitStream};
use futures_util::SinkExt;
use futures_util::{Sink, Stream, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use log::{debug, error, info};
use std::collections::HashMap;
use std::fmt;
use std::io::{Cursor, Error};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::select;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

#[repr(u8)]
#[derive(ChiaSerial, Copy, Clone, Debug, PartialEq, Eq)]
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
            _ => ProtocolMessageTypes::Unknown,
        }
    }
}

impl fmt::Display for ProtocolMessageTypes {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
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
    pub data: Vec<u8>,
}
impl ChiaMessage {
    pub fn new<T: ChiaSerialize>(
        msg_type: ProtocolMessageTypes,
        version: ChiaProtocolVersion,
        msg: &T,
        id: Option<u16>,
    ) -> Self {
        ChiaMessage {
            msg_type,
            id,
            data: msg.to_bytes(version),
        }
    }
}
impl From<ChiaMessage> for Message {
    fn from(val: ChiaMessage) -> Self {
        Message::Binary(val.to_bytes(ChiaProtocolVersion::default()).into())
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
        if let Some(s) = &self.msg_type {
            if *s != msg.msg_type {
                return false;
            }
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

pub type PeerMap = Arc<RwLock<HashMap<Bytes32, Arc<SocketPeer>>>>;

pub struct SocketPeer {
    pub node_type: Arc<RwLock<NodeType>>,
    pub protocol_version: Arc<RwLock<ChiaProtocolVersion>>,
    pub websocket: Arc<RwLock<WebsocketConnection>>,
}

pub enum WebsocketMsgStream {
    TokioIo(Box<WebSocketStream<TokioIo<Upgraded>>>),
    Tls(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}
impl Stream for WebsocketMsgStream {
    type Item = Result<Message, tokio_tungstenite::tungstenite::error::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(ref mut s) => Pin::new(s).poll_next(cx),
            WebsocketMsgStream::Tls(ref mut s) => Pin::new(s).poll_next(cx),
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
            WebsocketMsgStream::TokioIo(ref mut s) => Pin::new(s).poll_ready(cx),
            WebsocketMsgStream::Tls(ref mut s) => Pin::new(s).poll_ready(cx),
        }
    }
    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(ref mut s) => Pin::new(s).start_send(item),
            WebsocketMsgStream::Tls(ref mut s) => Pin::new(s).start_send(item),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(ref mut s) => Pin::new(s).poll_flush(cx),
            WebsocketMsgStream::Tls(ref mut s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.get_mut() {
            WebsocketMsgStream::TokioIo(ref mut s) => Pin::new(s).poll_close(cx),
            WebsocketMsgStream::Tls(ref mut s) => Pin::new(s).poll_close(cx),
        }
    }
}

pub struct WebsocketConnection {
    write: SplitSink<WebsocketMsgStream, Message>,
    message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
}
impl WebsocketConnection {
    pub fn new(
        websocket: WebsocketMsgStream,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> (Self, ReadStream) {
        let (write, read) = websocket.split();
        let websocket = WebsocketConnection {
            write,
            message_handlers: message_handlers.clone(),
        };
        let stream = ReadStream {
            read,
            message_handlers,
            peer_id,
            peers,
        };
        (websocket, stream)
    }
    pub async fn send(&mut self, msg: Message) -> Result<(), Error> {
        self.write.send(msg).await.map_err(Error::other)
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
            select! {
                msg = self.read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Binary(bin_data) => {
                                    let mut cursor = Cursor::new(&bin_data);
                                    match ChiaMessage::from_bytes(&mut cursor, protocol_version) {
                                        Ok(chia_msg) => {
                                            let msg_arc: Arc<ChiaMessage> = Arc::new(chia_msg);
                                            let mut matched = false;
                                            for v in self.message_handlers.read().await.values()
                                                .cloned().collect::<Vec<Arc<ChiaMessageHandler>>>() {
                                                if v.filter.matches(msg_arc.as_ref()) {
                                                    let msg_arc_c = msg_arc.clone();
                                                    let peer_id = self.peer_id.clone();
                                                    let peers = self.peers.clone();
                                                    let v_arc_c = v.handle.clone();
                                                    tokio::spawn(async move {
                                                        if let Err(e) = v_arc_c.handle(msg_arc_c.clone(), peer_id, peers).await {
                                                            error!("Error Handling Message({:#?}): {e:?}", msg_arc_c.msg_type);
                                                        }
                                                    });
                                                    matched = true;
                                                }
                                            }
                                            if !matched{
                                                error!("No Matches for Message: {:?}", &msg_arc);
                                            }
                                            debug!("Processed Message: {:?}", &msg_arc.msg_type);
                                        }
                                        Err(e) => {
                                            error!("Invalid Message: {:?}", e);
                                        }
                                    }
                                }
                                Message::Close(e) => {
                                    debug!("Server Got Close Message: {:?}", e);
                                    return;
                                },
                                _ => {
                                    error!("Invalid Message: {:?}", msg);
                                }
                            }
                        }
                        Some(Err(msg)) => {
                            match msg {
                                tokio_tungstenite::tungstenite::Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
                                    debug!("Server Stream Closed without Handshake");
                                },
                                others => {
                                    error!("Server Stream Error: {:?}", others);
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
                    return;
                }
                () = async {
                    loop {
                        if !run.load(Ordering::Relaxed){
                            debug!("Server is exiting");
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
