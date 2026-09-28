mod common;

use async_trait::async_trait;
use common::{MemApi, connect, spawn_full_node, wait_until};
use dg_xch_clients::websocket::oneshot;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::protocols::full_node::RequestBlock;
use dg_xch_core::protocols::rate_limits_v3::configure_message;
use dg_xch_core::protocols::shared::{CAPABILITIES, Capability, ConfigureWindowSizes, Handshake};
use dg_xch_core::protocols::{ChiaMessage, NodeType, ProtocolMessageTypes, SocketPeer};
use dg_xch_p2p::FullNodeApi;
use dg_xch_serialize::ChiaProtocolVersion;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

fn blind_api() -> Arc<MemApi> {
    Arc::new(MemApi {
        blocks: HashMap::new(),
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    })
}

// A store-blind api whose block serving is SLOW — each RequestBlock handler task holds its v3
// receive-window slot for the sleep's duration, so concurrent requests can pile into the window.
struct SlowApi;
#[async_trait]
impl FullNodeApi for SlowApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        tokio::time::sleep(Duration::from_millis(900)).await;
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
}

fn v3_caps() -> Vec<(u16, String)> {
    let mut caps: Vec<(u16, String)> = CAPABILITIES
        .iter()
        .map(|e| (e.0, e.1.to_string()))
        .collect();
    caps.push((Capability::RateLimitsV3 as u16, "1".to_string()));
    caps
}

async fn negotiate_v3(
    client: &dg_xch_clients::websocket::full_node::FullnodeClient,
    version: ChiaProtocolVersion,
) -> Handshake {
    oneshot::<Handshake>(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::Handshake,
            version,
            &Handshake {
                network_id: "mainnet".to_string(),
                protocol_version: version.to_string(),
                software_version: "test-v3-peer".to_string(),
                server_port: 0,
                node_type: NodeType::FullNode as u8,
                capabilities: v3_caps(),
            },
            Some(31),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::Handshake),
        version,
        Some(31),
        Some(15000),
    )
    .await
    .expect("handshake reply")
}

async fn send_plain(
    client: &dg_xch_clients::websocket::full_node::FullnodeClient,
    msg: ChiaMessage,
) {
    client
        .client
        .connection
        .write()
        .await
        .send(msg.into())
        .await
        .expect("send");
}

#[tokio::test]
async fn responder_mirrors_v3_and_activates_after_configure_exchange() {
    let server = spawn_full_node(blind_api()).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    let reply = negotiate_v3(&client, version).await;
    assert!(
        reply
            .capabilities
            .iter()
            .any(|(v, s)| *v == Capability::RateLimitsV3 as u16 && s == "1"),
        "the responder must mirror RATE_LIMITS_V3 in its handshake reply"
    );

    // Not active yet — our configure has not arrived.
    let peer = server.peers.read().await.values().next().cloned().unwrap();
    assert!(peer.v3.is_offered(), "the server offered v3 on this link");
    assert!(!peer.v3.is_active(), "inactive until the peer's configure");

    // Send our settings (the same table — a conforming peer's message).
    send_plain(
        &client,
        ChiaMessage::new(
            ProtocolMessageTypes::ConfigureWindowSizes,
            version,
            &configure_message(),
            None,
        )
        .unwrap(),
    )
    .await;

    let active = wait_until(
        || {
            let peer = peer.clone();
            async move { peer.v3.is_active() }
        },
        Duration::from_secs(5),
    )
    .await;
    assert!(active, "the configure exchange completes v3 activation");
    assert_eq!(server.peers.read().await.len(), 1, "peer stays connected");
}

// A configure that bounds one of OUR unlimited (response) types is an invalid handshake; the
// connection closes.
#[tokio::test]
async fn configure_bounding_our_unlimited_type_is_refused() {
    let server = spawn_full_node(blind_api()).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();
    let _ = negotiate_v3(&client, version).await;

    send_plain(
        &client,
        ChiaMessage::new(
            ProtocolMessageTypes::ConfigureWindowSizes,
            version,
            &ConfigureWindowSizes {
                settings: vec![(ProtocolMessageTypes::RespondBlocks as u8, 1u16)],
            },
            None,
        )
        .unwrap(),
    )
    .await;

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
        "bounding our unlimited RespondBlocks must close the connection (INVALID_HANDSHAKE)"
    );
}

