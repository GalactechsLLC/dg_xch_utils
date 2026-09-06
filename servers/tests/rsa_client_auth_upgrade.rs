use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs_from_bytes, load_private_key_from_bytes,
};
use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
use rustls::RootCertStore;
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_rustls::TlsConnector;

// Pin the process crypto provider: in a workspace-wide test build both `ring` and `aws-lc-rs`
// rustls features are unified in, so the builder cannot pick one from crate features alone.
fn install_test_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

fn spawn_server() -> (u16, Arc<AtomicBool>) {
    let port = free_port();
    let config = WebsocketServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        ssl_info: None,
    };
    let server = WebsocketServer::new(
        &config,
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(RwLock::new(HashMap::new())),
    )
    .expect("server");
    let run = Arc::new(AtomicBool::new(true));
    let run_c = run.clone();
    tokio::spawn(async move {
        let _ = server.run(run_c).await;
    });
    (port, run)
}

async fn ws_upgrade_via_tls(port: u16, server_ca: &[u8], client_ca: (&[u8], &[u8])) -> String {
    let mut roots = RootCertStore::empty();
    for cert in load_certs_from_bytes(server_ca).expect("server ca") {
        roots.add(cert).expect("add root");
    }
    let (client_cert_pem, client_key_pem) =
        generate_ca_signed_cert_data(client_ca.0, client_ca.1).expect("client cert");
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            load_certs_from_bytes(&client_cert_pem).expect("client certs"),
            load_private_key_from_bytes(&client_key_pem).expect("client key"),
        )
        .expect("client config");
    let connector = TlsConnector::from(Arc::new(client_config));
    // The accept loop may not be listening the instant the spawn returns — retry the TCP connect
    // briefly (bounded).
    let mut tcp = None;
    for _ in 0..50 {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    let tcp = tcp.expect("server not listening");
    let name = ServerName::try_from("chia.net").expect("server name");
    let mut tls = tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp))
        .await
        .expect("tls timeout")
        .expect("tls handshake");
    tls.write_all(
        b"GET /ws HTTP/1.1\r\n\
          Host: chia.net\r\n\
          Upgrade: websocket\r\n\
          Connection: Upgrade\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
          Sec-WebSocket-Version: 13\r\n\r\n",
    )
    .await
    .expect("write upgrade");
    tls.flush().await.expect("flush");
    // Read the response head. A regression aborts the connection with NO bytes (this read then
    // errors or returns 0) — the exact ServerDisconnectedError shape the incident showed.
    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(10), tls.read(&mut buf))
        .await
        .expect("read timeout")
        .expect("read upgrade response (server aborted the connection: no HTTP response)");
    assert!(
        n > 0,
        "server closed the connection without an HTTP response"
    );
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    head.lines().next().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn rsa_client_cert_upgrades_through_the_default_serve_path() {
    install_test_provider();
    let (port, run) = spawn_server();
    let status = ws_upgrade_via_tls(
        port,
        CHIA_CA_CRT.as_bytes(),
        (CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes()),
    )
    .await;
    run.store(false, Ordering::Relaxed);
    assert!(
        status.contains("101"),
        "default serve path must answer the websocket upgrade with 101 for an RSA client cert, got: {status}"
    );
}
