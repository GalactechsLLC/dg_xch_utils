// The HTTP RPC envelope and the 8555 TLS posture.
//
// Envelope: every response is `{"<named_key>": ..., "success": true}` and every application
// error is an HTTP-200 `{"success": false, "error": ...}`. The
// envelope tests exercise the Portfu routes and deserialize through dg_xch_clients' REAL
// response wrappers (the exact structs a Rust RPC client parses).
//
// TLS: Portfu serves the 8555 posture with a private-CA trust store.
// The end-to-end tests run the real Portfu accept loop and connect with the real
// `FullnodeClient` (client certs + https + envelope parse); the negative
// tests prove a no-cert client and a wrong-CA client are refused at the handshake.

mod common;

use dg_full_node::{Node, build_portfu_rpc_tls_context};
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::api::responses::full_node_responses::{
    BlockRecordResp, CoinRecordAryResp, FullBlockResp, TXResp,
};
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::ssl::generate_ca_signed_cert_data;
use dg_xch_node::Mempool;
use http::StatusCode;
use portfu::prelude::{ServerBuilder, ServerHandle};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

async fn seeded_rpc() -> (Arc<Node>, Arc<Mutex<Mempool>>) {
    let store = Arc::new(common::open_store().await);
    common::seed_peak(&store).await;
    let mempool = Arc::new(Mutex::new(Mempool::new(&MAINNET)));
    let node = Node::new(
        store,
        mempool.clone(),
        MAINNET,
        Arc::new(AtomicBool::new(true)),
        Arc::new(Mutex::new(Vec::new())),
    );
    (Arc::new(node), mempool)
}

async fn call(port: u16, path: &str, body: Value) -> (StatusCode, Value) {
    call_raw(port, path, body.to_string().into_bytes()).await
}

async fn call_raw(port: u16, path: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let config = Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    );
    let connector = tokio_rustls::TlsConnector::from(config);
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect RPC");
    let server_name = rustls::pki_types::ServerName::try_from("localhost").expect("name");
    let mut tls = connector.connect(server_name, tcp).await.expect("RPC TLS");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    tls.write_all(request.as_bytes())
        .await
        .expect("write headers");
    tls.write_all(&body).await.expect("write body");

    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    let body_end = loop {
        let read = tls.read(&mut chunk).await.expect("read response");
        assert_ne!(read, 0, "response ended before its body was complete");
        response.extend_from_slice(&chunk[..read]);
        let Some(header_end) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&response[..header_end]).expect("response headers");
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            })
            .expect("content length");
        let end = header_end + 4 + content_length;
        if response.len() >= end {
            break end;
        }
    };
    let header_end = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header terminator");
    let status = std::str::from_utf8(&response[..header_end])
        .expect("response headers")
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .and_then(|code| StatusCode::from_u16(code).ok())
        .expect("response status");
    let response_body = &response[header_end + 4..body_end];
    let value = serde_json::from_slice(response_body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(response_body).into_owned()));
    (status, value)
}

// ---- envelope shape ---------------------------------------------------------------------------

// Success envelope: `{"blockchain_state": {...}, "success": true}` with the full state shape
// (peak as a full block record, sync sub-object, mempool gauges, average_block_time).
#[tokio::test]
async fn envelope_blockchain_state_named_key_and_success() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let (status, v) = call(port, "/get_blockchain_state", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["success"], Value::from(true));
    let state = &v["blockchain_state"];
    assert_eq!(
        state["peak"]["height"].as_u64(),
        Some(u64::from(common::PEAK_HEIGHT))
    );
    assert_eq!(state["sync"]["synced"], Value::from(true));
    assert!(state["mempool_size"].is_u64());
    assert!(state["block_max_cost"].is_u64());
    assert!(
        state
            .as_object()
            .expect("obj")
            .contains_key("average_block_time"),
        "the average_block_time key is present"
    );
    assert!(state.as_object().expect("obj").contains_key("node_id"));
    run.shutdown();
}

// Error envelope: an application error is HTTP-200 `{"success": false, "error": ...}` with the
// traceback/structuredError keys — never an HTTP 4xx/5xx.
#[tokio::test]
async fn envelope_errors_are_http_200_success_false() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let bogus = Bytes32::from([0x42u8; 32]);
    let (status, v) = call(port, "/get_block", json!({"header_hash": bogus})).await;
    assert_eq!(status, StatusCode::OK, "app errors are HTTP 200");
    assert_eq!(v["success"], Value::from(false));
    assert!(
        v["error"]
            .as_str()
            .expect("error string")
            .contains("not found"),
        "the error message survives: {v}"
    );
    let obj = v.as_object().expect("obj");
    assert!(obj.contains_key("traceback"));
    assert!(obj.contains_key("structuredError"));

    // A malformed / missing-parameter body is the same envelope.
    let (status, v) = call(port, "/get_block", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["success"], Value::from(false));
    run.shutdown();
}

