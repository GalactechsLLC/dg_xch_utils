mod common;

use async_trait::async_trait;
use common::{MemApi, RunningServer, empty_api, install_crypto, spawn_full_node};
use dg_xch_clients::websocket::{WsClient, WsClientConfig};
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::full_node::{NewPeak, RequestMempoolTransactions};
use dg_xch_core::protocols::timelord::NewPeakTimelord;
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_p2p::FullNodeApi;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

// A store-blind API with canned on-connect greetings, used as the loopback server stand-in.
struct GreetingApi {
    inner: Arc<MemApi>,
    peak: Option<NewPeak>,
    timelord: Option<Box<NewPeakTimelord>>,
    filter: Option<Vec<u8>>,
}

#[async_trait]
impl FullNodeApi for GreetingApi {
    async fn block_by_height(
        &self,
        height: u32,
    ) -> Option<Box<dg_xch_core::blockchain::full_block::FullBlock>> {
        self.inner.block_by_height(height).await
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        self.inner.gossip_peers().await
    }
    async fn full_node_peak(&self) -> Option<NewPeak> {
        self.peak.clone()
    }
    async fn timelord_peak(&self) -> Option<Box<NewPeakTimelord>> {
        self.timelord.clone()
    }
    async fn mempool_sync_filter(&self) -> Option<Vec<u8>> {
        self.filter.clone()
    }
}

fn canned_peak() -> NewPeak {
    NewPeak {
        header_hash: Bytes32::from([0xaa; 32]),
        height: 4_040_404,
        weight: 77_777_777,
        fork_point_with_previous_peak: 4_040_404,
        unfinished_reward_block_hash: Bytes32::from([0xbb; 32]),
    }
}

fn canned_timelord_peak() -> Box<NewPeakTimelord> {
    // Field values are opaque to the dispatch layer — arrival + byte-faithful decode is the
    // contract under test, not consensus contents (the server's builder owns those).
    let block = common::load_full_block(5_000_000);
    Box::new(NewPeakTimelord {
        reward_chain_block: block.reward_chain_block,
        difficulty: 9999,
        deficit: 4,
        sub_slot_iters: 578_813_952,
        sub_epoch_summary: None,
        previous_reward_challenges: Vec::new(),
        last_challenge_sb_or_eos_total_iters: 123_456_789,
        passes_ses_height_but_not_yet_included: false,
    })
}

// Capture every push the server sends us (the greeting must arrive unsolicited).
struct PushCapture {
    tx: mpsc::Sender<Arc<ChiaMessage>>,
}

#[async_trait]
impl MessageHandler for PushCapture {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), std::io::Error> {
        let _ = self.tx.send(msg).await;
        Ok(())
    }
}

type HandlerMap = Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>;

fn capture_handlers(
    types: &'static [ProtocolMessageTypes],
) -> (HandlerMap, mpsc::Receiver<Arc<ChiaMessage>>) {
    let (tx, rx) = mpsc::channel(16);
    let mut map = HashMap::new();
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: None,
                id: None,
                custom_fn: Some(Box::new(move |m| types.contains(&m.msg_type))),
            }),
            Arc::new(PushCapture { tx }),
        )),
    );
    (Arc::new(RwLock::new(map)), rx)
}

// Dial the spawned node as `node_type`, with the capture handlers registered BEFORE the
// handshake so the greeting cannot race past them.
async fn dial_as(port: u16, node_type: NodeType, handlers: HandlerMap) -> WsClient {
    install_crypto();
    let cfg = Arc::new(WsClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        network_id: "mainnet".to_string(),
        ssl_info: None,
        software_version: None,
        protocol_version: ChiaProtocolVersion::default(),
        additional_headers: None,
        rate_limited: false,
    });
    WsClient::new(
        cfg,
        node_type,
        handlers,
        Arc::new(AtomicBool::new(true)),
        15,
    )
    .await
    .expect("dial + handshake")
}

async fn recv_within(rx: &mut mpsc::Receiver<Arc<ChiaMessage>>, what: &str) -> Arc<ChiaMessage> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("{what} must arrive within 5s of the handshake"))
        .expect("capture channel open")
}

