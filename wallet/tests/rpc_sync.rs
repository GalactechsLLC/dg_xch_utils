use blst::min_pk::SecretKey;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::{
    block_record::BlockRecord, blockchain_state::BlockchainState,
    class_group_element::ClassgroupElement, coin::Coin, coin_record::CoinRecord,
    sized_bytes::Bytes32,
};
use dg_xch_core::consensus::constants::SIMULATOR;
use dg_xch_core::protocols::wallet_sync::WalletCoinRecordsRequest;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_wallet::accounts::WalletSession;
use dg_xch_wallet::storage::WalletDatabase;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
enum Capability {
    Version(u32),
    Missing,
    Http(u16),
    Failed,
    Malformed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Peak,
    Height,
    Unordered,
    Duplicate,
    BadCursor,
    ShortPage,
    EmptyCursor,
    EmptyContinuation,
    RepeatedContinuation,
    OverLimit,
    ConflictingSelectors,
    FinalReorg,
    FinalHeight,
    WrongGenesis,
    PageHttp(u16),
    LegacyWindow,
}

struct Fixture {
    capability: Capability,
    fault: Fault,
    records: Vec<CoinRecord>,
    calls: Vec<(String, Value)>,
    state_reads: usize,
    pages: usize,
    page_size: usize,
}

impl Fixture {
    fn response(&mut self, path: &str, body: Value) -> (u16, Value) {
        self.calls.push((path.into(), body.clone()));
        let value = match path {
            "/get_block_record_by_height" => {
                let genesis = if self.fault == Fault::WrongGenesis {
                    [99; 32].into()
                } else {
                    genesis()
                };
                json!({"success":true,"block_record":block_record(genesis,0)})
            }
            "/get_blockchain_state" => {
                self.state_reads += 1;
                let mut state = BlockchainState::default();
                state.sync.synced = true;
                let changed = self.state_reads.is_multiple_of(2);
                state.peak = Some(block_record(
                    if changed && self.fault == Fault::FinalReorg {
                        [98; 32].into()
                    } else {
                        peak()
                    },
                    if changed && self.fault == Fault::FinalHeight {
                        43
                    } else {
                        42
                    },
                ));
                json!({"success":true,"blockchain_state":state})
            }
            "/get_rpc_capabilities" => match self.capability {
                Capability::Version(version) => {
                    json!({"success":true,"endpoints":{"/get_wallet_coin_records":version}})
                }
                Capability::Missing => json!({"success":true,"endpoints":{"/future_endpoint":8}}),
                Capability::Http(status) => {
                    return (
                        status,
                        json!({"success":false,"error":"capability request rejected"}),
                    );
                }
                Capability::Failed => {
                    json!({"success":false,"error":"capability discovery temporarily unavailable"})
                }
                Capability::Malformed => json!({"success":true,"endpoints":"invalid"}),
            },
            "/get_wallet_coin_records" => {
                self.pages += 1;
                if let Fault::PageHttp(status) = self.fault {
                    return (
                        status,
                        json!({"success":false,"error":"wallet discovery unavailable"}),
                    );
                }
                let request: WalletCoinRecordsRequest = serde_json::from_value(body).unwrap();
                assert!(request.validate().is_ok());
                assert_eq!(request.peak_hash, peak());
                let records: BTreeMap<_, _> = self
                    .records
                    .iter()
                    .filter(|record| request.puzzle_hashes.contains(&record.coin.puzzle_hash))
                    .filter(|record| {
                        request
                            .cursor
                            .is_none_or(|cursor| record.coin.name().bytes() > cursor.bytes())
                    })
                    .map(|record| (record.coin.name().bytes(), *record))
                    .collect();
                let size = self.page_size.min(request.limit as usize);
                let more = records.len() > size;
                let mut coins: Vec<_> = records.into_values().take(size).collect();
                let mut next = if more {
                    coins.last().map(|record| record.coin.name())
                } else {
                    None
                };
                match self.fault {
                    Fault::Unordered if coins.len() > 1 => coins.swap(0, 1),
                    Fault::Duplicate if !coins.is_empty() => coins.insert(0, coins[0]),
                    Fault::BadCursor => next = Some([255; 32].into()),
                    Fault::ShortPage => next = coins.last().map(|record| record.coin.name()),
                    Fault::EmptyCursor => {
                        coins.clear();
                        next = Some([255; 32].into());
                    }
                    Fault::EmptyContinuation if request.cursor.is_some() => {
                        coins.clear();
                        next = None;
                    }
                    Fault::RepeatedContinuation if request.cursor.is_some() => {
                        coins = vec![
                            *self
                                .records
                                .iter()
                                .min_by_key(|record| record.coin.name().bytes())
                                .unwrap(),
                        ];
                        next = Some(coins[0].coin.name());
                    }
                    Fault::OverLimit => {
                        coins = vec![self.records[0]; 501];
                        next = None;
                    }
                    Fault::ConflictingSelectors if self.pages == 2 => {
                        let mut record = self.records[0];
                        record.timestamp += 1;
                        coins = vec![record];
                        next = None;
                    }
                    _ => {}
                }
                json!({"success":true,
                    "peak_hash":if self.fault==Fault::Peak{Bytes32::from([97;32])}else{peak()},
                    "height":if self.fault==Fault::Height{41}else{42},
                    "coin_records":coins,"next_cursor":next})
            }
            "/get_coin_records_by_puzzle_hashes" => {
                if self.fault == Fault::LegacyWindow {
                    return (
                        400,
                        json!({"success":false,"error":"start_height and end_height must span at most 2000 blocks"}),
                    );
                }
                let hashes: Vec<Bytes32> =
                    serde_json::from_value(body["puzzle_hashes"].clone()).unwrap();
                let records: Vec<_> = self
                    .records
                    .iter()
                    .filter(|record| hashes.contains(&record.coin.puzzle_hash))
                    .copied()
                    .collect();
                json!({"success":true,"coin_records":records})
            }
            "/get_coin_records_by_hint" => json!({"success":true,"coin_records":[]}),
            _ => {
                return (
                    404,
                    json!({"success":false,"error":format!("unexpected RPC {path}")}),
                );
            }
        };
        (200, value)
    }
}