// Unknown endpoints are HTTP 404.
#[tokio::test]
async fn unknown_endpoint_is_404() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let (status, _v) = call(port, "/get_nonexistent", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    run.shutdown();
}

// An oversize body is refused with 413 (the 1 MiB body cap).
#[tokio::test]
async fn oversize_body_is_413() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let body = vec![b'{'; dg_full_node::rpc::MAX_RPC_BODY_BYTES + 1];
    let (status, _v) = call_raw(port, "/get_blockchain_state", body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    run.shutdown();
}

// The client-shaped assertion: the coin_records envelope parses through dg_xch_clients'
// real response wrapper (`response["coin_records"]` + `response["success"]`).
#[tokio::test]
async fn envelope_coin_records_parse_as_chia_client_shape() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let name = common::additions()[0].coin.name();
    let (status, v) = call(port, "/get_coin_records_by_names", json!({"names": [name]})).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: CoinRecordAryResp = serde_json::from_value(v).expect("client shape");
    assert!(parsed.success);
    assert_eq!(parsed.coin_records.len(), 1);
    assert_eq!(parsed.coin_records[0].coin.name(), name);
    run.shutdown();
}

// get_block / get_block_record envelopes parse through the client wrappers.
#[tokio::test]
async fn envelope_block_and_record_parse_as_chia_client_shape() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let hh = common::peak_record().header_hash;
    let (_s, v) = call(port, "/get_block", json!({"header_hash": hh})).await;
    let parsed: FullBlockResp = serde_json::from_value(v).expect("block shape");
    assert!(parsed.success);
    assert_eq!(parsed.block.header_hash().expect("hh"), hh);

    let (_s, v) = call(
        port,
        "/get_block_record_by_height",
        json!({"height": common::PEAK_HEIGHT}),
    )
    .await;
    let parsed: BlockRecordResp = serde_json::from_value(v).expect("record shape");
    assert!(parsed.success);
    assert_eq!(parsed.block_record.header_hash, hh);
    run.shutdown();
}

// push_tx answers `{"status": "SUCCESS"}`, idempotent on a duplicate; the status parses
// through the client's TXResp.
#[tokio::test]
async fn envelope_push_tx_status_success_and_idempotent() {
    let (port, run, mempool, rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    let coin = common::seed_easy_coin(rpc.store.as_ref(), 1_000).await;
    let bundle = common::easy_bundle(&coin, 1);
    let body = json!({"spend_bundle": bundle});
    let (status, v) = call(port, "/push_tx", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: TXResp = serde_json::from_value(v).expect("tx shape");
    assert!(parsed.success);
    assert_eq!(parsed.status, TXStatus::SUCCESS);
    // Duplicate: SUCCESS again, still one resident item.
    let (_s, v) = call(port, "/push_tx", body).await;
    let parsed: TXResp = serde_json::from_value(v).expect("tx shape");
    assert_eq!(parsed.status, TXStatus::SUCCESS);
    assert_eq!(mempool.lock().await.len(), 1);
    run.shutdown();
}

// The utility endpoints: healthz, get_routes, get_version, get_network_info,
// get_aggsig_additional_data (plain hex, `.hex()`), get_connections (empty sans live).
#[tokio::test]
async fn envelope_utility_endpoints() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;

    let (status, v) = call(port, "/healthz", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, json!({"success": true}));

    let (_s, v) = call(port, "/get_routes", json!({})).await;
    assert_eq!(v["success"], Value::from(true));
    let routes: Vec<String> = serde_json::from_value(v["routes"].clone()).expect("routes list");
    assert!(routes.contains(&"/get_blockchain_state".to_string()));
    assert!(routes.contains(&"/push_tx".to_string()));

    let (_s, v) = call(port, "/get_version", json!({})).await;
    assert_eq!(v["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));

    let (_s, v) = call(port, "/get_network_info", json!({})).await;
    assert_eq!(v["network_name"].as_str(), Some("mainnet"));
    assert_eq!(v["network_prefix"].as_str(), Some("xch"));
    let genesis = v["genesis_challenge"].as_str().expect("genesis");
    assert!(!genesis.starts_with("0x"), "plain hex is served here");

    let (_s, v) = call(port, "/get_aggsig_additional_data", json!({})).await;
    let data = v["additional_data"].as_str().expect("additional data");
    assert!(!data.starts_with("0x"), "no 0x prefix on this field");
    assert_eq!(
        format!("0x{data}"),
        MAINNET.agg_sig_me_additional_data.to_string()
    );

    let (_s, v) = call(port, "/get_connections", json!({})).await;
    assert_eq!(v["success"], Value::from(true));
    assert!(v["connections"].as_array().expect("array").is_empty());
    run.shutdown();
}

// The empty body of a GET-style probe reads as `{}` for all-optional endpoints and errors
// cleanly (not panics) for required-parameter endpoints.
#[tokio::test]
async fn empty_body_handling() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let (status, v) = call_raw(port, "/get_connections", Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["success"], Value::from(true));
    let (status, v) = call_raw(port, "/get_block", Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["success"], Value::from(false));
    run.shutdown();
}

// ---- TLS posture over the real accept loop ----------------------------------------------------

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

// Per-process test SSL dir holding the RPC private CA.
// shares one private CA and `write_client_certs` can sign client certs against it.
fn test_ssl_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rpc_http_ssl_{}", std::process::id()))
}

fn ensure_test_private_ca() -> std::path::PathBuf {
    use std::sync::OnceLock;
    static ONCE: OnceLock<std::path::PathBuf> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = test_ssl_dir();
        let ca_dir = dir.join("ca");
        std::fs::create_dir_all(&ca_dir).expect("mk ssl dir");
        let crt = ca_dir.join("private_ca.crt");
        let key = ca_dir.join("private_ca.key");
        if !(crt.exists() && key.exists()) {
            dg_xch_core::ssl::make_ca_cert(&crt, &key).expect("generate private CA");
        }
        dir
    })
    .clone()
}

