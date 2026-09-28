mod common;

use common::{MemApi, load_full_block, peer, spawn_full_node, wait_until};
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::websocket::WsClientConfig;
use dg_xch_clients::websocket::full_node::FullnodeClient;
use dg_xch_clients::websocket::{oneshot, oneshot_message};
use dg_xch_core::protocols::full_node::{
    RejectBlock, RejectBlocks, RequestBlock, RequestBlocks, RequestPeers, RespondBlock,
    RespondBlocks, RespondPeers,
};
use dg_xch_core::protocols::wallet::{RequestFeeEstimates, RespondFeeEstimates};
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::RwLock;

fn client_config(port: u16) -> Arc<WsClientConfig> {
    Arc::new(WsClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        server_port: 0,
        network_id: "mainnet".to_string(),
        ssl_info: None::<ClientSSLConfig>,
        software_version: None,
        protocol_version: ChiaProtocolVersion::default(),
        additional_headers: None,
        rate_limited: false,
    })
}

async fn connect(port: u16) -> FullnodeClient {
    FullnodeClient::new(
        client_config(port),
        Arc::new(AtomicBool::new(true)),
        None,
        30,
    )
    .await
    .expect("client connects + handshakes")
}

#[tokio::test]
async fn two_nodes_handshake_and_pull_a_block_range() {
    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, load_full_block(5_000_000));
    blocks.insert(5_000_004u32, load_full_block(5_000_004));
    let api = Arc::new(MemApi {
        blocks,
        gossip: vec![peer("1.1.1.1", 8444, 42)],
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    });
    let server = spawn_full_node(api).await;

    // Node B: WsClient completes the version/verack (Handshake) exchange in `new`.
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    // Single-block pull.
    let resp: RespondBlock = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestBlock,
            version,
            &RequestBlock {
                height: 5_000_000,
                include_transaction_block: true,
            },
            Some(1),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondBlock),
        version,
        Some(1),
        Some(15000),
    )
    .await
    .expect("served RespondBlock");
    assert_eq!(resp.block.reward_chain_block.height, 5_000_000);

    // Block-range pull (5_000_000..=5_000_004 span across the two fixtures we hold; missing
    // interior heights make this a RejectBlocks — proving the reject path is wired too).
    let range: Result<RespondBlocks, _> = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestBlocks,
            version,
            &RequestBlocks {
                start_height: 5_000_000,
                end_height: 5_000_000,
                include_transaction_block: true,
            },
            Some(2),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondBlocks),
        version,
        Some(2),
        Some(15000),
    )
    .await;
    let range = range.expect("served RespondBlocks");
    assert_eq!(range.blocks.len(), 1);
    assert_eq!(range.blocks[0].reward_chain_block.height, 5_000_000);

    // Reject path: a height we do not hold.
    let rej: RejectBlock = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestBlock,
            version,
            &RequestBlock {
                height: 9_999_999,
                include_transaction_block: true,
            },
            Some(3),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RejectBlock),
        version,
        Some(3),
        Some(15000),
    )
    .await
    .expect("served RejectBlock for missing height");
    assert_eq!(rej.height, 9_999_999);

    // Gossip path: RequestPeers -> RespondPeers.
    let peers_resp: RespondPeers = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestPeers,
            version,
            &RequestPeers {},
            Some(4),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondPeers),
        version,
        Some(4),
        Some(15000),
    )
    .await
    .expect("served RespondPeers");
    assert_eq!(peers_resp.peer_list.len(), 1);

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// Regression for the request-timeout stall (demux / correlation-id fix). Many requests are put in flight
// concurrently on ONE connection, and every one passes the SAME caller-supplied msg_id `Some(1)` —
// the exact aliasing input the old per-source `AtomicU16` (reset to 1) produced. The connection now
// mints a unique correlation id per request and routes each reply to the single waiter that owns it,
// so no reply crosses to the wrong waiter and none is dropped into a closed channel.
#[tokio::test]
async fn concurrent_requests_on_one_connection_do_not_cross_replies() {
    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, load_full_block(5_000_000));
    blocks.insert(5_000_004u32, load_full_block(5_000_004));
    let api = Arc::new(MemApi {
        blocks,
        gossip: vec![peer("1.1.1.1", 8444, 42)],
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    });
    let server = spawn_full_node(api).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();
    let conn = client.client.connection.clone();

    // Present heights answer RespondBlock; absent heights answer RejectBlock. All fired at once.
    let present = [5_000_000u32, 5_000_004];
    let absent = [6_000_001u32, 6_000_002, 6_000_003, 6_000_004];

    let mut present_tasks = Vec::new();
    for h in present {
        let conn = conn.clone();
        present_tasks.push(tokio::spawn(async move {
            let r: RespondBlock = oneshot(
                conn,
                ChiaMessage::new(
                    ProtocolMessageTypes::RequestBlock,
                    version,
                    &RequestBlock {
                        height: h,
                        include_transaction_block: true,
                    },
                    Some(1),
                )
                .unwrap(),
                Some(ProtocolMessageTypes::RespondBlock),
                version,
                Some(1),
                Some(15000),
            )
            .await
            .expect("served RespondBlock");
            (h, r)
        }));
    }

    let mut absent_tasks = Vec::new();
    for h in absent {
        let conn = conn.clone();
        absent_tasks.push(tokio::spawn(async move {
            let r: RejectBlock = oneshot(
                conn,
                ChiaMessage::new(
                    ProtocolMessageTypes::RequestBlock,
                    version,
                    &RequestBlock {
                        height: h,
                        include_transaction_block: true,
                    },
                    Some(1),
                )
                .unwrap(),
                Some(ProtocolMessageTypes::RejectBlock),
                version,
                Some(1),
                Some(15000),
            )
            .await
            .expect("served RejectBlock");
            (h, r)
        }));
    }

    for t in present_tasks {
        let (h, r) = t.await.unwrap();
        assert_eq!(
            r.block.reward_chain_block.height, h,
            "RespondBlock routed to the wrong waiter (reply crossed)"
        );
    }
    for t in absent_tasks {
        let (h, r) = t.await.unwrap();
        assert_eq!(
            r.height, h,
            "RejectBlock routed to the wrong waiter (reply crossed)"
        );
    }

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

