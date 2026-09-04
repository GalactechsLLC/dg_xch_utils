mod common;

use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode, OutboundPeers};
use dg_xch_clients::websocket::{WsClient, WsClientConfig};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::full_node::NewPeak;
use dg_xch_core::protocols::wallet::NewPeakWallet;
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_p2p::{FullNodeApi, OutboundPeer};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

static DBN: AtomicU64 = AtomicU64::new(0);

fn config(listen: SocketAddr, rpc: SocketAddr) -> Config {
    let n = DBN.fetch_add(1, Ordering::Relaxed);
    let db = std::env::temp_dir().join(format!(
        "full_node_syncend_{}_{n}.sqlite",
        std::process::id()
    ));
    Config {
        rpc_tls: dg_full_node::RpcTlsMode::Local,
        debug_endpoints: false,
        p2p: Default::default(),
        listen,
        rpc,
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 0,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
    }
}

fn free_addr() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr")
}

// ---- a recording server the node dials OUT to, capturing the NewPeak it receives -------------
struct PeerSideApi {
    new_peaks: Arc<RwLock<Vec<NewPeak>>>,
}

#[async_trait]
impl FullNodeApi for PeerSideApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
    async fn on_new_peak(&self, _peer: Bytes32, peak: NewPeak) {
        self.new_peaks.write().await.push(peak);
    }
}

// A one-peer OutboundPeers seam over a real dialed link (NewPeak goes to full-node peers).
struct OneOutbound(Arc<OutboundPeer>);

#[async_trait]
impl OutboundPeers for OneOutbound {
    async fn first_live(&self) -> Option<Arc<OutboundPeer>> {
        Some(self.0.clone())
    }
    async fn live_peers(&self) -> Vec<Arc<OutboundPeer>> {
        vec![self.0.clone()]
    }
}

async fn spawn_recording_peer() -> (u16, Arc<AtomicBool>, Arc<RwLock<Vec<NewPeak>>>) {
    use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let port = free_addr().port();
    let new_peaks: Arc<RwLock<Vec<NewPeak>>> = Arc::new(RwLock::new(Vec::new()));
    let api: Arc<dyn FullNodeApi> = Arc::new(PeerSideApi {
        new_peaks: new_peaks.clone(),
    });
    let handlers = dg_xch_p2p::full_node_handlers(api, "mainnet".to_string(), port);
    let peers: PeerMap = Arc::new(RwLock::new(HashMap::new()));
    let server = WebsocketServer::new(
        &WebsocketServerConfig {
            host: "127.0.0.1".to_string(),
            port,
            ssl_info: None,
        },
        peers,
        Arc::new(RwLock::new(handlers)),
    )
    .expect("server");
    let run = Arc::new(AtomicBool::new(true));
    let run_c = run.clone();
    tokio::spawn(async move {
        let _ = server.run(run_c).await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    (port, run, new_peaks)
}

// ---- an inbound wallet peer dialing the node, capturing its NewPeakWallet pushes -------------
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

fn wallet_capture() -> (HandlerMap, mpsc::Receiver<Arc<ChiaMessage>>) {
    let (tx, rx) = mpsc::channel(16);
    let mut map = HashMap::new();
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: Some(ProtocolMessageTypes::NewPeakWallet),
                id: None,
                custom_fn: None,
            }),
            Arc::new(PushCapture { tx }),
        )),
    );
    (Arc::new(RwLock::new(map)), rx)
}

async fn dial_wallet(port: u16, handlers: HandlerMap) -> WsClient {
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
    WsClient::with_ca(
        cfg,
        NodeType::Wallet,
        handlers,
        Arc::new(AtomicBool::new(true)),
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        15,
    )
    .await
    .expect("wallet dial")
}

// The band-exit transition fires all of `_finish_sync`'s peak-post-processing sends at once.
#[tokio::test]
async fn finish_sync_transition_fires_mempool_and_peer_and_wallet_sends() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // The node at the mainnet-fixture peak, peer server up (populates its inbound map).
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    node.synced.store(true, Ordering::Relaxed);
    let (server, run, inbound_peers) = node.build_peer_server().expect("peer server");
    tokio::spawn(async move { server.run(run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A wallet peer dials in (lands in the node's inbound map); drain its on-connect NewPeakWallet
    // greeting so the transition's push is the one we assert on.
    let (wallet_handlers, mut wallet_rx) = wallet_capture();
    let _wallet = dial_wallet(listen.port(), wallet_handlers).await;
    let greeting = tokio::time::timeout(Duration::from_secs(5), wallet_rx.recv())
        .await
        .expect("on-connect wallet greeting arrives")
        .expect("channel open");
    assert_eq!(greeting.msg_type, ProtocolMessageTypes::NewPeakWallet);

    // The node dials OUT to a recording full-node peer (the NewPeak target). Its own on-connect
    // hook is NOT installed on this raw dial, so any NewPeak recorded is the transition's.
    let (peer_port, peer_run, new_peaks) = spawn_recording_peer().await;
    let out_handlers = Arc::new(RwLock::new((node.outbound_handler_factory())()));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        peer_port,
        out_handlers,
        Arc::new(AtomicBool::new(true)),
        &dg_xch_p2p::P2pSettings::default(),
    )
    .await
    .expect("dial recording peer");
    let outbound = Arc::new(OutboundPeer {
        endpoint: ("127.0.0.1".to_string(), peer_port),
        client,
        run: Arc::new(AtomicBool::new(true)),
    });
    let registry: Arc<dyn OutboundPeers> = Arc::new(OneOutbound(outbound));

    // The mempool starts frameless — the transition sets it (`mempool_manager`).
    assert!(
        node.mempool.lock().await.peak().is_none(),
        "mempool has no peak frame before the transition"
    );

    // ---- fire the sync-end transition (the driver's fast-sync-landing call) ----
    node.finish_sync_transition(&registry, &inbound_peers).await;

    // Mempool revalidated against the tx peak.
    assert_eq!(
        node.mempool.lock().await.peak().map(|(h, _)| h),
        Some(common::PEAK_HEIGHT),
        "mempool peak framed at the landed tip"
    );

    // The wallet peer received a SECOND NewPeakWallet (the transition's, not the greeting).
    let msg = tokio::time::timeout(Duration::from_secs(5), wallet_rx.recv())
        .await
        .expect("the transition pushes NewPeakWallet to the wallet peer")
        .expect("channel open");
    let np = NewPeakWallet::from_bytes(
        &mut Cursor::new(msg.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("NewPeakWallet decodes");
    assert_eq!(np.height, common::PEAK_HEIGHT);
    assert_eq!(np.header_hash, common::peak_record().header_hash);
    assert_eq!(
        np.fork_point_with_previous_peak,
        common::PEAK_HEIGHT - 1,
        "sync-end wallet fork point is height-1 (not the on-connect convention)"
    );

    // The outbound full-node peer received NewPeak of the landed tip.
    let mut got = None;
    for _ in 0..50 {
        if let Some(p) = new_peaks.read().await.first().cloned() {
            got = Some(p);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let got = got.expect("the transition broadcasts NewPeak to full-node peers");
    assert_eq!(got.height, common::PEAK_HEIGHT);
    assert_eq!(got.header_hash, common::peak_record().header_hash);
    assert_eq!(
        got.fork_point_with_previous_peak,
        common::PEAK_HEIGHT - 1,
        "sync-end NewPeak fork point is height-1 (the sync-end fork_point default)"
    );

    peer_run.store(false, Ordering::Relaxed);
}
