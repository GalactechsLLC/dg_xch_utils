mod common;

use common::{
    connect, contiguous_api, rate_limited_client, spawn_full_node_rate_limited, wait_until,
};
use dg_xch_clients::websocket::oneshot_message;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::protocols::full_node::{RejectBlocks, RequestBlocks};
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;
use std::time::Duration;

// Send a raw framed message with an arbitrary body and no correlation id (unsolicited/gossip shape),
// bypassing the request machinery so the read-loop limiter sees exactly what we send.
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

// A message of `msg_type` carrying `len` bytes of body (content irrelevant — the limiter charges the
// wire size, and an oversize body is rejected before the inner type is ever decoded).
fn sized_msg(msg_type: ProtocolMessageTypes, len: usize) -> ChiaMessage {
    ChiaMessage {
        msg_type,
        id: None,
        data: UnsizedBytes::new(vec![0u8; len]),
    }
}

#[tokio::test]
async fn flooding_a_frequency_capped_type_closes_and_evicts_the_peer() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    let client = connect(server.port).await;

    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "server registers the inbound peer after the handshake"
    );

    // Five request_proof_of_weight are within budget; the sixth trips the 5/min cap.
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
        "the 6th request_proof_of_weight must close + evict the peer (chia bans it)"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn a_single_oversized_message_closes_and_evicts_the_peer() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    let client = connect(server.port).await;

    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "server registers the inbound peer"
    );

    // One 200-byte request_block: over the 100-byte per-message cap.
    raw_send(&client, sized_msg(ProtocolMessageTypes::RequestBlock, 200)).await;

    assert!(
        wait_until(
            || async { server.peers.read().await.is_empty() },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "an oversized request_block must close + evict the peer"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// A compliant peer under every budget is never disconnected: five request_proof_of_weight (== the
// cap) leave the peer connected and serving. Guards against a false-positive limiter.
#[tokio::test]
async fn a_compliant_peer_stays_connected() {
    let server = spawn_full_node_rate_limited(contiguous_api(100, 1)).await;
    let client = connect(server.port).await;

    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "server registers the inbound peer"
    );

    for _ in 0..5 {
        raw_send(
            &client,
            sized_msg(ProtocolMessageTypes::RequestProofOfWeight, 10),
        )
        .await;
    }
    // Give the read loop time to process all five; the peer must remain.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.peers.read().await.len(),
        1,
        "a peer at (not over) the cap must stay connected"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn solicited_respond_blocks_burst_does_not_self_trip_the_client_limiter() {
    let server = spawn_full_node_rate_limited(contiguous_api(1_000, 40)).await;
    let client = rate_limited_client(server.port).await;
    let version = ChiaProtocolVersion::default();

    // 40 rapid single-block range pulls: each returns a RespondBlocks (Unlimited, 50 MiB size cap).
    // 40 > the 500/min request_blocks frequency is irrelevant for the reply direction — the point is
    // the inbound RespondBlocks stream stays within budget.
    for h in 1_000u32..1_040 {
        let msg = ChiaMessage::new(
            ProtocolMessageTypes::RequestBlocks,
            version,
            &RequestBlocks {
                start_height: h,
                end_height: h,
                include_transaction_block: false,
            },
            None,
        )
        .expect("encode RequestBlocks");
        let reply = oneshot_message(
            client.client.connection.clone(),
            msg,
            None,
            None,
            Some(15000),
        )
        .await
        .expect("a correlated reply within budget (client not self-closed)");
        assert_eq!(
            reply.msg_type,
            ProtocolMessageTypes::RespondBlocks,
            "each solicited reply is delivered; the client limiter never closed us"
        );
    }

    // The client is still alive after the burst.
    assert!(
        !client.client.is_closed(),
        "client survived its own fetch burst"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// A flooding fake peer sending oversized RejectBlocks to our client would trip the client's inbound
// limiter (reject_blocks is Unlimited(100) — a 200-byte one is a size violation). This proves the
// client-side read-loop enforcement is live, not just the server's.
#[tokio::test]
async fn oversized_reply_from_a_peer_closes_our_client() {
    let server = spawn_full_node_rate_limited(contiguous_api(1_000, 1)).await;
    let client = rate_limited_client(server.port).await;
    let version = ChiaProtocolVersion::default();

    // Sanity: the link works for a normal pull first.
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::RequestBlocks,
        version,
        &RequestBlocks {
            start_height: 1_000,
            end_height: 1_000,
            include_transaction_block: false,
        },
        None,
    )
    .unwrap();
    let reply = oneshot_message(
        client.client.connection.clone(),
        msg,
        None,
        None,
        Some(15000),
    )
    .await
    .expect("normal pull works");
    let _ = RejectBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version);

    // Now have the SERVER push an oversized unsolicited reject_blocks (200 bytes > the 100-byte
    // Unlimited cap) at the client. The client's read-loop limiter must close the connection.
    for p in server.peers.read().await.values() {
        let _ = p
            .websocket
            .write()
            .await
            .send(sized_msg(ProtocolMessageTypes::RejectBlocks, 200).into())
            .await;
    }

    assert!(
        wait_until(
            || async { client.client.is_closed() },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "an oversized reply must close our client's connection"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