async fn spawn_greeting_node(api: GreetingApi) -> RunningServer {
    spawn_full_node(Arc::new(api)).await
}

#[tokio::test]
async fn full_node_peer_is_greeted_with_new_peak_on_connect() {
    let server = spawn_greeting_node(GreetingApi {
        inner: empty_api(),
        peak: Some(canned_peak()),
        timelord: None,
        filter: None,
    })
    .await;
    let (handlers, mut rx) = capture_handlers(&[ProtocolMessageTypes::NewPeak]);
    let _client = dial_as(server.port, NodeType::FullNode, handlers).await;

    let msg = recv_within(&mut rx, "the NewPeak greeting").await;
    let got = NewPeak::from_bytes(
        &mut Cursor::new(msg.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("NewPeak decodes");
    assert_eq!(got, canned_peak(), "the greeting carries the current peak");

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn synced_node_requests_mempool_sync_on_full_node_connect() {
    let filter = vec![7u8, 1, 5, 8];
    let server = spawn_greeting_node(GreetingApi {
        inner: empty_api(),
        peak: Some(canned_peak()),
        timelord: None,
        filter: Some(filter.clone()),
    })
    .await;
    let (handlers, mut rx) = capture_handlers(&[ProtocolMessageTypes::RequestMempoolTransactions]);
    let _client = dial_as(server.port, NodeType::FullNode, handlers).await;

    let msg = recv_within(&mut rx, "the RequestMempoolTransactions greeting").await;
    let got = RequestMempoolTransactions::from_bytes(
        &mut Cursor::new(msg.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("RequestMempoolTransactions decodes");
    assert_eq!(got.filter, filter, "the request carries OUR mempool filter");

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn unsynced_node_does_not_request_mempool_sync_on_connect() {
    let server = spawn_greeting_node(GreetingApi {
        inner: empty_api(),
        peak: Some(canned_peak()),
        timelord: None,
        filter: None,
    })
    .await;
    let (handlers, mut rx) = capture_handlers(&[
        ProtocolMessageTypes::RequestMempoolTransactions,
        ProtocolMessageTypes::NewPeak,
    ]);
    let _client = dial_as(server.port, NodeType::FullNode, handlers).await;

    // The NewPeak greeting still arrives; nothing else may.
    let msg = recv_within(&mut rx, "the NewPeak greeting").await;
    assert_eq!(msg.msg_type, ProtocolMessageTypes::NewPeak);
    assert!(
        tokio::time::timeout(Duration::from_millis(600), rx.recv())
            .await
            .is_err(),
        "an unsynced node must not send RequestMempoolTransactions on connect"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn timelord_peer_is_greeted_with_new_peak_timelord() {
    let want = canned_timelord_peak();
    let server = spawn_greeting_node(GreetingApi {
        inner: empty_api(),
        peak: Some(canned_peak()),
        timelord: Some(want.clone()),
        filter: None,
    })
    .await;
    let (handlers, mut rx) = capture_handlers(&[ProtocolMessageTypes::NewPeakTimelord]);
    let _client = dial_as(server.port, NodeType::Timelord, handlers).await;

    let msg = recv_within(&mut rx, "the NewPeakTimelord greeting").await;
    let got = NewPeakTimelord::from_bytes(
        &mut Cursor::new(msg.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("NewPeakTimelord decodes");
    assert_eq!(got, *want, "the greeting carries the timelord peak");

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn peakless_node_sends_no_greeting() {
    let server = spawn_greeting_node(GreetingApi {
        inner: empty_api(),
        peak: None,
        timelord: None,
        filter: None,
    })
    .await;
    let (handlers, mut rx) = capture_handlers(&[
        ProtocolMessageTypes::NewPeak,
        ProtocolMessageTypes::RequestMempoolTransactions,
        ProtocolMessageTypes::NewPeakTimelord,
    ]);
    let _client = dial_as(server.port, NodeType::FullNode, handlers).await;

    assert!(
        tokio::time::timeout(Duration::from_millis(600), rx.recv())
            .await
            .is_err(),
        "no peak → no greeting"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
