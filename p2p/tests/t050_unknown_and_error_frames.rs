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
use std::time::Duration;
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
        Duration::from_secs(10),
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

    // And the ban is enforced at the accept path: an immediate reconnect is refused.
    let reconnect = dg_xch_clients::websocket::full_node::FullnodeClient::new(
        Arc::new(dg_xch_clients::websocket::WsClientConfig {
            host: "127.0.0.1".to_string(),
            port: server.port,
            server_port: 0,
            network_id: "mainnet".to_string(),
            ssl_info: None,
            software_version: None,
            protocol_version: ChiaProtocolVersion::default(),
            additional_headers: None,
            rate_limited: false,
        }),
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
        None,
        5,
    )
    .await;
    assert!(
        reconnect.is_err(),
        "a banned host's reconnect must be refused within the ban window"
    );
}
