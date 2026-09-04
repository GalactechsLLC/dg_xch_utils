// `SendTransaction` (wallet protocol, code 48) served with `TransactionAck` semantics: every
// submit is ACKED — never silently dropped — with SUCCESS(1)/PENDING(2)/FAILED(3) and the
// error-name string in the error field. Duplicates are
// idempotent SUCCESS; a height-parked bundle acks PENDING; a not-synced node acks FAILED
// NO_TRANSACTIONS_WHILE_SYNCING before any CLVM runs. Admission shares the push_tx seam
// (validate → mempool.admit → NewTransaction announce queue), so a p2p-admitted spend is
// announce-queued and block-generator-includable exactly like an HTTP-pushed one.
//
// Both ends run the production stacks: the node under test serves `full_node_handlers`, the
// submitting wallet-side peer dials with `full_node_handlers_client` and correlates the ack by
// request id (the Sage submit path: sage-wallet wallet_peer.rs send_transaction → RequestOrReject
// on TransactionAck).

mod common;

use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode};
use dg_xch_clients::websocket::{WsClient, oneshot_message};
use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::protocols::wallet::{SendTransaction, TransactionAck};
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes, WebsocketConnection};
use dg_xch_p2p::{FullNodeApi, P2pSettings, full_node_handlers_client};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::CoinStore;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;

static DBN: AtomicU64 = AtomicU64::new(0);

fn config(listen: SocketAddr, rpc: SocketAddr) -> Config {
    let n = DBN.fetch_add(1, Ordering::Relaxed);
    let db = std::env::temp_dir().join(format!(
        "full_node_sendtx_{}_{n}.sqlite",
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

// The submitting peer's protocol surface: a wallet-shaped client that only submits (serves nothing).
struct SubmitterApi;

#[async_trait]
impl FullNodeApi for SubmitterApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
}

// A node at the mainnet-fixture peak with the easy coin seeded, serving the production handler
// stack, plus a dialed-in submitting client. `synced` posture is the caller's choice.
async fn rig(synced: bool) -> (Arc<FullNode>, dg_xch_core::blockchain::coin::Coin, WsClient) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    let coin = common::seed_easy_coin(&node.store, 1_000).await;
    node.mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    node.synced.store(synced, Ordering::Relaxed);
    let (server, serve_run, _inbound_peers) = node.build_peer_server().expect("peer server");
    tokio::spawn(async move { server.run(serve_run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let api: Arc<dyn FullNodeApi> = Arc::new(SubmitterApi);
    let handlers = Arc::new(RwLock::new(full_node_handlers_client(
        api,
        "mainnet".to_string(),
        0,
    )));
    let run = Arc::new(AtomicBool::new(true));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        listen.port(),
        handlers,
        run.clone(),
        &P2pSettings::default(),
    )
    .await
    .expect("dial node");
    (node, coin, client)
}

// Submit `bundle` as a wire `SendTransaction` and await the correlated reply — the ack
// wallets block on. Errors when no reply arrives inside `timeout_ms` (the silent-drop failure
// mode this suite guards).
async fn submit(
    connection: &Arc<RwLock<WebsocketConnection>>,
    bundle: &SpendBundle,
    timeout_ms: u64,
) -> Result<TransactionAck, std::io::Error> {
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::SendTransaction,
        ChiaProtocolVersion::default(),
        &SendTransaction {
            transaction: bundle.clone(),
        },
        Some(1),
    )?;
    let reply = oneshot_message(connection.clone(), msg, None, None, Some(timeout_ms)).await?;
    assert_eq!(
        reply.msg_type,
        ProtocolMessageTypes::TransactionAck,
        "SendTransaction must be answered by TransactionAck, got {:?}",
        reply.msg_type
    );
    let mut cursor = Cursor::new(reply.data.bytes.as_slice());
    TransactionAck::from_bytes(&mut cursor, ChiaProtocolVersion::default())
}

// ── 1. The headline gap (was RED: no dispatch arm — every wallet submit timed out) ───────────────
// A valid spend bundle is acked SUCCESS with the txid and no error; the item is mempool-resident;
// and the NewTransaction re-broadcast is queued exactly as the HTTP push_tx path queues it
// (the announce fires for both ingress paths — one admission seam).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_transaction_acks_success_and_admits() {
    let (node, coin, client) = rig(true).await;
    let bundle = common::easy_bundle(&coin, 100);
    let name = bundle.name().expect("bundle name");

    let ack = submit(&client.connection, &bundle, 10_000)
        .await
        .expect("a TransactionAck reply (send_transaction is never silently dropped)");

    assert_eq!(ack.txid, name, "ack carries the spend bundle name");
    assert_eq!(ack.status, TXStatus::SUCCESS, "error: {:?}", ack.error);
    assert_eq!(ack.error, None, "SUCCESS acks with error=None");
    assert!(
        node.mempool.lock().await.get(&name).is_some(),
        "acked-SUCCESS bundle must be mempool-resident"
    );
    let announces: Vec<Bytes32> = node
        .tx_announce
        .lock()
        .await
        .iter()
        .map(|a| a.transaction_id)
        .collect();
    assert_eq!(
        announces,
        vec![name],
        "p2p-admitted tx queues the NewTransaction announce exactly like push_tx"
    );
}

// ── 2. Idempotent duplicate (`send_transaction` fast path) ────────────────────
// Resubmitting a bundle already in the mempool acks SUCCESS — and does NOT queue a second
// announce (the duplicate path returns before the broadcast).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_send_transaction_is_idempotent_success() {
    let (node, coin, client) = rig(true).await;
    let bundle = common::easy_bundle(&coin, 100);
    let name = bundle.name().expect("bundle name");

    let first = submit(&client.connection, &bundle, 10_000)
        .await
        .expect("first ack");
    assert_eq!(first.status, TXStatus::SUCCESS, "error: {:?}", first.error);
    let second = submit(&client.connection, &bundle, 10_000)
        .await
        .expect("second ack");
    assert_eq!(
        second.status,
        TXStatus::SUCCESS,
        "duplicate submit is idempotent SUCCESS, got error: {:?}",
        second.error
    );
    assert_eq!(second.txid, name);
    assert_eq!(second.error, None);
    assert_eq!(
        node.tx_announce.lock().await.len(),
        1,
        "the duplicate must not queue a second NewTransaction announce"
    );
}