fn write_client_certs(tag: &str) -> ClientSSLConfig {
    let ca_dir = ensure_test_private_ca().join("ca");
    let ca_crt = std::fs::read(ca_dir.join("private_ca.crt")).expect("private CA crt");
    let ca_key = std::fs::read(ca_dir.join("private_ca.key")).expect("private CA key");
    let (crt, key) = generate_ca_signed_cert_data(&ca_crt, &ca_key).expect("client cert");
    let dir = std::env::temp_dir();
    let crt_path = dir.join(format!("rpc_http_{}_{tag}.crt", std::process::id()));
    let key_path = dir.join(format!("rpc_http_{}_{tag}.key", std::process::id()));
    std::fs::write(&crt_path, &crt).expect("write crt");
    std::fs::write(&key_path, &key).expect("write key");
    ClientSSLConfig {
        ssl_crt_path: crt_path.to_string_lossy().to_string(),
        ssl_key_path: key_path.to_string_lossy().to_string(),
        ssl_ca_crt_path: String::new(),
    }
}

async fn spawn_tls_server() -> (u16, ServerHandle, Arc<Mutex<Mempool>>, Arc<Node>) {
    spawn_tls_server_mode(dg_full_node::RpcTlsMode::PrivateCa {
        ssl_dir: ensure_test_private_ca(),
    })
    .await
}

async fn spawn_tls_server_mode(
    mode: dg_full_node::RpcTlsMode,
) -> (u16, ServerHandle, Arc<Mutex<Mempool>>, Arc<Node>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (rpc, mempool) = seeded_rpc().await;
    let port = free_port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().expect("bind addr");
    let tls = build_portfu_rpc_tls_context(&mode, bind).expect("tls context");
    let server = ServerBuilder::new()
        .host("127.0.0.1")
        .port(port)
        .tls(tls.tls_config)
        .shutdown_grace_period(Duration::from_millis(100))
        .global_state::<dg_full_node::Node>(rpc.clone())
        .build();
    let run = server.handle();
    tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    (port, run, mempool, rpc)
}