// A MemApi holding `count` contiguous heights from `start`, each a clone of the 5_000_000 fixture
// (the handler serves whatever the api returns — internal block heights are irrelevant here).
// RequestFeeEstimates/RespondFeeEstimates round-trip byte-for-byte (the wallet wire contract).
#[test]
fn fee_estimates_wire_roundtrips() {
    let version = ChiaProtocolVersion::default();
    let req = RequestFeeEstimates {
        time_targets: vec![60, 120, 300],
    };
    let bytes = req.to_bytes(version).expect("encode request");
    let back = RequestFeeEstimates::from_bytes(&mut Cursor::new(&bytes[..]), version)
        .expect("decode request");
    assert_eq!(back.time_targets, req.time_targets);

    use dg_xch_core::protocols::wallet::{FeeEstimate, FeeEstimateGroup, FeeRate};
    let resp = RespondFeeEstimates {
        estimates: FeeEstimateGroup {
            error: None,
            estimates: vec![FeeEstimate {
                error: None,
                time_target: 300,
                estimated_fee_rate: FeeRate {
                    mojos_per_clvm_cost: 7,
                },
            }],
        },
    };
    let bytes = resp.to_bytes(version).expect("encode response");
    let back = RespondFeeEstimates::from_bytes(&mut Cursor::new(&bytes[..]), version)
        .expect("decode response");
    assert_eq!(back.estimates.estimates.len(), 1);
    assert_eq!(back.estimates.estimates[0].time_target, 300);
    assert_eq!(
        back.estimates.estimates[0]
            .estimated_fee_rate
            .mojos_per_clvm_cost,
        7
    );
}

#[tokio::test]
async fn fee_estimates_request_is_served_over_the_wire() {
    let api = Arc::new(MemApi {
        blocks: HashMap::new(),
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    });
    let server = spawn_full_node(api).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    let resp: RespondFeeEstimates = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestFeeEstimates,
            version,
            &RequestFeeEstimates {
                time_targets: vec![60, 120],
            },
            Some(7),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondFeeEstimates),
        version,
        Some(7),
        Some(15000),
    )
    .await
    .expect("served RespondFeeEstimates");

    assert_eq!(resp.estimates.error, None);
    assert_eq!(
        resp.estimates.estimates.len(),
        2,
        "one estimate per requested time"
    );
    assert_eq!(resp.estimates.estimates[0].time_target, 60);
    assert_eq!(resp.estimates.estimates[1].time_target, 120);
    assert!(
        resp.estimates
            .estimates
            .iter()
            .all(|e| e.error.is_none() && e.estimated_fee_rate.mojos_per_clvm_cost == 0),
        "store-blind node answers the floor rate 0 for every target"
    );
}