// ── 3. Invalid bundles ack FAILED with the exact error-name strings ────────────────────────────────────
// The ack error field is the error-name string: the wallet string-matches these, so the names
// must be exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_bundles_ack_failed_with_chia_err_names() {
    let (node, coin, client) = rig(true).await;

    // Bad aggregate signature: a non-infinity signature over a bundle with no AGG_SIG conditions.
    let mut bad_sig = common::easy_bundle(&coin, 100);
    bad_sig.aggregated_signature = Bytes96::from([0x42; 96]);
    let ack = submit(&client.connection, &bad_sig, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.status, TXStatus::FAILED);
    assert_eq!(ack.error.as_deref(), Some("BAD_AGGREGATE_SIGNATURE"));

    // Double spend: the coin is spent in the confirmed set.
    let spent = common::seed_easy_coin(&node.store, 2_000).await;
    node.store
        .apply_block(common::PEAK_HEIGHT, 0, &[], &[spent.name()])
        .await
        .expect("mark spent");
    let ack = submit(&client.connection, &common::easy_bundle(&spent, 1), 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.status, TXStatus::FAILED);
    assert_eq!(ack.error.as_deref(), Some("DOUBLE_SPEND"));

    // Unmet SECONDS timelock: seconds-based locks fail outright (only height locks pend).
    let locked = common::conditioned_bundle(
        &coin,
        &[
            ConditionWithArgs::ReserveFee(1),
            ConditionWithArgs::AssertSecondsAbsolute(4_000_000_000),
        ],
    );
    let ack = submit(&client.connection, &locked, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.status, TXStatus::FAILED);
    assert_eq!(ack.error.as_deref(), Some("ASSERT_SECONDS_ABSOLUTE_FAILED"));
    assert!(
        node.mempool.lock().await.is_empty(),
        "no rejected bundle may be resident"
    );
}