// A ConfigureWindowSizes on a link where v3 was never negotiated is a protocol violation.
#[tokio::test]
async fn configure_without_negotiation_is_refused() {
    let server = spawn_full_node(blind_api()).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    send_plain(
        &client,
        ChiaMessage::new(
            ProtocolMessageTypes::ConfigureWindowSizes,
            version,
            &configure_message(),
            None,
        )
        .unwrap(),
    )
    .await;

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
        "an unnegotiated ConfigureWindowSizes must close the connection"
    );
}

// Window enforcement: with v3 active and the peer NOT localhost/exempt, a third concurrent
// RequestBlock (window 2) while two are still being processed closes the connection with the
// RATE_LIMITER ban. The loopback peer's recorded host is swapped to a public address to step
// past the localhost bypass; the bypass itself is the second half of the test.
#[tokio::test]
async fn third_concurrent_request_over_the_window_disconnects_and_bans() {
    let server = spawn_full_node(Arc::new(SlowApi)).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();
    let _ = negotiate_v3(&client, version).await;
    send_plain(
        &client,
        ChiaMessage::new(
            ProtocolMessageTypes::ConfigureWindowSizes,
            version,
            &configure_message(),
            None,
        )
        .unwrap(),
    )
    .await;
    let peer = server.peers.read().await.values().next().cloned().unwrap();
    assert!(
        wait_until(
            || {
                let peer = peer.clone();
                async move { peer.v3.is_active() }
            },
            Duration::from_secs(5),
        )
        .await
    );

    // Swap the recorded host to a non-loopback address so enforcement applies.
    let (peer_id, old) = {
        let map = server.peers.read().await;
        let (k, v) = map.iter().next().unwrap();
        (*k, v.clone())
    };
    let swapped = Arc::new(SocketPeer {
        peer_peak: old.peer_peak.clone(),
        node_type: old.node_type.clone(),
        protocol_version: old.protocol_version.clone(),
        capabilities: old.capabilities.clone(),
        websocket: old.websocket.clone(),
        host: Some("203.0.113.7".parse::<IpAddr>().unwrap()),
        bans: old.bans.clone(),
        outbound_limiter: old.outbound_limiter.clone(),
        v3: old.v3.clone(),
    });
    server.peers.write().await.insert(peer_id, swapped);

    // Four back-to-back RequestBlocks with distinct ids against the slow api: two occupy the
    // window for ~900 ms and a subsequent one must trip it. (Four, not three: the read loop
    // snapshots the peer entry at the top of each iteration, so the single iteration already
    // parked on the socket when the host swap landed may still see the loopback host and
    // bypass enforcement for exactly one frame.)
    for id in [41u16, 42, 43, 44] {
        send_plain(
            &client,
            ChiaMessage::new(
                ProtocolMessageTypes::RequestBlock,
                version,
                &RequestBlock {
                    height: 1,
                    include_transaction_block: false,
                },
                Some(id),
            )
            .unwrap(),
        )
        .await;
    }

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
        "the third concurrent RequestBlock exceeds the v3 window (2) and must disconnect"
    );
    assert!(
        server
            .bans
            .is_banned(&"203.0.113.7".parse::<IpAddr>().unwrap()),
        "the window violation carries the RATE_LIMITER ban"
    );
}

// The localhost bypass: the SAME three-concurrent burst from a genuine loopback peer is NOT
// enforced for a localhost or exempt peer, and the connection keeps serving.
#[tokio::test]
async fn localhost_peer_bypasses_window_enforcement() {
    let server = spawn_full_node(Arc::new(SlowApi)).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();
    let _ = negotiate_v3(&client, version).await;
    send_plain(
        &client,
        ChiaMessage::new(
            ProtocolMessageTypes::ConfigureWindowSizes,
            version,
            &configure_message(),
            None,
        )
        .unwrap(),
    )
    .await;
    let peer = server.peers.read().await.values().next().cloned().unwrap();
    assert!(
        wait_until(
            || {
                let peer = peer.clone();
                async move { peer.v3.is_active() }
            },
            Duration::from_secs(5),
        )
        .await
    );

    for id in [51u16, 52, 53, 54] {
        send_plain(
            &client,
            ChiaMessage::new(
                ProtocolMessageTypes::RequestBlock,
                version,
                &RequestBlock {
                    height: 1,
                    include_transaction_block: false,
                },
                Some(id),
            )
            .unwrap(),
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        server.peers.read().await.len(),
        1,
        "a localhost peer is exempt from window enforcement"
    );
    assert!(server.bans.is_empty());
}
