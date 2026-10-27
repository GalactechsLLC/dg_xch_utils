use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

struct Fixture {
    height: u32,
    fork: u32,
    synced: bool,
    network: String,
    omit_deltas: bool,
}
fn block_hash(height: u32, fork: u32) -> String {
    format!(
        "{:064x}",
        u64::from(height)
            + 1
            + if height >= 2 {
                u64::from(fork) * 10000
            } else {
                0
            }
    )
}
fn record(height: u32, fork: u32) -> Value {
    json!({"height":height,"header_hash":block_hash(height,fork),"prev_hash":if height==0 {"0".repeat(64)} else {block_hash(height-1,fork)},"timestamp":1000+u64::from(height)*20,"fees":100,"farmer_puzzle_hash":format!("0x{}","a".repeat(64)),"pool_puzzle_hash":format!("0x{}","b".repeat(64))})
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn mock(name: &str, state: Arc<Mutex<Fixture>>, calls: Arc<AtomicUsize>) -> (Node, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let state = state.clone();
            let calls = calls.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0u8; 4096];
                let (path, body) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break (
                                headers
                                    .lines()
                                    .next()
                                    .unwrap()
                                    .split_whitespace()
                                    .nth(1)
                                    .unwrap()
                                    .to_string(),
                                serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + length])
                                    .unwrap(),
                            );
                        }
                    }
                };
                calls.fetch_add(1, Ordering::Relaxed);
                let state = state.lock().await;
                let mut response = match path.as_str() {
                    "/get_network_info" => json!({"network_name":state.network}),
                    "/get_blockchain_state" => {
                        json!({"blockchain_state":{"sync":{"synced":state.synced,"sync_mode":!state.synced},"peak":record(state.height,state.fork),"space":1000,"difficulty":42,"sub_slot_iters":10,"mempool_size":5,"mempool_cost":20,"mempool_max_total_cost":1000,"block_max_cost":100,"mempool_min_fees":{"cost_5000000":0}}})
                    }
                    "/get_block_record_by_height" => {
                        json!({"block_record":record(body["height"].as_u64().unwrap() as u32,state.fork)})
                    }
                    "/get_block" => {
                        let height = (0..=state.height)
                            .find(|h| {
                                Some(block_hash(*h, state.fork)) == hash(&body["header_hash"])
                            })
                            .unwrap();
                        json!({"block":{"reward_chain_block":{"height":height,"proof_of_space":{"size":32}},"foliage":{"prev_block_hash":record(height,state.fork)["prev_hash"]},"transactions_info":{"cost":50}}})
                    }
                    "/get_additions_and_removals" if !state.omit_deltas => {
                        json!({"additions":[{"coinbase":true},{"coinbase":false}],"removals":[{},{}]})
                    }
                    _ => json!({"success":false}),
                };
                if response.get("success").is_none() {
                    response["success"] = json!(true);
                }
                let data = serde_json::to_vec(&response).unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(&data).await.unwrap();
            });
        }
    });
    (
        Node {
            counts_cache: Default::default(),
            config: NodeConfig {
                name: name.into(),
                url: format!("http://{address}"),
                ssl: None,
            },
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
        },
        Server(task),
    )
}
async fn reset(store: &Store) {
    sqlx::query("UPDATE dg_discovery.chain_status SET updated_at=0 WHERE network=$1")
        .bind(&store.network)
        .execute(&store.pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL via DGX_TEST_POSTGRES_URL"]
async fn synced_quorum_indexes_once_reconciles_reorgs_and_never_fabricates_missing_rpc_data() {
    let network = format!("telemetry-{}", uuid::Uuid::new_v4());
    let store = Store::connect(
        &std::env::var("DGX_TEST_POSTGRES_URL").unwrap(),
        network.clone(),
    )
    .await
    .unwrap();
    let state = Arc::new(Mutex::new(Fixture {
        height: 3,
        fork: 0,
        synced: false,
        network,
        omit_deltas: false,
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut nodes = Vec::new();
    let mut servers = Vec::new();
    for name in ["one", "two", "three"] {
        let (node, server) = mock(name, state.clone(), calls.clone()).await;
        nodes.push(node);
        servers.push(server);
    }
    let config = TelemetryConfig {
        initial_lookback: 3,
        blocks_per_poll: 4,
        ..Default::default()
    };
    collect(&store, &config, &nodes).await.unwrap();
    let data = dashboard(&store, 60).await.unwrap();
    assert_eq!(data["quorum"], false);
    assert_eq!(data["indexed"]["blocks"], 0);
    state.lock().await.synced = true;
    reset(&store).await;
    collect(&store, &config, &nodes).await.unwrap();
    let data = dashboard(&store, 60).await.unwrap();
    assert_eq!(data["quorum"], true);
    assert!(data["index_error"].is_null(), "{data}");
    assert_eq!(data["indexed"]["blocks"], 3);
    assert_eq!(data["indexed"]["coin_spends"], "6");
    assert!(data["nodes"][0]["fee_estimates_mojos"].is_null());
    let before = calls.load(Ordering::Relaxed);
    collect(&store, &config, &nodes).await.unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        before,
        "replicas must respect shared collection interval"
    );
    {
        let mut s = state.lock().await;
        s.height = 4;
        s.fork = 1;
        s.omit_deltas = true;
    }
    reset(&store).await;
    collect(&store, &config, &nodes).await.unwrap();
    let data = dashboard(&store, 60).await.unwrap();
    assert!(data["index_error"].is_null(), "{data}");
    assert_eq!(data["index"]["reorg_removed_blocks"], 2);
    assert_eq!(data["indexed"]["blocks"], 4);
    assert_eq!(data["indexed"]["blocks_with_unknown_spends"], 3);
    sqlx::query("UPDATE dg_discovery.chain_status SET data=jsonb_set(data,'{observed_at}',to_jsonb(1::bigint)) WHERE network=$1").bind(&store.network).execute(&store.pool).await.unwrap();
    assert_eq!(dashboard(&store, 60).await.unwrap()["quorum"], false);
    // Wrong-network responses can never become chain sources.
    state.lock().await.network = "wrong-network".into();
    reset(&store).await;
    collect(&store, &config, &nodes).await.unwrap();
    assert_eq!(dashboard(&store, 60).await.unwrap()["quorum"], false);
    // SQL identifiers below come only from this fixed table allowlist; network is bound.
    for table in ["chain_status", "chain_blocks", "node_samples"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM dg_discovery.{table} WHERE network=$1"
        )))
        .bind(&store.network)
        .execute(&store.pool)
        .await
        .unwrap();
    }
    drop(servers);
}

#[tokio::test]
async fn ancestor_search_handles_full_reorg_window_with_bounded_rpc_work() {
    let state = Arc::new(Mutex::new(Fixture {
        height: 130,
        fork: 1,
        synced: true,
        network: "ancestor-test".into(),
        omit_deltas: false,
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let (node, _server) = mock("source", state, calls.clone()).await;
    // The source fork diverges at height 2. Exercise shallow, interior and
    // full-window ancestors without a database or a 40-second timeout test.
    for depth in [0, 1, 64, 127, 128] {
        let local: Vec<_> = (1..=depth + 1).map(|h| (h, block_hash(h, 0))).collect();
        calls.store(0, Ordering::Relaxed);
        let ancestor = tokio::time::timeout(Duration::from_secs(5), common_ancestor(&node, &local))
            .await
            .expect("bounded ancestor search must complete promptly")
            .unwrap();
        assert_eq!(ancestor, Some((1, block_hash(1, 0))), "depth {depth}");
        assert!(calls.load(Ordering::Relaxed) <= 10, "depth {depth}");
        if depth == 0 {
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }
    }
    // Also exercise a matching midpoint, rather than only a match at the floor.
    let local: Vec<_> = (1..=129)
        .map(|h| (h, block_hash(h, u32::from(h <= 65))))
        .collect();
    calls.store(0, Ordering::Relaxed);
    assert_eq!(
        common_ancestor(&node, &local).await.unwrap(),
        Some((65, block_hash(65, 1)))
    );
    assert!(calls.load(Ordering::Relaxed) <= 10);
    // A fork older than the retained suffix must leave the local chain intact.
    let local: Vec<_> = (2..=130).map(|h| (h, block_hash(h, 0))).collect();
    calls.store(0, Ordering::Relaxed);
    assert!(common_ancestor(&node, &local).await.unwrap().is_none());
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert!(common_ancestor(&node, &[]).await.unwrap().is_none());
}
