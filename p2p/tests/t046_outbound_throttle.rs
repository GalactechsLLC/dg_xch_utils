mod common;

use common::{
    connect, contiguous_api, rate_limited_client, spawn_full_node_rate_limited, wait_until,
};
use dg_xch_core::protocols::full_node::RequestBlocks;
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_serialize::ChiaProtocolVersion;
use std::time::{Duration, Instant};

// A rate-limited client's `WsClient::send` installs the outbound limiter, yet a burst of under-budget
// requests (request_blocks is 500/min, rate_limit_numbers.py:99) is admitted with no pacing at all —
// 60 sends complete far under a single 1s re-queue delay, and the client stays connected. This is the
// "our normal operation is unaffected" guarantee at the wire.
#[tokio::test]
async fn outbound_throttle_does_not_delay_under_budget_requests() {
    let server = spawn_full_node_rate_limited(contiguous_api(1_000, 1)).await;
    let client = rate_limited_client(server.port).await;
    let version = ChiaProtocolVersion::default();

    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "server registers the inbound peer after the handshake"
    );

    let start = Instant::now();
    for h in 0u32..60 {
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
        // Route through the throttle-equipped send path.
        client
            .client
            .send(msg)
            .await
            .expect("throttled send succeeds");
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "under-budget sends must not be paced (took {elapsed:?}, a single defer is 1s)"
    );
    assert!(
        !client.client.is_closed(),
        "the client stays connected after its own under-budget burst"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn non_rate_limited_client_send_path_is_unthrottled() {
    let server = spawn_full_node_rate_limited(contiguous_api(1_000, 1)).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    assert!(
        wait_until(
            || async { server.peers.read().await.len() == 1 },
            common::network::NETWORK_TIMEOUT,
        )
        .await,
        "server registers the inbound peer"
    );

    let start = Instant::now();
    for h in 0u32..60 {
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
        client.client.send(msg).await.expect("direct send succeeds");
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "an unpoliced link writes directly, never paced"
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