// End-to-end with the REAL Rust RPC client: client cert signed by our CA is
// accepted, https + envelope parse round-trips four representative endpoints.
#[tokio::test]
async fn tls_e2e_chia_client_four_endpoints() {
    let (port, run, mempool, rpc) = spawn_tls_server().await;
    mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    let coin = common::seed_easy_coin(rpc.store.as_ref(), 1_000).await;

    let ssl = write_client_certs("e2e");
    let client = FullnodeClient::new("127.0.0.1", port, 15, Some(ssl), &None).expect("client");

    // 1. get_blockchain_state
    let state = client.get_blockchain_state().await.expect("state");
    assert_eq!(
        state.peak.as_ref().map(|p| p.height),
        Some(common::PEAK_HEIGHT)
    );
    assert!(state.sync.synced);

    // 2. get_block
    let hh = common::peak_record().header_hash;
    let block = client.get_block(&hh).await.expect("block");
    assert_eq!(block.header_hash().expect("hh"), hh);

    // 3. get_coin_records_by_names (spent default false — the seeded additions are unspent)
    let name = common::additions()[0].coin.name();
    let records = client
        .get_coin_records_by_names(&[name], None, None, None)
        .await
        .expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].coin.name(), name);

    // 4. push_tx (typed status through the envelope)
    let status = client
        .push_tx(&common::easy_bundle(&coin, 1))
        .await
        .expect("push");
    assert_eq!(status, TXStatus::SUCCESS);
    assert_eq!(mempool.lock().await.len(), 1);

    // The error envelope surfaces as a typed client error, not a decode failure.
    let err = client
        .get_block(&Bytes32::from([0x24u8; 32]))
        .await
        .expect_err("unknown block errors");
    assert!(
        err.error.unwrap_or_default().contains("not found"),
        "server error string reaches the client"
    );

    run.shutdown();
}

// Route policy is PrivateCa | Loopback, so a loopback client does not need a certificate even when
// the listener also trusts private-CA clients from remote addresses.
#[tokio::test]
async fn tls_no_client_cert_is_allowed_from_loopback() {
    let (port, run, _mp, _rpc) = spawn_tls_server().await;
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let failed = raw_request_fails(port, Arc::new(cfg)).await;
    assert!(
        !failed,
        "a loopback client must be accepted without a certificate"
    );
    run.shutdown();
}

// TLS-posture negative: a client certificate from the WRONG CA (here: signed by a mere leaf,
// so it cannot chain to the server's root) is refused at the handshake.
#[tokio::test]
async fn tls_wrong_ca_client_cert_is_refused() {
    use dg_xch_core::ssl::{load_certs_from_bytes, load_private_key_from_bytes};
    let (port, run, _mp, _rpc) = spawn_tls_server().await;
    // A cert signed by a LEAF of the real CA — presentable, but it does not chain to the CA root.
    let (leaf_crt, leaf_key) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()).expect("leaf");
    let (wrong_crt, wrong_key) =
        generate_ca_signed_cert_data(&leaf_crt, &leaf_key).expect("wrong-ca cert");
    let certs = load_certs_from_bytes(&wrong_crt).expect("certs");
    let key = load_private_key_from_bytes(&wrong_key).expect("key");
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)
        .expect("client auth");
    let refused = raw_request_fails(port, Arc::new(cfg)).await;
    assert!(refused, "a wrong-CA client must not get an HTTP response");
    run.shutdown();
}

// Positive control for the raw path: the SAME raw client WITH a CA-chained cert gets an HTTP
// response — so the negatives above fail for the cert reason, not a plumbing one.
#[tokio::test]
async fn tls_raw_client_with_valid_cert_succeeds() {
    use dg_xch_core::ssl::{load_certs_from_bytes, load_private_key_from_bytes};
    let (port, run, _mp, _rpc) = spawn_tls_server().await;
    let ca_dir = ensure_test_private_ca().join("ca");
    let ca_crt = std::fs::read(ca_dir.join("private_ca.crt")).expect("private CA crt");
    let ca_key = std::fs::read(ca_dir.join("private_ca.key")).expect("private CA key");
    let (crt, key_bytes) = generate_ca_signed_cert_data(&ca_crt, &ca_key).expect("cert");
    let certs = load_certs_from_bytes(&crt).expect("certs");
    let key = load_private_key_from_bytes(&key_bytes).expect("key");
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)
        .expect("client auth");
    let refused = raw_request_fails(port, Arc::new(cfg)).await;
    assert!(!refused, "a CA-chained client cert is accepted");
    run.shutdown();
}

// Issue one POST /healthz over TLS with the given client config; true means the handshake failed or
// the RPC route returned a non-200 response.
async fn raw_request_fails(port: u16, cfg: Arc<rustls::ClientConfig>) -> bool {
    let connector = tokio_rustls::TlsConnector::from(cfg);
    let Ok(tcp) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return true;
    };
    let server_name = rustls::pki_types::ServerName::try_from("localhost").expect("name");
    let Ok(mut tls) = connector.connect(server_name, tcp).await else {
        return true;
    };
    let req = "POST /healthz HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    if tls.write_all(req.as_bytes()).await.is_err() {
        return true;
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    !String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200")
}