struct MockRpc {
    client: FullnodeClient,
    fixture: Arc<Mutex<Fixture>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl MockRpc {
    fn new(capability: Capability) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let fixture = Arc::new(Mutex::new(Fixture {
            capability,
            fault: Fault::None,
            records: Vec::new(),
            calls: Vec::new(),
            state_reads: 0,
            pages: 0,
            page_size: 500,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let state = fixture.clone();
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("mock RPC accept: {error}"),
                };
                // Accepted sockets can inherit the listener's nonblocking mode.
                // Request parsing below uses blocking reads with a timeout.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                let mut length = 0;
                let mut header_bytes = line.len();
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    header_bytes += line.len();
                    assert!(header_bytes < 16384);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                assert!(length <= 65536);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                let (status, response) = state
                    .lock()
                    .unwrap()
                    .response(&path, serde_json::from_slice(&body).unwrap());
                let bytes = serde_json::to_vec(&response).unwrap();
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        Self {
            client: FullnodeClient::new_simulator("127.0.0.1", address.port(), 5).unwrap(),
            fixture,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self, path: &str) -> usize {
        self.fixture
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(method, _)| method == path)
            .count()
    }
}

impl Drop for MockRpc {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take()
            && let Err(panic) = worker.join()
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

fn secret() -> SecretKey {
    SecretKey::key_gen_v3(&[71; 32], &[]).unwrap()
}
fn genesis() -> Bytes32 {
    [44; 32].into()
}
fn peak() -> Bytes32 {
    [88; 32].into()
}
fn block_record(header_hash: Bytes32, height: u32) -> BlockRecord {
    BlockRecord {
        header_hash,
        prev_hash: genesis(),
        height,
        weight: u128::from(height) + 1,
        total_iters: 1,
        signage_point_index: 0,
        challenge_vdf_output: ClassgroupElement::get_default_element(),
        infused_challenge_vdf_output: None,
        reward_infusion_new_challenge: genesis(),
        challenge_block_info_hash: genesis(),
        sub_slot_iters: 1,
        pool_puzzle_hash: genesis(),
        farmer_puzzle_hash: genesis(),
        required_iters: 1,
        deficit: 0,
        overflow: false,
        prev_transaction_block_height: 0,
        timestamp: Some(123),
        prev_transaction_block_hash: None,
        fees: None,
        reward_claims_incorporated: None,
        finished_challenge_slot_hashes: None,
        finished_infused_challenge_slot_hashes: None,
        finished_reward_slot_hashes: None,
        sub_epoch_summary_included: None,
    }
}

async fn session(mock: &MockRpc, path: &Path) -> WalletSession {
    WalletSession::new(
        secret(),
        mock.client.clone(),
        Arc::new(SIMULATOR),
        genesis(),
        path.to_owned(),
    )
    .await
    .unwrap()
}
fn coins(hash: Bytes32, count: usize) -> Vec<CoinRecord> {
    (0..count)
        .map(|index| CoinRecord {
            coin: Coin {
                parent_coin_info: genesis(),
                puzzle_hash: hash,
                amount: index as u64 + 1,
            },
            confirmed_block_index: 3,
            spent_block_index: 0,
            coinbase: false,
            timestamp: 123,
            spent: false,
        })
        .collect()
}

#[tokio::test]
async fn negotiated_sync_paginates_and_batches_hashes_and_hints_with_cached_capabilities() {
    let mock = MockRpc::new(Capability::Version(1));
    let directory = tempfile::tempdir().unwrap();
    let mut wallet = session(&mock, &directory.path().join("wallet.sqlite")).await;
    mock.fixture.lock().unwrap().records = coins(wallet.snapshot().receive_puzzle_hash, 601);
    let result = wallet.sync().await.unwrap();
    assert!(result.synced);
    assert_eq!(result.coins.len(), 601);
    assert_eq!(result.confirmed, 601 * 602 / 2);
    assert_eq!(mock.count("/get_wallet_coin_records"), 3); // 500 + 101, then the unused address gap.
    assert_eq!(mock.count("/get_coin_records_by_hint"), 0);
    assert_eq!(mock.count("/get_coin_records_by_puzzle_hashes"), 0);
    {
        let fixture = mock.fixture.lock().unwrap();
        for (_, body) in fixture
            .calls
            .iter()
            .filter(|(path, _)| path == "/get_wallet_coin_records")
        {
            assert_eq!(body["puzzle_hashes"].as_array().unwrap().len(), 40);
            assert_eq!(body["hints"].as_array().unwrap().len(), 40);
            assert_eq!(body["limit"], 500);
        }
    }
    wallet.sync().await.unwrap();
    assert_eq!(mock.count("/get_rpc_capabilities"), 1);
}

#[tokio::test]
async fn watched_asset_selectors_are_batched_within_the_negotiated_limit() {
    let mock = MockRpc::new(Capability::Version(1));
    let directory = tempfile::tempdir().unwrap();
    let mut wallet = session(&mock, &directory.path().join("wallet.sqlite")).await;
    wallet.watch_cat([91; 32].into()).await.unwrap();
    wallet.watch_cat([92; 32].into()).await.unwrap();
    assert!(wallet.sync().await.unwrap().synced);
    let fixture = mock.fixture.lock().unwrap();
    let sizes: Vec<_> = fixture
        .calls
        .iter()
        .filter(|(path, _)| path == "/get_wallet_coin_records")
        .map(|(_, body)| {
            (
                body["puzzle_hashes"].as_array().unwrap().len(),
                body["hints"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(sizes, vec![(40, 40), (100, 0), (60, 0)]);
}

#[tokio::test]
async fn explicitly_unsupported_capabilities_use_legacy_and_cache_the_result() {
    for capability in [
        Capability::Http(404),
        Capability::Http(405),
        Capability::Version(2),
        Capability::Missing,
    ] {
        let mock = MockRpc::new(capability);
        let directory = tempfile::tempdir().unwrap();
        let mut wallet = session(&mock, &directory.path().join("wallet.sqlite")).await;
        assert!(wallet.sync().await.unwrap().synced);
        assert!(wallet.sync().await.unwrap().synced);
        assert_eq!(mock.count("/get_rpc_capabilities"), 1);
        assert_eq!(mock.count("/get_wallet_coin_records"), 0);
        assert_eq!(mock.count("/get_coin_records_by_puzzle_hashes"), 2);
        assert_eq!(mock.count("/get_coin_records_by_hint"), 80);
    }
}

#[tokio::test]
async fn capability_errors_remain_visible_and_are_retried_without_legacy_fallback() {
    for capability in [
        Capability::Http(400),
        Capability::Http(401),
        Capability::Http(403),
        Capability::Http(500),
        Capability::Failed,
        Capability::Malformed,
    ] {
        let mock = MockRpc::new(capability);
        let directory = tempfile::tempdir().unwrap();
        let mut wallet = session(&mock, &directory.path().join("wallet.sqlite")).await;
        assert!(wallet.sync().await.is_err());
        assert!(wallet.sync().await.is_err());
        assert_eq!(mock.count("/get_rpc_capabilities"), 2);
        assert_eq!(mock.count("/get_wallet_coin_records"), 0);
        assert_eq!(mock.count("/get_coin_records_by_puzzle_hashes"), 0);
        assert_eq!(mock.count("/get_coin_records_by_hint"), 0);
        assert!(!wallet.snapshot().synced);
    }
}

#[tokio::test]
async fn invalid_pages_and_reorgs_never_publish_or_persist_partial_wallet_results() {
    for fault in [
        Fault::Peak,
        Fault::Height,
        Fault::Unordered,
        Fault::Duplicate,
        Fault::BadCursor,
        Fault::ShortPage,
        Fault::EmptyCursor,
        Fault::EmptyContinuation,
        Fault::RepeatedContinuation,
        Fault::OverLimit,
        Fault::FinalReorg,
        Fault::FinalHeight,
        Fault::ConflictingSelectors,
        Fault::PageHttp(404),
        Fault::PageHttp(503),
    ] {
        let mock = MockRpc::new(Capability::Version(1));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.sqlite");
        let mut wallet = session(&mock, &path).await;
        let original = coins(wallet.snapshot().receive_puzzle_hash, 1);
        mock.fixture.lock().unwrap().records = original.clone();
        wallet.sync().await.unwrap();
        if fault == Fault::ConflictingSelectors {
            wallet.watch_cat([91; 32].into()).await.unwrap();
        }
        {
            let mut fixture = mock.fixture.lock().unwrap();
            fixture.records = coins(wallet.snapshot().receive_puzzle_hash, 3);
            fixture.fault = fault;
            fixture.state_reads = 0;
            fixture.pages = 0;
            if matches!(
                fault,
                Fault::EmptyContinuation | Fault::RepeatedContinuation
            ) {
                fixture.records = coins(wallet.snapshot().receive_puzzle_hash, 501);
            }
            if fault == Fault::BadCursor {
                fixture.records = coins(wallet.snapshot().receive_puzzle_hash, 500);
            }
        }
        assert!(wallet.sync().await.is_err());
        let snapshot = wallet.snapshot();
        assert!(!snapshot.synced);
        assert_eq!(snapshot.coins, original);
        assert_eq!(snapshot.confirmed, 1);
        assert_eq!(snapshot.height, Some(42));
        assert_eq!(mock.count("/get_coin_records_by_puzzle_hashes"), 0);
        drop(wallet);
        let identity = hex::encode(hash_256(secret().sk_to_pk().to_bytes()));
        let mut database = WalletDatabase::open(&path, &identity, genesis())
            .await
            .unwrap();
        let saved = database.load().await.unwrap().unwrap();
        assert_eq!(saved.snapshot.coins, original);
        assert_eq!(saved.snapshot.confirmed, 1);
    }
}

#[tokio::test]
async fn legacy_gateway_range_error_is_actionable_and_cannot_become_an_empty_balance() {
    let mock = MockRpc::new(Capability::Http(404));
    let directory = tempfile::tempdir().unwrap();
    let mut wallet = session(&mock, &directory.path().join("wallet.sqlite")).await;
    mock.fixture.lock().unwrap().fault = Fault::LegacyWindow;
    let error = wallet.sync().await.err().unwrap().to_string();
    assert!(error.contains("400"));
    assert!(error.contains("get_coin_records_by_puzzle_hashes"));
    assert!(error.contains("2000 blocks"));
    assert!(!wallet.snapshot().synced);
    assert_eq!(wallet.snapshot().height, None);
}

#[tokio::test]
async fn genesis_is_checked_before_capability_negotiation() {
    let mock = MockRpc::new(Capability::Version(1));
    let directory = tempfile::tempdir().unwrap();
    let mut wallet = session(&mock, &directory.path().join("wallet.sqlite")).await;
    mock.fixture.lock().unwrap().fault = Fault::WrongGenesis;
    assert!(
        wallet
            .sync()
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("genesis")
    );
    assert_eq!(mock.count("/get_rpc_capabilities"), 0);
    assert_eq!(mock.count("/get_wallet_coin_records"), 0);
}
