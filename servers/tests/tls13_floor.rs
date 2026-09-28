// Every server-side socket refuses TLS below 1.3. rustls' default version set also accepts
// TLS 1.2, so the listener must pin the protocol versions explicitly or the floor is lost.
// Since one end of every p2p link is a server, a server-side floor floors the whole network.

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
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_rustls::TlsConnector;

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

// Attempt a TLS handshake pinned to exactly `versions`, with an RSA client cert.
// Returns Ok(()) when the handshake completes.
async fn handshake_with_versions(
    port: u16,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Result<(), String> {
    let mut roots = RootCertStore::empty();
    for cert in load_certs_from_bytes(CHIA_CA_CRT.as_bytes()).expect("server ca") {
        roots.add(cert).expect("add root");
    }
    let (client_cert_pem, client_key_pem) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())
            .expect("client cert");
    let client_config = rustls::ClientConfig::builder_with_protocol_versions(versions)
        .with_root_certificates(roots)
        .with_client_auth_cert(
            load_certs_from_bytes(&client_cert_pem).expect("client certs"),
            load_private_key_from_bytes(&client_key_pem).expect("client key"),
        )
        .expect("client config");
    let connector = TlsConnector::from(Arc::new(client_config));
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
    let tcp = tcp.ok_or_else(|| "server not listening".to_string())?;
    let name = ServerName::try_from("chia.net").expect("server name");
    match tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp)).await {
        Ok(Ok(_tls)) => Ok(()),
        Ok(Err(e)) => Err(format!("{e:?}")),
        Err(_) => Err("handshake timeout".to_string()),
    }
}

// A TLS 1.2-pinned client must be refused, while the same client pinned to 1.3 handshakes fine
// against the same listener.
#[tokio::test(flavor = "multi_thread")]
async fn server_refuses_tls12_and_accepts_tls13() {
    install_test_provider();
    let (port, run) = spawn_server();

    let v13 = handshake_with_versions(port, &[&rustls::version::TLS13]).await;
    assert!(
        v13.is_ok(),
        "a TLS 1.3 client must handshake: {:?}",
        v13.err()
    );

    let v12 = handshake_with_versions(port, &[&rustls::version::TLS12]).await;
    run.store(false, Ordering::Relaxed);
    assert!(
        v12.is_err(),
        "a TLS 1.2-only client must be refused; the server is floored at TLS 1.3"
    );
}