#[tokio::test]
async fn tls_public_chia_ca_client_is_rejected() {
    use dg_xch_core::ssl::{load_certs_from_bytes, load_private_key_from_bytes};
    let (port, run, _mp, _rpc) = spawn_tls_server().await;
    let (crt, key_bytes) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())
            .expect("attacker cert signed by the world-public Chia CA");
    let certs = load_certs_from_bytes(&crt).expect("certs");
    let key = load_private_key_from_bytes(&key_bytes).expect("key");
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)
        .expect("client auth");
    let refused = raw_request_fails(port, Arc::new(cfg)).await;
    assert!(
        refused,
        "a client whose cert merely chains to the world-public Chia CA must NOT reach the RPC"
    );
    run.shutdown();
}

// A client cert signed by the node's private CA is accepted.
#[tokio::test]
async fn tls_private_ca_client_is_accepted() {
    use dg_xch_core::ssl::{load_certs_from_bytes, load_private_key_from_bytes};
    let (port, run, _mp, _rpc) = spawn_tls_server().await;
    let ca_dir = ensure_test_private_ca().join("ca");
    let ca_crt = std::fs::read(ca_dir.join("private_ca.crt")).expect("private CA crt");
    let ca_key = std::fs::read(ca_dir.join("private_ca.key")).expect("private CA key");
    let (crt, key_bytes) = generate_ca_signed_cert_data(&ca_crt, &ca_key).expect("client cert");
    let certs = load_certs_from_bytes(&crt).expect("certs");
    let key = load_private_key_from_bytes(&key_bytes).expect("key");
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)
        .expect("client auth");
    let refused = raw_request_fails(port, Arc::new(cfg)).await;
    assert!(
        !refused,
        "a client cert chained to the node's private CA must be accepted"
    );
    run.shutdown();
}

// `--rpc-tls local` on a loopback bind requires no client cert.
#[tokio::test]
async fn tls_local_mode_allows_no_client_cert_on_loopback() {
    let (port, run, _mp, _rpc) = spawn_tls_server_mode(dg_full_node::RpcTlsMode::Local).await;
    let verifier = Arc::new(dg_xch_core::protocols::shared::NoCertificateVerification);
    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let refused = raw_request_fails(port, Arc::new(cfg)).await;
    assert!(
        !refused,
        "local mode on loopback must serve a cert-less client"
    );
    run.shutdown();
}

// `--rpc-tls local` is unauthenticated, so it must refuse to build on a routable (non-loopback)
// bind: an unauthenticated RPC can never be exposed to the network.
#[test]
fn tls_local_mode_refuses_non_loopback_bind() {
    let bind: SocketAddr = "0.0.0.0:8555".parse().expect("addr");
    match build_portfu_rpc_tls_context(&dg_full_node::RpcTlsMode::Local, bind) {
        Ok(_) => panic!("local mode on a 0.0.0.0 bind must fail closed"),
        Err(err) => assert!(
            err.to_string().contains("loopback"),
            "the error must name the loopback requirement, got: {err}"
        ),
    }
}

#[test]
fn local_mode_downgrades_routable_bind_to_loopback() {
    use dg_full_node::RpcTlsMode;
    let routable: SocketAddr = "0.0.0.0:8555".parse().unwrap();
    let (bind, downgraded) = RpcTlsMode::Local.resolve_bind(routable);
    assert!(
        downgraded,
        "a routable local-mode bind must be flagged downgraded"
    );
    assert!(bind.ip().is_loopback(), "downgraded bind must be loopback");
    assert_eq!(bind.port(), 8555, "port is preserved");
    // A loopback bind is left untouched.
    let loop_in: SocketAddr = "127.0.0.1:8555".parse().unwrap();
    let (b2, d2) = RpcTlsMode::Local.resolve_bind(loop_in);
    assert!(!d2 && b2 == loop_in, "loopback local bind is untouched");
    let (b3, d3) = RpcTlsMode::PrivateCa {
        ssl_dir: std::path::PathBuf::from("ssl"),
    }
    .resolve_bind(routable);
    assert!(
        !d3 && b3 == routable,
        "private CA binds exactly as configured"
    );
}