// ── 4. Height-parked bundle acks PENDING ──────────────────────────────
// An unmet ASSERT_HEIGHT_ABSOLUTE parks the bundle for retry and acks PENDING with the failing
// assert's Err name (→ status PENDING, error ASSERT_HEIGHT_ABSOLUTE_FAILED).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn height_locked_bundle_acks_pending() {
    let (node, coin, client) = rig(true).await;
    let parked = common::conditioned_bundle(
        &coin,
        &[
            ConditionWithArgs::ReserveFee(1),
            ConditionWithArgs::AssertHeightAbsolute(common::PEAK_HEIGHT + 100),
        ],
    );
    let name = parked.name().expect("bundle name");

    let ack = submit(&client.connection, &parked, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.txid, name);
    assert_eq!(
        ack.status,
        TXStatus::PENDING,
        "unmet height assert is PENDING (parked), got error: {:?}",
        ack.error
    );
    assert_eq!(ack.error.as_deref(), Some("ASSERT_HEIGHT_ABSOLUTE_FAILED"));
    assert!(
        node.mempool.lock().await.get(&name).is_none(),
        "a parked bundle is not resident"
    );
    assert!(
        node.tx_announce.lock().await.is_empty(),
        "only successful admissions broadcast, never pending ones (DoS vector)"
    );
}

// ── 5. Not-synced node rejects before validating ───────────────────
// add_transaction gates on synced BEFORE any CLVM: FAILED, Err.NO_TRANSACTIONS_WHILE_SYNCING.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn not_synced_node_acks_failed_no_transactions_while_syncing() {
    let (node, coin, client) = rig(false).await;
    let bundle = common::easy_bundle(&coin, 100);
    let name = bundle.name().expect("bundle name");

    let ack = submit(&client.connection, &bundle, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.txid, name);
    assert_eq!(ack.status, TXStatus::FAILED);
    assert_eq!(ack.error.as_deref(), Some("NO_TRANSACTIONS_WHILE_SYNCING"));
    assert!(node.mempool.lock().await.is_empty());
    assert!(node.tx_announce.lock().await.is_empty());
}

// ── 6. The spend pipeline's last mile: SendTransaction → mempool → block generator ───────────────
// The p2p-admitted item is includable by the producer path (T2-1): create_block_generator selects
// it, and the built transactions remove exactly the submitted coin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admitted_transaction_flows_to_block_generator() {
    let (node, coin, client) = rig(true).await;
    let bundle = common::easy_bundle(&coin, 100);
    let ack = submit(&client.connection, &bundle, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.status, TXStatus::SUCCESS, "error: {:?}", ack.error);

    let tx = node
        .mempool
        .lock()
        .await
        .create_block_generator(&MAINNET, common::PEAK_HEIGHT + 1, Duration::from_secs(2))
        .expect("the acked spend must be selectable into a block generator");
    assert_eq!(
        tx.removals.iter().map(|c| c.name()).collect::<Vec<_>>(),
        vec![coin.name()],
        "the block generator spends exactly the submitted coin"
    );
}

// ── Seen-cache: a known-invalid bundle is not revalidated (mempool) ─────────────────────
// A spend id already in the seen cache (kept there for validation rejects, so known-invalid
// bundles are not re-validated) acks FAILED ALREADY_INCLUDING_TRANSACTION without a second
// CLVM/signature run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn known_invalid_bundle_is_not_revalidated() {
    let (_node, coin, client) = rig(true).await;
    let mut bad_sig = common::easy_bundle(&coin, 100);
    bad_sig.aggregated_signature = Bytes96::from([0x42; 96]);

    let ack = submit(&client.connection, &bad_sig, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.status, TXStatus::FAILED);
    assert_eq!(ack.error.as_deref(), Some("BAD_AGGREGATE_SIGNATURE"));

    // Second submission of the SAME invalid bundle: the seen cache answers.
    let ack = submit(&client.connection, &bad_sig, 10_000)
        .await
        .expect("ack");
    assert_eq!(ack.status, TXStatus::FAILED);
    assert_eq!(
        ack.error.as_deref(),
        Some("ALREADY_INCLUDING_TRANSACTION"),
        "a known-invalid bundle must not be revalidated"
    );
}