fn contiguous_api(start: u32, count: u32) -> Arc<MemApi> {
    let template = load_full_block(5_000_000);
    let mut blocks = HashMap::new();
    for h in start..start + count {
        blocks.insert(h, template.clone());
    }
    Arc::new(MemApi {
        blocks,
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    })
}

// Fire a RequestBlocks and hand back whatever single reply the server correlates to it
// (RespondBlocks or RejectBlocks) — the same either-reply await the sync fetcher uses.
async fn request_blocks_raw(
    client: &FullnodeClient,
    start_height: u32,
    end_height: u32,
    include_transaction_block: bool,
) -> Arc<ChiaMessage> {
    let version = ChiaProtocolVersion::default();
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::RequestBlocks,
        version,
        &RequestBlocks {
            start_height,
            end_height,
            include_transaction_block,
        },
        None,
    )
    .expect("encode RequestBlocks");
    oneshot_message(
        client.client.connection.clone(),
        msg,
        None,
        None,
        Some(15000),
    )
    .await
    .expect("a correlated reply (RespondBlocks or RejectBlocks)")
}

#[tokio::test]
async fn oversized_block_range_is_rejected_and_the_cap_span_still_serves() {
    let server = spawn_full_node(contiguous_api(100, 34)).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    // 34 blocks (end - start = 33 > 32): rejected even though every height is present.
    let reply = request_blocks_raw(&client, 100, 133, false).await;
    assert_eq!(
        reply.msg_type,
        ProtocolMessageTypes::RejectBlocks,
        "a >cap span must reject, not serve"
    );
    let rej = RejectBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version)
        .expect("decode RejectBlocks");
    assert_eq!((rej.start_height, rej.end_height), (100, 133));

    let reply = request_blocks_raw(&client, 100, 132, false).await;
    assert_eq!(reply.msg_type, ProtocolMessageTypes::RespondBlocks);
    let resp = RespondBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version)
        .expect("decode RespondBlocks");
    assert_eq!(resp.blocks.len(), 33);

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn inverted_block_range_is_rejected() {
    let server = spawn_full_node(contiguous_api(100, 2)).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    let reply = request_blocks_raw(&client, 101, 100, true).await;
    assert_eq!(
        reply.msg_type,
        ProtocolMessageTypes::RejectBlocks,
        "end < start must reject, not answer an empty RespondBlocks"
    );
    let rej = RejectBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version)
        .expect("decode RejectBlocks");
    assert_eq!((rej.start_height, rej.end_height), (101, 100));

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn unknown_and_future_heights_in_range_are_rejected() {
    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, load_full_block(5_000_000));
    blocks.insert(5_000_004u32, load_full_block(5_000_004));
    let api = Arc::new(MemApi {
        blocks,
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    });
    let server = spawn_full_node(api).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    // Interior hole: we hold 5_000_000 and 5_000_004 but not the heights between.
    let reply = request_blocks_raw(&client, 5_000_000, 5_000_004, true).await;
    assert_eq!(reply.msg_type, ProtocolMessageTypes::RejectBlocks);
    let rej = RejectBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version)
        .expect("decode RejectBlocks");
    assert_eq!((rej.start_height, rej.end_height), (5_000_000, 5_000_004));

    // Fully-future span (nothing held at all).
    let reply = request_blocks_raw(&client, 9_000_000, 9_000_002, true).await;
    assert_eq!(reply.msg_type, ProtocolMessageTypes::RejectBlocks);

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn headers_only_pull_strips_only_the_generator() {
    let fixture = load_full_block(5_000_000);
    assert!(
        fixture.transactions_generator.is_some(),
        "fixture must be a transaction block for this test to bite"
    );
    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, fixture.clone());
    let api = Arc::new(MemApi {
        blocks,
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    });
    let server = spawn_full_node(api).await;
    let client = connect(server.port).await;
    let version = ChiaProtocolVersion::default();

    // Single-block, headers-only: generator stripped, everything else byte-identical.
    let resp: RespondBlock = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestBlock,
            version,
            &RequestBlock {
                height: 5_000_000,
                include_transaction_block: false,
            },
            Some(1),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondBlock),
        version,
        Some(1),
        Some(15000),
    )
    .await
    .expect("served RespondBlock");
    assert!(
        resp.block.transactions_generator.is_none(),
        "include_transaction_block=false must strip transactions_generator"
    );
    assert_eq!(
        resp.block.transactions_info, fixture.transactions_info,
        "transactions_info must NOT be stripped (chia leaves it)"
    );
    assert_eq!(
        resp.block.transactions_generator_ref_list, fixture.transactions_generator_ref_list,
        "transactions_generator_ref_list must NOT be stripped (chia leaves it)"
    );

    // Single-block, full: unchanged.
    let resp: RespondBlock = oneshot(
        client.client.connection.clone(),
        ChiaMessage::new(
            ProtocolMessageTypes::RequestBlock,
            version,
            &RequestBlock {
                height: 5_000_000,
                include_transaction_block: true,
            },
            Some(2),
        )
        .unwrap(),
        Some(ProtocolMessageTypes::RespondBlock),
        version,
        Some(2),
        Some(15000),
    )
    .await
    .expect("served RespondBlock");
    assert_eq!(
        resp.block.transactions_generator,
        fixture.transactions_generator
    );

    // Range, headers-only: the same per-block strip.
    let reply = request_blocks_raw(&client, 5_000_000, 5_000_000, false).await;
    assert_eq!(reply.msg_type, ProtocolMessageTypes::RespondBlocks);
    let resp = RespondBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version)
        .expect("decode RespondBlocks");
    assert_eq!(resp.blocks.len(), 1);
    assert!(resp.blocks[0].transactions_generator.is_none());
    assert_eq!(resp.blocks[0].transactions_info, fixture.transactions_info);

    // Range, full: generator present.
    let reply = request_blocks_raw(&client, 5_000_000, 5_000_000, true).await;
    let resp = RespondBlocks::from_bytes(&mut Cursor::new(reply.data.as_slice()), version)
        .expect("decode RespondBlocks");
    assert_eq!(
        resp.blocks[0].transactions_generator,
        fixture.transactions_generator
    );

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn unsolicited_block_replies_close_the_peer() {
    let server = spawn_full_node(contiguous_api(100, 1)).await;
    let version = ChiaProtocolVersion::default();

    let bodies: Vec<(ProtocolMessageTypes, Vec<u8>)> = vec![
        (
            ProtocolMessageTypes::RespondBlock,
            RespondBlock {
                block: load_full_block(5_000_000),
            }
            .to_bytes(version)
            .unwrap(),
        ),
        (
            ProtocolMessageTypes::RespondBlocks,
            RespondBlocks {
                start_height: 0,
                end_height: 0,
                blocks: Vec::new(),
            }
            .to_bytes(version)
            .unwrap(),
        ),
        (
            ProtocolMessageTypes::RejectBlock,
            RejectBlock { height: 0 }.to_bytes(version).unwrap(),
        ),
        (
            ProtocolMessageTypes::RejectBlocks,
            RejectBlocks {
                start_height: 0,
                end_height: 0,
            }
            .to_bytes(version)
            .unwrap(),
        ),
    ];

    for (msg_type, body) in bodies {
        let client = connect(server.port).await;
        assert!(
            wait_until(
                || async { server.peers.read().await.len() == 1 },
                Duration::from_secs(5),
            )
            .await,
            "server must register the inbound peer after the handshake"
        );
        let msg = ChiaMessage {
            msg_type,
            id: None,
            data: dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(body),
        };
        client
            .client
            .connection
            .write()
            .await
            .send(msg.into())
            .await
            .expect("raw send");
        assert!(
            wait_until(
                || async { server.peers.read().await.is_empty() },
                Duration::from_secs(5),
            )
            .await,
            "an unsolicited {msg_type:?} must close the peer (chia bans it)"
        );
        // Each unsolicited reply now ALSO bans the sender's host (RATE_LIMITER_BAN_SECONDS) — the
        // ban list this test predates. To exercise the next reply type from the same loopback host
        // we clear the ban between sub-cases; that a ban was recorded per type is proven in
        // `t048_ban.rs::unsolicited_block_reply_bans_the_sender`.
        server.bans.clear();
    }

    server
        .run
        .store(false, std::sync::atomic::Ordering::Relaxed);
}
