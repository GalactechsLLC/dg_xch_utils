mod common;

// Timelord gating — two rules, both enforced at the p2p layer:
//
// 1. An inbound TIMELORD connection is accepted only from localhost or an exempt peer network.
//    A VDF-producing timelord is node-local infrastructure; a remote "timelord" is free
//    verify-CPU for an attacker.
// 2. The timelord-class messages (new_infusion_point_vdf / new_signage_point_vdf /
//    new_end_of_sub_slot_vdf / respond_compact_proof_of_time) are accepted only from a
//    TIMELORD-typed connection. A sender-type mismatch closes the connection with the short
//    (10 s) ban.

use async_trait::async_trait;
use common::{MemApi, connect, spawn_full_node, wait_until};
use dg_xch_clients::websocket::{WsClient, WsClientConfig};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::protocols::{NodeType, ProtocolMessageTypes};
use dg_xch_p2p::FullNodeApi;
use dg_xch_serialize::ChiaProtocolVersion;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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

// A store-blind api that reports the peer's host as NOT timelord-eligible — the loopback
// stand-in for a REMOTE (non-localhost, non-exempt) source address.
struct RemoteTimelordApi;
#[async_trait]
impl FullNodeApi for RemoteTimelordApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
    fn accept_inbound_timelord(&self, _host: Option<IpAddr>) -> bool {
        false
    }
}

fn client_config(port: u16) -> Arc<WsClientConfig> {
    Arc::new(WsClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        server_port: 0,
        network_id: "mainnet".to_string(),
        ssl_info: None,
        software_version: None,
        protocol_version: ChiaProtocolVersion::default(),
        additional_headers: None,
        rate_limited: false,
    })
}

async fn timelord_client(port: u16) -> Result<WsClient, std::io::Error> {
    WsClient::new(
        client_config(port),
        NodeType::Timelord,
        Arc::new(RwLock::new(HashMap::new())),
        Arc::new(AtomicBool::new(true)),
        15,
    )
    .await
}

// An inbound TIMELORD handshake from an ineligible host must be refused: the peer is dropped
// right after the handshake completes, before any greeting, and without a ban.
#[tokio::test]
async fn remote_timelord_handshake_is_refused() {
    let server = spawn_full_node(Arc::new(RemoteTimelordApi)).await;
    let _client = timelord_client(server.port).await;

    let dropped = wait_until(
        || {
            let peers = server.peers.clone();
            async move { peers.read().await.is_empty() }
        },
        Duration::from_secs(10),
    )
    .await;
    assert!(
        dropped,
        "a TIMELORD handshake from a non-localhost/non-exempt host must be refused"
    );
    assert!(
        server.bans.is_empty(),
        "the inbound-limit refusal closes without banning"
    );
}

// The default predicate accepts a LOCALHOST timelord; the loopback client really is 127.0.0.1,
// so the stock MemApi keeps it connected.
#[tokio::test]
async fn localhost_timelord_handshake_is_accepted() {
    let server = spawn_full_node(blind_api()).await;
    let _client = timelord_client(server.port).await.expect("connects");

    // Give the server a moment to (wrongly) evict; it must still hold the peer.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        server.peers.read().await.len(),
        1,
        "a localhost timelord stays connected"
    );
}

// A timelord-class frame from a FULL_NODE-typed connection is a sender-type violation: close
// plus the short ban. Sent with an empty body, since the sender-type check must fire BEFORE
// decode.
#[tokio::test]
async fn vdf_frame_from_a_full_node_peer_disconnects_and_bans() {
    let server = spawn_full_node(blind_api()).await;
    let client = connect(server.port).await;

    // NewSignagePointVdf (16), no id, empty length-prefixed body.
    let raw: Vec<u8> = vec![
        ProtocolMessageTypes::NewSignagePointVdf as u8,
        0,
        0,
        0,
        0,
        0,
    ];
    client
        .client
        .connection
        .write()
        .await
        .send(Message::Binary(raw.into()))
        .await
        .expect("frame sent");

    let dropped = wait_until(
        || {
            let peers = server.peers.clone();
            async move { peers.read().await.is_empty() }
        },
        Duration::from_secs(10),
    )
    .await;
    assert!(
        dropped,
        "a timelord-class message from a non-timelord peer must disconnect it"
    );
    assert!(
        server
            .bans
            .is_banned(&"127.0.0.1".parse::<IpAddr>().unwrap()),
        "the sender-type violation carries the short protocol ban"
    );
}
