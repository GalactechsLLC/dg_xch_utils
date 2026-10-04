mod common;

// Inbound `error` protocol frames (code 255) and unknown message types over the real WS
// loopback. The posture:
//   - an `error` frame is DECODED + LOGGED and the connection carries on — no ban, no
//     disconnect;
//   - an undefined message type disconnects the peer with a PROTOCOL_ERROR close and the short
//     INTERNAL_PROTOCOL_ERROR ban, before the rate limiter or any dispatch sees it.

use common::{MemApi, connect, spawn_full_node};
use dg_xch_clients::websocket::oneshot;
use dg_xch_core::protocols::full_node::{RequestPeers, RespondPeers};
use dg_xch_core::protocols::shared::ErrorMessage;
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_serialize::ChiaProtocolVersion;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

fn blind_api() -> Arc<MemApi> {
    Arc::new(MemApi {
        blocks: HashMap::new(),
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    })
}

async fn request_peers_round_trips(client: &dg_xch_clients::websocket::full_node::FullnodeClient) {
    let version = ChiaProtocolVersion::default();
    let _: RespondPeers = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestPeers,
            version,
            &RequestPeers {},
            Some(21),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondPeers),
        version,
        Some(21),
        Some(15000),
    )
    .await
    .expect("the connection still serves after the frame");
}

// A conforming peer's `error` report is a defined message type and must be tolerated: logged,
// never treated as a protocol violation. This guards the unknown-type disconnect from
// over-reaching onto code 255.
#[tokio::test]
async fn error_frame_is_tolerated_and_the_connection_keeps_serving() {
    let server = spawn_full_node(blind_api()).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    let err_frame = ChiaMessage::new(
        ProtocolMessageTypes::Error,
        version,
        &ErrorMessage {
            code: -13,
            message: "INVALID_FEE_TOO_CLOSE_TO_ZERO".to_string(),
            data: None,
        },
        None,
    )
    .unwrap();
    client
        .client
        .connection
        .write()
        .await
        .send(err_frame.into())
        .await
        .expect("error frame sent");

    request_peers_round_trips(&client).await;
    assert_eq!(server.peers.read().await.len(), 1, "peer not evicted");
    assert!(
        server.bans.is_empty(),
        "no ban for a legitimate error frame"
    );
}

// An undefined message type must disconnect the peer with a short host ban: logging it and
// carrying on leaves an unhandled code as a rate-limit bypass.
#[tokio::test]
async fn unknown_message_type_disconnects_and_bans() {
    let server = spawn_full_node(blind_api()).await;
    // Prepare the reconnect identity before the 10-second protocol ban starts.
    // Debug RSA key generation can otherwise consume the entire ban on CI.
    use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
    use dg_xch_core::ssl::{
        generate_ca_signed_cert_data, load_certs_from_bytes, load_private_key_from_bytes,
    };
    let (cert, key) = generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())
        .expect("reconnect certificate");
    let tls = Arc::new(
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(
                dg_xch_core::protocols::shared::NoCertificateVerification,
            ))
            .with_client_auth_cert(
                load_certs_from_bytes(&cert).expect("reconnect cert"),
                load_private_key_from_bytes(&key).expect("reconnect key"),
            )
            .expect("reconnect TLS config"),
    );
    let client = connect(server.port).await;

    // Hand-framed message: type 254 (unassigned), no id, empty length-prefixed body — the wire
    // shape is uint8 type, Optional[uint16] id, bytes data.
    let raw: Vec<u8> = vec![254u8, 0u8, 0, 0, 0, 0];
    client
        .client
        .connection
        .write()
        .await
        .send(Message::Binary(raw.into()))
        .await
        .expect("frame sent");

    // The read loop must evict + close promptly.
    let evicted = common::wait_until(
        || {
            let peers = server.peers.clone();
            async move { peers.read().await.is_empty() }
        },
        common::network::NETWORK_TIMEOUT,
    )
    .await;
    assert!(
        evicted,
        "the peer must be evicted for an unknown message type"
    );
    assert!(
        server
            .bans
            .is_banned(&"127.0.0.1".parse::<std::net::IpAddr>().unwrap()),
        "the host gets the short internal-protocol-error ban"
    );

    // Exercise the TLS/WebSocket accept path with the already prepared identity.
    let reconnect = || {
        tokio_tungstenite::connect_async_tls_with_config(
            format!("wss://127.0.0.1:{}/ws", server.port),
            None,
            false,
            Some(tokio_tungstenite::Connector::Rustls(tls.clone())),
        )
    };
    let refused = tokio::time::timeout(common::network::NETWORK_TIMEOUT, reconnect())
        .await
        .expect("banned reconnect must finish, not time out");
    assert!(
        refused.is_err(),
        "a banned host's reconnect must be refused within the ban window"
    );

    // The same identity succeeds once the ban is cleared: rejection was due to
    // the ban, not an invalid TLS setup.
    server.bans.clear();
    let (mut admitted, _) = tokio::time::timeout(common::network::NETWORK_TIMEOUT, reconnect())
        .await
        .expect("unbanned reconnect finishes")
        .expect("the prepared identity is accepted after the ban lifts");
    admitted.close(None).await.expect("close reconnect");
    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
