//! Shared loopback test setup; readiness is observed rather than inferred from a sleep.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;

// Includes scheduling headroom for debug RSA/TLS work on shared CI runners.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn connect_ready(addr: SocketAddr) -> TcpStream {
    tokio::time::timeout(NETWORK_TIMEOUT, async {
        loop {
            match TcpStream::connect(addr).await {
                Ok(stream) => return stream,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("cannot connect to test listener {addr}: {error}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("test listener {addr} did not become ready within {NETWORK_TIMEOUT:?}")
    })
}

pub async fn wait_for_listener(port: u16) {
    // Close before TLS negotiation: this probe must not register a protocol peer.
    drop(connect_ready(([127, 0, 0, 1], port).into()).await);
}
