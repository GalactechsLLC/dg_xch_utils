mod common;

use common::{connect, contiguous_api, spawn_full_node_rate_limited, try_connect, wait_until};
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use std::net::{IpAddr, Ipv4Addr};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

// Send a raw framed message with no correlation id (unsolicited shape) straight down the socket,
// bypassing the request machinery so the read loop sees exactly what we send.
async fn raw_send(client: &dg_xch_clients::websocket::full_node::FullnodeClient, msg: ChiaMessage) {
    client
        .client
        .connection
        .write()
        .await
        .send(msg.into())
        .await
        .expect("raw send");
}

fn sized_msg(msg_type: ProtocolMessageTypes, len: usize) -> ChiaMessage {
    ChiaMessage {
        msg_type,
        id: None,
        data: UnsizedBytes::new(vec![0u8; len]),
    }
}

// Drive a rate-limit violation and wait for the server to evict + ban the peer.
async fn trip_rate_limit(server: &common::RunningServer) {
    let client = connect(server.port).await;
    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "server registers the inbound peer"
    );
    for _ in 0..6 {
        raw_send(
            &client,
            sized_msg(ProtocolMessageTypes::RequestProofOfWeight, 10),
        )
        .await;
    }
    assert!(
        wait_until(
            || async { server.peers.read().await.is_empty() },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "the flooding peer is closed + evicted"
    );
}

// Headline red→green: a rate-limit violation not only closes the peer, it BANS the host — a reconnect
// from the same host within the window is refused at the accept path. On the pre-ban-list code the
// second connect succeeds (the residual this test closes); with the ban list it fails.
#[tokio::test]
async fn banned_host_cannot_reconnect_within_the_window() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    trip_rate_limit(&server).await;

    assert!(
        wait_until(
            || async { server.bans.is_banned(&LOOPBACK) },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "the violating host is entered into the timed ban list"
    );

    // A fresh dial from the same host is refused at the accept path (HTTP 403 → handshake fails).
    let refused = try_connect(server.port).await;
    assert!(
        refused.is_err(),
        "a reconnect from a banned host must be refused (was allowed on the pre-ban-list code)"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// After the ban lifts (here simulated deterministically by clearing the registry, standing in for the
// 300s expiry the unit test proves), the same host connects fine again — the accept path consults the
// live registry, it is not a permanent block.
#[tokio::test]
async fn host_reconnects_after_the_ban_is_lifted() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    trip_rate_limit(&server).await;
    assert!(
        wait_until(
            || async { server.bans.is_banned(&LOOPBACK) },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "host banned"
    );
    assert!(
        try_connect(server.port).await.is_err(),
        "refused while banned"
    );

    // Ban lifts (expiry / operator reset).
    server.bans.clear();
    assert!(!server.bans.is_banned(&LOOPBACK), "ban cleared");

    // The same host is admitted again.
    let ok = try_connect(server.port).await;
    assert!(
        ok.is_ok(),
        "after the ban lifts the host connects normally: {:?}",
        ok.err()
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn unsolicited_block_reply_bans_the_sender() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    let client = connect(server.port).await;
    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "peer registers"
    );

    // One unsolicited RespondBlock (id=None → not consumed by the correlation fast path → reaches the
    // close arm). Small body so the inbound size limiter passes it through to dispatch.
    raw_send(&client, sized_msg(ProtocolMessageTypes::RespondBlock, 16)).await;

    assert!(
        wait_until(
            || async { server.peers.read().await.is_empty() && server.bans.is_banned(&LOOPBACK) },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "the unsolicited-reply sender is closed + evicted + banned"
    );
    assert!(
        try_connect(server.port).await.is_err(),
        "the banned sender cannot reconnect within the window"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// The ban keys on the REMOTE host, not on our own node identity (cert hash). After a violation the
// registry holds the peer's remote IP (127.0.0.1) — a wire-level confirmation of the host-keying the
// unit test asserts in isolation.
#[tokio::test]
async fn ban_keys_on_remote_host() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    trip_rate_limit(&server).await;

    assert!(
        wait_until(
            || async { server.bans.is_banned(&LOOPBACK) },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "the ban is keyed on the peer's remote host (127.0.0.1), the ban key chia uses"
    );
    // A different host is not swept up by one peer's violation.
    assert!(
        !server
            .bans
            .is_banned(&IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))),
        "an unrelated host is unaffected"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
