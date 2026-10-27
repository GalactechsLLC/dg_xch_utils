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

use async_trait::async_trait;
use dg_xch_clients::websocket::{oneshot, oneshot_message};
use dg_xch_core::protocols::full_node::RejectBlocks;
use dg_xch_core::protocols::outbound_limiter::{OutboundDecision, OutboundLimiter};
use dg_xch_p2p::FullNodeApi;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct CountingApi {
    requests: AtomicUsize,
}

#[async_trait]
impl FullNodeApi for CountingApi {
    async fn gossip_peers(&self) -> Vec<dg_xch_core::blockchain::peer_info::TimestampedPeerInfo> {
        Vec::new()
    }
    async fn block_by_height(
        &self,
        _: u32,
    ) -> Option<Box<dg_xch_core::blockchain::full_block::FullBlock>> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        None
    }
}

fn block_request(id: Option<u16>) -> ChiaMessage {
    ChiaMessage::new(
        ProtocolMessageTypes::RequestBlocks,
        ChiaProtocolVersion::default(),
        &RequestBlocks {
            start_height: 0,
            end_height: 0,
            include_transaction_block: false,
        },
        id,
    )
    .unwrap()
}

#[tokio::test]
async fn every_request_path_shares_the_send_budget_and_never_writes_over_budget() {
    let api = Arc::new(CountingApi::default());
    let server = spawn_full_node_rate_limited(api.clone()).await;
    let client = rate_limited_client(server.port).await;
    let connection = client.client.connection.clone();
    let policy = connection.read().await.outbound_policy().unwrap();
    // Five RequestBlocks fit at 1% of the published budget. Pin the window and cap deferrals
    // to test all paths quickly without sending hundreds of requests or sleeping a minute.
    let limiter = Arc::new(OutboundLimiter::with_params(
        3600,
        1,
        1,
        Duration::from_millis(1),
    ));
    connection
        .write()
        .await
        .set_outbound_policy(Some(limiter), policy.capabilities.clone());
    for _ in 0..5 {
        client.client.send(block_request(None)).await.unwrap();
    }
    assert!(
        wait_until(
            || async { api.requests.load(Ordering::Relaxed) == 5 },
            common::network::NETWORK_TIMEOUT
        )
        .await
    );
    let version = ChiaProtocolVersion::default();
    let error = oneshot_message(
        connection.clone(),
        block_request(None),
        None,
        None,
        Some(1000),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    for id in [Some(1), None] {
        let error = oneshot::<RejectBlocks>(
            connection.clone(),
            block_request(id),
            Some(ProtocolMessageTypes::RejectBlocks),
            version,
            id,
            Some(1000),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }
    assert_eq!(
        api.requests.load(Ordering::Relaxed),
        5,
        "no rejected admission reaches the wire"
    );
    connection
        .write()
        .await
        .set_outbound_policy(Some(policy.limiter), policy.capabilities);
    let reply = oneshot_message(connection, block_request(None), None, None, Some(1000))
        .await
        .unwrap();
    assert_eq!(reply.msg_type, ProtocolMessageTypes::RejectBlocks);
    assert_eq!(api.requests.load(Ordering::Relaxed), 6);
    assert!(!client.client.is_closed());
    server.run.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn deferred_sync_request_resumes_without_holding_the_connection_lock() {
    let api = Arc::new(CountingApi::default());
    let server = spawn_full_node_rate_limited(api.clone()).await;
    let client = rate_limited_client(server.port).await;
    let connection = client.client.connection.clone();
    let policy = connection.read().await.outbound_policy().unwrap();
    let limiter = Arc::new(OutboundLimiter::with_params(
        1,
        1,
        100,
        Duration::from_millis(20),
    ));
    let caps = policy.capabilities.read().await.clone();
    for _ in 0..5 {
        assert_eq!(
            limiter.decide(ProtocolMessageTypes::RequestBlocks, 9, &caps),
            OutboundDecision::Send
        );
    }
    connection
        .write()
        .await
        .set_outbound_policy(Some(limiter), policy.capabilities);
    let request_connection = connection.clone();
    let request = tokio::spawn(async move {
        oneshot_message(
            request_connection,
            block_request(None),
            None,
            None,
            Some(1000),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!request.is_finished());
    assert_eq!(api.requests.load(Ordering::Relaxed), 0);
    // Other replies/sends must be able to acquire the write lock while this request is paced.
    drop(
        tokio::time::timeout(Duration::from_millis(250), connection.write())
            .await
            .expect("throttle holds no connection lock"),
    );
    let reply = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply.msg_type, ProtocolMessageTypes::RejectBlocks);
    assert_eq!(api.requests.load(Ordering::Relaxed), 1);
    server.run.store(false, Ordering::Relaxed);
}
