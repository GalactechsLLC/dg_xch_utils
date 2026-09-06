use super::*;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;

const PEAK: u32 = 5_000_000;

fn fixture_block() -> FullBlock {
    serde_json::from_str(include_str!(
        "../../../../tests/fixtures/full_block_5000000.json"
    ))
    .expect("block fixture")
}
fn fixture_peak_record() -> BlockRecord {
    let recs: Vec<BlockRecord> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/block_records.json"
    ))
    .expect("records fixture");
    recs.into_iter()
        .find(|r| r.height == PEAK)
        .expect("peak record present")
}
fn fixture_adds_rems() -> (Vec<CoinRecord>, Vec<CoinRecord>) {
    #[derive(serde::Deserialize)]
    struct AddsRems {
        additions: Vec<CoinRecord>,
        removals: Vec<CoinRecord>,
    }
    let ar: AddsRems = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/adds_rems_5000000.json"
    ))
    .expect("adds_rems fixture");
    (ar.additions, ar.removals)
}

async fn store_at_peak() -> Arc<SqliteStore> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("fn_wallet_{}_{nanos}.sqlite", std::process::id()));
    let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
    let rec = fixture_peak_record();
    let block = fixture_block();
    assert_eq!(rec.header_hash, block.header_hash().expect("hh"));
    let (adds, rems) = fixture_adds_rems();
    // Create the removal coins first, then spend them + create the additions at the peak: the
    // coin store's spent_index/confirmed_index come from the apply_block height, so this makes
    // block 5,000,000's real removals resolvable as spent-at-PEAK and its additions as
    // created-at-PEAK.
    store
        .apply_block(PEAK - 1, 0, &rems, &[])
        .await
        .expect("seed removal coins");
    let rem_names: Vec<Bytes32> = rems.iter().map(|r| r.coin.name()).collect();
    store
        .apply_block(PEAK, rec.timestamp.unwrap_or(0), &adds, &rem_names)
        .await
        .expect("apply peak deltas");
    store
        .add_block_records(std::slice::from_ref(&rec))
        .await
        .expect("records");
    let mut batch = store.begin().await.expect("begin");
    store
        .append_many(&mut batch, std::slice::from_ref(&block))
        .await
        .expect("append body");
    store.commit(batch).await.expect("commit");
    store.set_peak(&rec.header_hash).await.expect("set peak");
    store
}

fn api(store: Arc<SqliteStore>) -> StoreApi<SqliteStore> {
    api_tuned(
        store,
        Arc::new(WalletNotifier::new()),
        MAX_SUBSCRIBE_RESPONSE_ITEMS,
        Arc::new(LimitedSemaphore::new(
            WALLET_SYNC_ACTIVE_LIMIT,
            WALLET_SYNC_WAITING_LIMIT,
        )),
    )
}

// An api with test-scale wallet-serve bounds: an injected subscription registry (small caps), an
// injected initial-state response budget, and an injected wallet-sync semaphore — the production
// numbers (100k budget, 200k subscriptions) are impractical to seed in a unit test. The response
// budget becomes an untrusted-everywhere [`TrustPolicy`] (no peer trusted), so behaviour matches
// the pre-tier default; the trusted-tier tests inject their own policy via [`api_trust`].
fn api_tuned(
    store: Arc<SqliteStore>,
    wallet: Arc<WalletNotifier>,
    max_subscribe_response_items: usize,
    wallet_sync_sem: Arc<LimitedSemaphore>,
) -> StoreApi<SqliteStore> {
    let trust = Arc::new(TrustPolicy::with_caps(
        std::collections::HashSet::new(),
        usize::MAX,
        usize::MAX,
        max_subscribe_response_items,
        max_subscribe_response_items,
    ));
    api_trust(store, wallet, trust, wallet_sync_sem)
}

fn api_trust(
    store: Arc<SqliteStore>,
    wallet: Arc<WalletNotifier>,
    trust: Arc<TrustPolicy>,
    wallet_sync_sem: Arc<LimitedSemaphore>,
) -> StoreApi<SqliteStore> {
    StoreApi {
        store,
        mempool: Arc::new(Mutex::new(Mempool::new(&MAINNET))),
        constants: MAINNET,
        claimed_peak: Arc::new(AtomicU32::new(0)),
        peak_book: Arc::new(PeakBook::new(Arc::new(AtomicU32::new(0)))),
        claim_guard: None,
        new_peak_signal: Arc::new(Notify::new()),
        known_peers: Arc::new(RwLock::new(Vec::new())),
        tx_requested: Arc::new(Mutex::new(HashMap::new())),
        slot_state: Arc::new(Mutex::new(SlotState::new(MAINNET))),
        sp_inbox: Arc::new(Mutex::new(Vec::new())),
        unfinished: Arc::new(Mutex::new(UnfinishedCache::new())),
        ub_inbox: Arc::new(Mutex::new(Vec::new())),
        ip_inbox: Arc::new(Mutex::new(Vec::new())),
        synced: Arc::new(AtomicBool::new(true)),
        wallet_compat: Arc::new(AtomicBool::new(false)),
        tx_inbox: Arc::new(Mutex::new(TxQueue::new(
            TX_INBOX_CAP,
            TX_INBOX_PER_PEER,
            MAINNET.max_block_cost_clvm / 2,
        ))),
        tx_announce: Arc::new(Mutex::new(Vec::new())),
        tx_origin: Arc::new(Mutex::new(HashMap::new())),
        wp_inbox: Arc::new(Mutex::new(Vec::new())),
        compact_vdf_inbox: Arc::new(Mutex::new(Vec::new())),
        proof_candidates: Arc::new(Mutex::new(ProofCandidateStore::default())),
        candidates: Arc::new(Mutex::new(CandidateBlockStore::default())),
        producer: Arc::new(ProducerMetrics::default()),
        farmed_headers: Arc::new(Mutex::new(VecDeque::new())),
        wallet,
        trust,
        wallet_sync_sem,
        record_window: Arc::new(Mutex::new(BlockRecordCache::new(64))),
        sync_metrics: Arc::new(SyncMetrics::default()),
    }
}

#[tokio::test]
async fn puzzle_solution_guards_reject_unknown_unspent_and_wrong_height() {
    let store = store_at_peak().await;
    let (adds, rems) = fixture_adds_rems();
    let api = api(store);

    // Unknown coin: not in the store at all.
    assert!(
        api.puzzle_solution(Bytes32::from([0x13; 32]), PEAK)
            .await
            .is_none()
    );

    // Known but UNSPENT coin (an addition at the peak, spent_index == 0): refused before any
    // generator work.
    let unspent = adds[0].coin.name();
    assert!(api.puzzle_solution(unspent, PEAK).await.is_none());

    // A coin spent at the peak, queried at the WRONG height (spent_block_index != height):
    // refused.
    let spent = rems[0].coin.name();
    assert!(api.puzzle_solution(spent, PEAK - 1).await.is_none());
}

// RequestBlockHeader: the confirmed block at a height serves its HeaderBlock; an unknown height
// rejects.
#[tokio::test]
async fn block_header_serves_and_rejects() {
    let store = store_at_peak().await;
    let api = api(store);
    match api.block_header(PEAK).await {
        BlockHeaderReply::Respond(hb) => assert_eq!(hb.height(), PEAK),
        _ => panic!("the peak block must serve a header"),
    }
    assert!(matches!(
        api.block_header(PEAK + 500).await,
        BlockHeaderReply::Reject(h) if h == PEAK + 500
    ));
}

// G3 closed — the served header carries the block's REAL BIP158 transactions_filter: its
// sha256 is the foliage filter_hash the wallet validates against, byte-equal to the
// validation-side builder over the same delta, identical across all three header-serving
// handlers; return_filter=false serves the encoded-empty b"\x00".
#[tokio::test]
async fn served_header_filter_matches_the_blocks_filter_hash() {
    let store = store_at_peak().await;
    let block = fixture_block();
    let ftb = block.foliage_transaction_block.expect("tx block");
    let (adds, rems) = fixture_adds_rems();
    let api = api(store);

    let hb = match api.block_header(PEAK).await {
        BlockHeaderReply::Respond(hb) => hb,
        _ => panic!("the peak header must serve"),
    };
    let filter = hb.transactions_filter.as_slice().to_vec();
    assert_eq!(
        Bytes32::from(dg_xch_core::utils::hash_256(&filter)),
        ftb.filter_hash,
        "sha256(served filter) must equal the foliage filter_hash"
    );
    // Byte-equality with the proven validation-side construction (engine rule 12): every
    // added coin's puzzle hash (incl. reward claims) then every removed coin's name.
    let mut items: Vec<Vec<u8>> = Vec::new();
    for a in &adds {
        items.push(a.coin.puzzle_hash.bytes().to_vec());
    }
    for r in &rems {
        items.push(r.coin.name().bytes().to_vec());
    }
    assert_eq!(
        filter,
        dg_xch_core::consensus::block_filter::chia_block_filter(&items),
        "served filter bytes equal the rule-12 builder's"
    );

    // The two range handlers serve the same filter bytes.
    match api.header_blocks(PEAK, PEAK).await {
        HeaderBlocksReply::Respond(r) => assert_eq!(
            r.header_blocks[0].transactions_filter.as_slice(),
            filter.as_slice(),
            "request_header_blocks serves the same filter"
        ),
        _ => panic!("header_blocks must serve"),
    }
    match api.block_headers(PEAK, PEAK, true).await {
        BlockHeadersReply::Respond(r) => assert_eq!(
            r.header_blocks[0].transactions_filter.as_slice(),
            filter.as_slice(),
            "request_block_headers(return_filter=true) serves the same filter"
        ),
        _ => panic!("block_headers must serve"),
    }
    // return_filter = false: serve the one-byte encoded-empty filter, NOT the real
    // one and NOT a zero-length string (header_block_from_block).
    match api.block_headers(PEAK, PEAK, false).await {
        BlockHeadersReply::Respond(r) => assert_eq!(
            r.header_blocks[0].transactions_filter.as_slice(),
            &[0u8],
            "return_filter=false serves b\"\\x00\""
        ),
        _ => panic!("block_headers must serve"),
    }
}

// A non-transaction block's served filter is the encoded-empty b"\x00" (PyBIP158([]) —
// the same constant the fast path hardcodes), never the real-filter computation.
#[tokio::test]
async fn non_transaction_block_serves_the_encoded_empty_filter() {
    let store = store_at_peak().await;
    let api = api(store);
    let mut non_tx = fixture_block();
    non_tx.foliage_transaction_block = None;
    non_tx.transactions_info = None;
    let hb = api
        .served_header_block(&non_tx, true)
        .await
        .expect("serves");
    assert_eq!(hb.transactions_filter.as_slice(), &[0u8]);
}

// G2 closed — the specific-puzzle-hashes path serves coins WITH MerkleSet proofs: an
// inclusion proof for a present hash (plus the hash_coin_ids inclusion proof), an
// exclusion proof for an absent one, all verifying against the block's REAL foliage
// additions_root (what the wallet checks them against).
#[tokio::test]
async fn additions_serve_proofs_that_verify_against_the_foliage_root() {
    use dg_xch_core::consensus::merkle_set::validate_merkle_proof;
    let store = store_at_peak().await;
    let additions_root = fixture_block()
        .foliage_transaction_block
        .as_ref()
        .expect("tx block")
        .additions_root;
    let (adds, _rems) = fixture_adds_rems();
    let api = api(store);

    let included_ph = adds[0].coin.puzzle_hash;
    let excluded_ph = Bytes32::from([0x13; 32]);
    let req = RequestAdditions {
        height: PEAK,
        header_hash: None,
        puzzle_hashes: Some(vec![included_ph, excluded_ph]),
    };
    let r = match api.additions(req).await {
        AdditionsReply::Respond(r) => r,
        AdditionsReply::Reject(_) => {
            panic!("a proof-requiring RequestAdditions must serve (G2)")
        }
    };
    let proofs = r.proofs.expect("the proof path carries proofs");
    assert_eq!(proofs.len(), 2, "one proof triple per requested hash");
    let root = additions_root.bytes();

    // Included hash: coins served, both proofs verify as INCLUSION.
    let (ph, proof, coin_proof) = &proofs[0];
    assert_eq!(*ph, included_ph);
    assert_eq!(
        validate_merkle_proof(proof, &ph.bytes(), &root),
        Ok(true),
        "puzzle-hash inclusion proof verifies against the foliage additions_root"
    );
    let served_coins = &r
        .coins
        .iter()
        .find(|(p, _)| *p == included_ph)
        .expect("served entry")
        .1;
    assert!(
        !served_coins.is_empty(),
        "the present hash serves its coins"
    );
    let names: Vec<[u8; 32]> = served_coins.iter().map(|c| c.name().bytes()).collect();
    let coin_ids_hash = hash_coin_ids(&names);
    assert_eq!(
        validate_merkle_proof(
            coin_proof
                .as_ref()
                .expect("inclusion carries the coin-ids proof"),
            &coin_ids_hash,
            &root
        ),
        Ok(true),
        "hash_coin_ids inclusion proof verifies"
    );

    // Excluded hash: empty coins, an EXCLUSION proof, no coin-ids proof.
    let (ph_e, proof_e, coin_proof_e) = &proofs[1];
    assert_eq!(*ph_e, excluded_ph);
    assert!(coin_proof_e.is_none());
    assert_eq!(
        validate_merkle_proof(proof_e, &ph_e.bytes(), &root),
        Ok(false),
        "exclusion proof verifies as NOT-in-set against the additions_root"
    );
    let excluded_entry = &r
        .coins
        .iter()
        .find(|(p, _)| *p == excluded_ph)
        .expect("excluded entry present")
        .1;
    assert!(excluded_entry.is_empty(), "an absent hash serves no coins");
}

// The empty-puzzle-hashes short-circuit answers proofs=Some([]) — [] (an EMPTY
// proofs list), not None; a wallet distinguishes the two on
// the wire.
#[tokio::test]
async fn additions_empty_request_serves_some_empty_proofs() {
    let store = store_at_peak().await;
    let api = api(store);
    let req = RequestAdditions {
        height: PEAK,
        header_hash: None,
        puzzle_hashes: Some(Vec::new()),
    };
    match api.additions(req).await {
        AdditionsReply::Respond(r) => {
            assert!(r.coins.is_empty());
            assert_eq!(
                r.proofs,
                Some(Vec::new()),
                "the empty short-circuit sends proofs=[], not None"
            );
        }
        AdditionsReply::Reject(_) => panic!("the empty request must serve"),
    }
}

// G2 closed — request_removals with specific coin names serves the removal coins with
// MerkleSet proofs verifying against the foliage removals_root; Some-empty behaves like
// None (all removals, proofs=None — ).
#[tokio::test]
async fn removals_serve_proofs_that_verify_against_the_foliage_root() {
    use dg_xch_core::consensus::merkle_set::validate_merkle_proof;
    let store = store_at_peak().await;
    let header_hash = fixture_peak_record().header_hash;
    let removals_root = fixture_block()
        .foliage_transaction_block
        .as_ref()
        .expect("tx block")
        .removals_root;
    let (_adds, rems) = fixture_adds_rems();
    let api = api(store);

    let included_name = rems[0].coin.name();
    let excluded_name = Bytes32::from([0x31; 32]);
    let req = RequestRemovals {
        height: PEAK,
        header_hash,
        coin_names: Some(vec![included_name, excluded_name]),
    };
    let r = match api.removals(req).await {
        RemovalsReply::Respond(r) => r,
        RemovalsReply::Reject(_) => {
            panic!("a proof-requiring RequestRemovals must serve (G2)")
        }
    };
    let proofs = r.proofs.expect("the proof path carries proofs");
    assert_eq!(proofs.len(), 2);
    let root = removals_root.bytes();

    let (name, proof) = &proofs[0];
    assert_eq!(*name, included_name);
    assert_eq!(
        validate_merkle_proof(proof, &name.bytes(), &root),
        Ok(true),
        "removal inclusion proof verifies against the foliage removals_root"
    );
    assert_eq!(
        r.coins[0],
        (included_name, Some(rems[0].coin)),
        "the present name serves its coin"
    );

    let (name_e, proof_e) = &proofs[1];
    assert_eq!(*name_e, excluded_name);
    assert_eq!(
        validate_merkle_proof(proof_e, &name_e.bytes(), &root),
        Ok(false),
        "removal exclusion proof verifies as NOT-in-set"
    );
    assert_eq!(r.coins[1], (excluded_name, None));

    // Some-empty = the trusted all-removals path with proofs None.
    let req_empty = RequestRemovals {
        height: PEAK,
        header_hash,
        coin_names: Some(Vec::new()),
    };
    match api.removals(req_empty).await {
        RemovalsReply::Respond(r) => {
            assert_eq!(r.coins.len(), rems.len(), "Some-empty serves ALL removals");
            assert!(
                r.proofs.is_none(),
                "Some-empty carries proofs=None like None"
            );
        }
        RemovalsReply::Reject(_) => panic!("Some-empty must serve"),
    }
}

#[tokio::test]
async fn served_proofs_match_protocol_vectors() {
    #[derive(serde::Deserialize)]
    struct AddCase {
        puzzle_hash: String,
        included: bool,
        proof: String,
        coin_ids_proof: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct RemCase {
        coin_name: String,
        included: bool,
        proof: String,
    }
    #[derive(serde::Deserialize)]
    struct Fixture {
        additions: Vec<AddCase>,
        removals: Vec<RemCase>,
    }
    fn b32(s: &str) -> Bytes32 {
        Bytes32::from_str(s).expect("hex")
    }
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/merkle_proofs_5000000.json"
    ))
    .expect("proof fixture");

    let store = store_at_peak().await;
    let header_hash = fixture_peak_record().header_hash;
    let api = api(store);

    let req = RequestAdditions {
        height: PEAK,
        header_hash: None,
        puzzle_hashes: Some(
            fixture
                .additions
                .iter()
                .map(|c| b32(&c.puzzle_hash))
                .collect(),
        ),
    };
    let r = match api.additions(req).await {
        AdditionsReply::Respond(r) => r,
        AdditionsReply::Reject(_) => panic!("additions must serve"),
    };
    let proofs = r.proofs.expect("proofs");
    assert_eq!(proofs.len(), fixture.additions.len());
    for (case, (ph, proof, coin_proof)) in fixture.additions.iter().zip(&proofs) {
        assert_eq!(*ph, b32(&case.puzzle_hash));
        assert_eq!(
            hex::encode(proof),
            case.proof,
            "addition proof mismatch for {}",
            case.puzzle_hash
        );
        match (&case.coin_ids_proof, coin_proof) {
            (Some(expected), Some(served)) => assert_eq!(
                hex::encode(served),
                *expected,
                "coin-ids proof mismatch for {}",
                case.puzzle_hash
            ),
            (None, None) => assert!(!case.included),
            _ => panic!("coin-ids proof presence mismatch for {}", case.puzzle_hash),
        }
    }

    let req = RequestRemovals {
        height: PEAK,
        header_hash,
        coin_names: Some(fixture.removals.iter().map(|c| b32(&c.coin_name)).collect()),
    };
    let r = match api.removals(req).await {
        RemovalsReply::Respond(r) => r,
        RemovalsReply::Reject(_) => panic!("removals must serve"),
    };
    let proofs = r.proofs.expect("proofs");
    assert_eq!(proofs.len(), fixture.removals.len());
    for (case, (name, proof)) in fixture.removals.iter().zip(&proofs) {
        assert_eq!(*name, b32(&case.coin_name));
        assert_eq!(
            hex::encode(proof),
            case.proof,
            "removal proof mismatch for {} (included={})",
            case.coin_name,
            case.included
        );
    }
}

// RequestAdditions (trusted, no-proof path): every addition coin comes back grouped by puzzle
// hash; a fork header hash and an oversized puzzle-hash list both reject.
#[tokio::test]
async fn additions_group_by_puzzle_hash_and_reject_forks() {
    let store = store_at_peak().await;
    let header_hash = fixture_peak_record().header_hash;
    let (adds, _rems) = fixture_adds_rems();
    let api = api(store);

    let req = RequestAdditions {
        height: PEAK,
        header_hash: None,
        puzzle_hashes: None,
    };
    match api.additions(req).await {
        AdditionsReply::Respond(r) => {
            assert_eq!(r.header_hash, header_hash);
            let total: usize = r.coins.iter().map(|(_, cs)| cs.len()).sum();
            assert_eq!(total, adds.len(), "every addition coin is served");
        }
        AdditionsReply::Reject(_) => panic!("the peak additions must serve"),
    }

    // A header hash that is not the confirmed block at this height is a fork → reject.
    let forked = RequestAdditions {
        height: PEAK,
        header_hash: Some(Bytes32::from([0x99; 32])),
        puzzle_hashes: None,
    };
    assert!(matches!(
        api.additions(forked).await,
        AdditionsReply::Reject(_)
    ));

    // Too many puzzle hashes → reject before any DB work.
    let oversized = RequestAdditions {
        height: PEAK,
        header_hash: None,
        puzzle_hashes: Some(vec![Bytes32::default(); MAX_COIN_HASHES_PER_REQUEST + 1]),
    };
    assert!(matches!(
        api.additions(oversized).await,
        AdditionsReply::Reject(_)
    ));
}

// RequestRemovals (trusted, no-proof path): every removed coin comes back; an unknown block
// rejects.
#[tokio::test]
async fn removals_serve_all_and_reject_unknown_block() {
    let store = store_at_peak().await;
    let header_hash = fixture_peak_record().header_hash;
    let (_adds, rems) = fixture_adds_rems();
    let api = api(store);

    let req = RequestRemovals {
        height: PEAK,
        header_hash,
        coin_names: None,
    };
    match api.removals(req).await {
        RemovalsReply::Respond(r) => {
            assert_eq!(r.coins.len(), rems.len(), "every removed coin is served");
            assert!(r.coins.iter().all(|(_, c)| c.is_some()));
        }
        RemovalsReply::Reject(_) => panic!("the peak removals must serve"),
    }

    let unknown = RequestRemovals {
        height: PEAK,
        header_hash: Bytes32::from([0x77; 32]),
        coin_names: None,
    };
    assert!(matches!(
        api.removals(unknown).await,
        RemovalsReply::Reject(_)
    ));
}

// RequestChildren: the coin states of a coin's children (spent + unspent), read from the parent
// index.
#[tokio::test]
async fn children_returns_coin_states_by_parent() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "fn_wallet_kids_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
    let parent = Bytes32::from([0x0C; 32]);
    let child = Coin {
        parent_coin_info: parent,
        puzzle_hash: Bytes32::from([0x0D; 32]),
        amount: 42,
    };
    let record = CoinRecord {
        coin: child,
        confirmed_block_index: 10,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    };
    store
        .apply_block(10, 0, &[record], &[])
        .await
        .expect("seed");
    let api = api(store);

    let states = api.children(parent).await;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].coin.name(), child.name());
    assert_eq!(states[0].created_height, Some(10));
    assert_eq!(states[0].spent_height, None);

    // A coin with no children yields an empty (still valid) response.
    assert!(api.children(Bytes32::from([0xEE; 32])).await.is_empty());
}

// RegisterForPhUpdates returns the initial matching CoinState set — and it is FULL history:
// both an unspent addition and a SPENT removal at the peak come back (unspent-only would be a
// broken wallet backend). The first registration also yields the delivery receiver.
#[tokio::test]
async fn register_ph_returns_spent_and_unspent_initial_state() {
    let store = store_at_peak().await;
    let (adds, rems) = fixture_adds_rems();
    let api = api(store);

    let unspent_ph = adds[0].coin.puzzle_hash;
    let reg = api
        .register_for_ph_updates(
            Bytes32::from([0xA1; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![unspent_ph],
                min_height: 0,
            },
        )
        .await;
    assert!(
        reg.receiver.is_some(),
        "the first registration hands back the delivery receiver"
    );
    assert_eq!(reg.response.puzzle_hashes, vec![unspent_ph]);
    assert!(
        reg.response
            .coin_states
            .iter()
            .any(|cs| cs.coin.puzzle_hash == unspent_ph
                && cs.created_height == Some(PEAK)
                && cs.spent_height.is_none()),
        "an unspent addition with the subscribed ph is in the initial state"
    );

    // A puzzle hash that only a SPENT (removal) coin carries must still come back, with its
    // spent height set — the full-history guarantee.
    let spent_ph = rems[0].coin.puzzle_hash;
    let reg2 = api
        .register_for_ph_updates(
            Bytes32::from([0xA2; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![spent_ph],
                min_height: 0,
            },
        )
        .await;
    assert!(
        reg2.response
            .coin_states
            .iter()
            .any(|cs| cs.spent_height == Some(PEAK)),
        "a spent coin is included in the initial state (spent + unspent history)"
    );
}

// A CoinStateUpdate reaches a subscribed peer's delivery channel when a new peak creates a coin
// with its puzzle hash - the peak-delta push the server's confirm path drives, exercised through
// the same WalletNotifier the register handler subscribed against.
#[tokio::test]
async fn coin_state_update_delivered_across_a_peak_advance() {
    let store = store_at_peak().await;
    let (adds, _rems) = fixture_adds_rems();
    let api = api(store);

    let ph = adds[0].coin.puzzle_hash;
    let mut rx = api
        .register_for_ph_updates(
            Bytes32::from([0xB1; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![ph],
                min_height: 0,
            },
        )
        .await
        .receiver
        .expect("receiver");

    // A new block at PEAK+1 creates a coin with the subscribed puzzle hash.
    let new_coin = Coin {
        parent_coin_info: Bytes32::from([0xEE; 32]),
        puzzle_hash: ph,
        amount: 7,
    };
    let record = CoinRecord {
        coin: new_coin,
        confirmed_block_index: PEAK + 1,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    };
    api.wallet
        .on_new_peak(
            api.store.as_ref(),
            crate::wallet::WalletUpdate {
                peak_hash: Bytes32::from([0xF1; 32]),
                height: PEAK + 1,
                fork_height: PEAK,
                created: &[record],
                spent_ids: &[],
                hints: &[],
            },
        )
        .await
        .expect("push");

    let update = rx.recv().await.expect("a CoinStateUpdate is delivered");
    assert_eq!(update.height, PEAK + 1);
    assert!(
        update
            .items
            .iter()
            .any(|cs| cs.coin.name() == new_coin.name() && cs.created_height == Some(PEAK + 1)),
        "the created coin is in the update"
    );
}

// Disconnect hygiene: reconciling the registry against the live inbound peer set drops the gone
// peer's subscription, and dropping its subscriber closes its delivery channel — so the per-peer
// forwarder task ends (no leak).
#[tokio::test]
async fn disconnect_reconciliation_drops_the_subscription() {
    let store = store_at_peak().await;
    let api = api(store);

    let peer_live = Bytes32::from([0xC1; 32]);
    let peer_gone = Bytes32::from([0xC2; 32]);
    let _rx_live = api
        .register_for_ph_updates(
            peer_live,
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![Bytes32::from([0x51; 32])],
                min_height: 0,
            },
        )
        .await
        .receiver
        .expect("live rx");
    let mut rx_gone = api
        .register_for_coin_updates(
            peer_gone,
            None,
            RegisterForCoinUpdates {
                coin_ids: vec![Bytes32::from([0x52; 32])],
                min_height: 0,
            },
        )
        .await
        .receiver
        .expect("gone rx");
    assert_eq!(api.wallet.subscriber_count().await, 2);

    let live: std::collections::HashSet<Bytes32> = std::iter::once(peer_live).collect();
    api.wallet.retain_live(&live).await;

    assert_eq!(api.wallet.subscriber_count().await, 1);
    assert!(
        rx_gone.recv().await.is_none(),
        "the disconnected peer's delivery channel closes, ending its forwarder"
    );
}

// ---- Wallet-serve bounds ----------------------------------------------

// The height the synthetic subscription coins are seeded at (any tx-block height works: the
// register read is a pure coin-store query, blind to the block store).
const SEED_HEIGHT: u32 = 100;

// A synthetic unspent coin on `ph`, keyed by `tag` so coin names are distinct.
fn synth_record(tag: u8, ph: Bytes32, height: u32) -> CoinRecord {
    CoinRecord {
        coin: Coin {
            parent_coin_info: Bytes32::from([tag; 32]),
            puzzle_hash: ph,
            amount: u64::from(tag) + 1,
        },
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    }
}

async fn store_with(records: &[CoinRecord]) -> Arc<SqliteStore> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "fn_walletcap_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
    store
        .apply_block(SEED_HEIGHT, 0, records, &[])
        .await
        .expect("seed coins");
    store
}

fn default_sem() -> Arc<LimitedSemaphore> {
    Arc::new(LimitedSemaphore::new(
        WALLET_SYNC_ACTIVE_LIMIT,
        WALLET_SYNC_WAITING_LIMIT,
    ))
}

// The RegisterForPhUpdates initial-state read is bounded by
// `max_subscribe_response_items` (the store query takes the budget as max_items).
// Truncation is SILENT: logged, and the reply still echoes the REQUESTED puzzle hashes.
#[tokio::test]
async fn ph_initial_state_is_bounded_by_the_response_budget() {
    let ph = Bytes32::from([0x42; 32]);
    let records: Vec<CoinRecord> = (1..=8).map(|t| synth_record(t, ph, SEED_HEIGHT)).collect();
    let store = store_with(&records).await;
    let api = api_tuned(store, Arc::new(WalletNotifier::new()), 5, default_sem());

    let reg = api
        .register_for_ph_updates(
            Bytes32::from([0xD1; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![ph],
                min_height: 0,
            },
        )
        .await;
    assert_eq!(
        reg.response.puzzle_hashes,
        vec![ph],
        "the reply echoes the REQUESTED hashes even when truncating"
    );
    assert_eq!(
        reg.response.coin_states.len(),
        5,
        "the initial-state set is silently truncated to max_subscribe_response_items"
    );
}

// Hint leg: ONE budget is decremented across the puzzle-hash query and then the hint
// query (`max_items -= len(states)` before the hint-id lookup).
#[cfg(feature = "hint")]
#[tokio::test]
async fn ph_response_budget_is_shared_with_the_hint_query() {
    let ph = Bytes32::from([0x43; 32]);
    let mut records: Vec<CoinRecord> = (1..=3).map(|t| synth_record(t, ph, SEED_HEIGHT)).collect();
    // Four coins on OTHER puzzle hashes, each HINTED by the subscribed 32-byte value
    // (the subscribed hashes double as hint keys).
    let hinted: Vec<CoinRecord> = (10..=13)
        .map(|t| synth_record(t, Bytes32::from([t; 32]), SEED_HEIGHT))
        .collect();
    records.extend(hinted.iter().cloned());
    let store = store_with(&records).await;
    let pairs: Vec<(Bytes32, Bytes32)> = hinted.iter().map(|r| (ph, r.coin.name())).collect();
    store.apply_hints(&pairs).await.expect("hints");

    // Budget 5: the ph query consumes 3, leaving 2 for the hint side → exactly 5 states.
    let api = api_tuned(
        store.clone(),
        Arc::new(WalletNotifier::new()),
        5,
        default_sem(),
    );
    let reg = api
        .register_for_ph_updates(
            Bytes32::from([0xD2; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![ph],
                min_height: 0,
            },
        )
        .await;
    assert_eq!(
        reg.response.coin_states.len(),
        5,
        "ph states (3) + hint states capped to the remaining budget (2)"
    );

    // Budget 3: fully consumed by the ph query — the hint query gets nothing.
    let api = api_tuned(store, Arc::new(WalletNotifier::new()), 3, default_sem());
    let reg = api
        .register_for_ph_updates(
            Bytes32::from([0xD3; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![ph],
                min_height: 0,
            },
        )
        .await;
    assert_eq!(
        reg.response.coin_states.len(),
        3,
        "an exhausted budget starves the hint query entirely"
    );
}

// Dedup half: ONLY add_puzzle_subscriptions' return — the newly-subscribed set —
// feeds the initial-state query, so re-registering an
// already-subscribed hash yields NO initial states (and no repeated heavy scan).
#[tokio::test]
async fn repeat_ph_registration_yields_no_initial_state() {
    let ph = Bytes32::from([0x44; 32]);
    let records: Vec<CoinRecord> = (1..=2).map(|t| synth_record(t, ph, SEED_HEIGHT)).collect();
    let store = store_with(&records).await;
    let api = api(store);
    let peer = Bytes32::from([0xD4; 32]);
    let req = || RegisterForPhUpdates {
        puzzle_hashes: vec![ph],
        min_height: 0,
    };

    let reg = api.register_for_ph_updates(peer, None, req()).await;
    assert_eq!(reg.response.coin_states.len(), 2);

    let reg2 = api.register_for_ph_updates(peer, None, req()).await;
    assert!(
        reg2.response.coin_states.is_empty(),
        "an already-subscribed hash is filtered from the initial-state query"
    );
}

// Overflow half: a hash dropped by the per-peer subscription cap is NOT part of
// add_puzzle_subscriptions' return, so it is never queried — the initial-state read cannot be
// driven past the subscription cap with hashes that were never subscribed.
#[tokio::test]
async fn overflow_dropped_puzzle_hashes_are_not_queried() {
    let (ph_a, ph_b, ph_c) = (
        Bytes32::from([0x51; 32]),
        Bytes32::from([0x52; 32]),
        Bytes32::from([0x53; 32]),
    );
    // Coins exist ONLY on the third hash — the one the cap (2) drops.
    let records: Vec<CoinRecord> = (1..=3)
        .map(|t| synth_record(t, ph_c, SEED_HEIGHT))
        .collect();
    let store = store_with(&records).await;
    let api = api_tuned(
        store,
        Arc::new(WalletNotifier::with_limits(8, 2)),
        MAX_SUBSCRIBE_RESPONSE_ITEMS,
        default_sem(),
    );

    let reg = api
        .register_for_ph_updates(
            Bytes32::from([0xD5; 32]),
            None,
            RegisterForPhUpdates {
                puzzle_hashes: vec![ph_a, ph_b, ph_c],
                min_height: 0,
            },
        )
        .await;
    assert_eq!(
        reg.response.puzzle_hashes,
        vec![ph_a, ph_b, ph_c],
        "the reply echoes the full requested list"
    );
    assert!(
        reg.response.coin_states.is_empty(),
        "the overflow-dropped hash must not feed the initial-state query"
    );
}

// Coin leg: the REQUEST list truncates to max_subscriptions; the SLICED list is
// subscribed, queried, and echoed back (the coin path deliberately keeps in-request
// duplicates queryable).
#[tokio::test]
async fn coin_registration_slices_the_request_to_the_subscription_cap() {
    let records: Vec<CoinRecord> = (1..=3)
        .map(|t| synth_record(t, Bytes32::from([0x60 + t; 32]), SEED_HEIGHT))
        .collect();
    let ids: Vec<Bytes32> = records.iter().map(|r| r.coin.name()).collect();
    let store = store_with(&records).await;
    let api = api_tuned(
        store,
        Arc::new(WalletNotifier::with_limits(8, 2)),
        MAX_SUBSCRIBE_RESPONSE_ITEMS,
        default_sem(),
    );

    let reg = api
        .register_for_coin_updates(
            Bytes32::from([0xD6; 32]),
            None,
            RegisterForCoinUpdates {
                coin_ids: ids.clone(),
                min_height: 0,
            },
        )
        .await;
    assert_eq!(
        reg.response.coin_ids,
        ids[..2].to_vec(),
        "the coin response echoes the SLICED list (unlike the ph response)"
    );
    assert_eq!(
        reg.response.coin_states.len(),
        2,
        "only the sliced ids are queried"
    );
}

// Coin leg: the RegisterForCoinUpdates initial read is bounded by the same response
// budget (get_coin_states_by_ids(max_items=max_items)).
#[tokio::test]
async fn coin_initial_state_is_bounded_by_the_response_budget() {
    let records: Vec<CoinRecord> = (1..=4)
        .map(|t| synth_record(t, Bytes32::from([0x70 + t; 32]), SEED_HEIGHT))
        .collect();
    let ids: Vec<Bytes32> = records.iter().map(|r| r.coin.name()).collect();
    let store = store_with(&records).await;
    let api = api_tuned(store, Arc::new(WalletNotifier::new()), 2, default_sem());

    let reg = api
        .register_for_coin_updates(
            Bytes32::from([0xD7; 32]),
            None,
            RegisterForCoinUpdates {
                coin_ids: ids.clone(),
                min_height: 0,
            },
        )
        .await;
    assert_eq!(
        reg.response.coin_ids, ids,
        "under the subscription cap the echo is the full request"
    );
    assert_eq!(
        reg.response.coin_states.len(),
        2,
        "the initial-state set is truncated to the response budget"
    );
}

// additions/removals are guarded by the wallet-sync LimitedSemaphore — overflow REJECTS
// (RejectAdditionsRequest / RejectRemovalsRequest) instead of queueing unbounded
// concurrent block-delta scans.
#[tokio::test]
async fn wallet_serve_rejects_when_the_sync_semaphore_is_full() {
    let store = store_at_peak().await;
    let peak_hash = fixture_peak_record().header_hash;

    // active=0, waiting=0: every acquire overflows immediately.
    let api_full = api_tuned(
        store.clone(),
        Arc::new(WalletNotifier::new()),
        MAX_SUBSCRIBE_RESPONSE_ITEMS,
        Arc::new(LimitedSemaphore::new(0, 0)),
    );
    let additions_req = || RequestAdditions {
        height: PEAK,
        header_hash: None,
        puzzle_hashes: None,
    };
    let removals_req = || RequestRemovals {
        height: PEAK,
        header_hash: peak_hash,
        coin_names: None,
    };
    assert!(
        matches!(
            api_full.additions(additions_req()).await,
            AdditionsReply::Reject(_)
        ),
        "additions must reject on wallet-sync semaphore overflow"
    );
    assert!(
        matches!(
            api_full.removals(removals_req()).await,
            RemovalsReply::Reject(_)
        ),
        "removals must reject on wallet-sync semaphore overflow"
    );

    // Within bounds, the same requests serve.
    let api_ok = api_tuned(
        store,
        Arc::new(WalletNotifier::new()),
        MAX_SUBSCRIBE_RESPONSE_ITEMS,
        default_sem(),
    );
    assert!(matches!(
        api_ok.additions(additions_req()).await,
        AdditionsReply::Respond(_)
    ));
    assert!(matches!(
        api_ok.removals(removals_req()).await,
        RemovalsReply::Respond(_)
    ));
}

// ---- request_puzzle_state / request_coin_state at the api seam, where the response
// budget and subscription caps are
// injectable — the production 100k/200k numbers are impractical to seed. The wire-level
// contract (dispatch, rejects, the Sage sequence) is proven in tests/puzzle_state.rs.

// A minimal MAIN-CHAIN block record at a height: header_hash [height;32] linked by
// prev_hash, so add_block_records + set_peak(tip) marks the whole ancestry in-main-chain
// and height_to_hash resolves every page boundary.
fn chain_rec(height: u32) -> BlockRecord {
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    BlockRecord {
        header_hash: Bytes32::from([height as u8; 32]),
        prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
        height,
        weight: u128::from(height) * 100,
        total_iters: u128::from(height),
        signage_point_index: 0,
        challenge_vdf_output: ClassgroupElement::get_default_element(),
        infused_challenge_vdf_output: None,
        reward_infusion_new_challenge: Bytes32::default(),
        challenge_block_info_hash: Bytes32::default(),
        sub_slot_iters: MAINNET.sub_slot_iters_starting,
        pool_puzzle_hash: Bytes32::default(),
        farmer_puzzle_hash: Bytes32::default(),
        required_iters: 1,
        deficit: 0,
        overflow: false,
        prev_transaction_block_height: 0,
        timestamp: Some(1_700_000_000),
        prev_transaction_block_hash: None,
        fees: None,
        reward_claims_incorporated: None,
        finished_challenge_slot_hashes: None,
        finished_infused_challenge_slot_hashes: None,
        finished_reward_slot_hashes: None,
        sub_epoch_summary_included: None,
    }
}

// A store carrying a synthetic 0..=tip main chain plus `per_height` coins on `ph` at each
// of `coin_heights` — the paging scenario RequestPuzzleState's height/header_hash contract
// needs (every page boundary must resolve on the main chain for the NEXT request's
// reorg-consistency check to pass).
async fn paging_store(
    ph: Bytes32,
    coin_heights: &[u32],
    per_height: usize,
    tip: u32,
) -> Arc<SqliteStore> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("fn_puzstate_{}_{nanos}.sqlite", std::process::id()));
    let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
    let records: Vec<BlockRecord> = (0..=tip).map(chain_rec).collect();
    store.add_block_records(&records).await.expect("records");
    store
        .set_peak(&chain_rec(tip).header_hash)
        .await
        .expect("peak");
    let mut tag = 0u8;
    for h in coin_heights {
        let recs: Vec<CoinRecord> = (0..per_height)
            .map(|_| {
                tag += 1;
                CoinRecord {
                    coin: Coin {
                        parent_coin_info: Bytes32::from([tag; 32]),
                        puzzle_hash: ph,
                        amount: 1_000,
                    },
                    confirmed_block_index: *h,
                    spent_block_index: 0,
                    coinbase: false,
                    timestamp: 0,
                    spent: false,
                }
            })
            .collect();
        store.apply_block(*h, 0, &recs, &[]).await.expect("coins");
    }
    store
}

fn all_filters() -> dg_xch_core::protocols::wallet::CoinStateFilters {
    dg_xch_core::protocols::wallet::CoinStateFilters {
        include_spent: true,
        include_unspent: true,
        include_hinted: true,
        min_amount: 0,
    }
}

// Sage's sync_puzzle_hashes loop (wallet_sync.rs:169-206) against an injected 4-item
// budget: each page's (height, header_hash) feeds the NEXT request's reorg-consistency
// check (which must PASS against our chain), no page splits a height, heights are
// ordered, and the union over pages is exactly the seeded set. This is the paging
// contract end-to-end at the api seam.
#[tokio::test]
async fn puzzle_state_pages_thread_the_sage_loop_to_convergence() {
    let ph = Bytes32::from([0x5A; 32]);
    // 4 heights x 3 coins, budget 4: boundaries fall inside heights, forcing the
    // whole-height trim to shrink pages.
    let store = paging_store(ph, &[10, 11, 12, 13], 3, 20).await;
    let api = api_tuned(store, Arc::new(WalletNotifier::new()), 4, default_sem());
    let peer = Bytes32::from([0xB1; 32]);

    let mut previous_height: Option<u32> = None;
    let mut header_hash = MAINNET.genesis_challenge;
    let mut names = HashSet::new();
    let mut pages = 0;
    loop {
        let reply = api
            .puzzle_state(
                peer,
                None,
                RequestPuzzleState {
                    puzzle_hashes: vec![ph],
                    previous_height,
                    header_hash,
                    filters: all_filters(),
                    subscribe_when_finished: true,
                },
            )
            .await;
        let PuzzleStateReply::Respond(resp, _rx) = reply else {
            panic!("page {pages} must serve, not reject");
        };
        pages += 1;
        assert!(resp.coin_states.len() <= 4, "no page exceeds the budget");
        let heights: Vec<u32> = resp
            .coin_states
            .iter()
            .map(|cs| cs.created_height.unwrap_or(0))
            .collect();
        let mut sorted = heights.clone();
        sorted.sort_unstable();
        assert_eq!(heights, sorted, "page is height-ordered");
        if let Some(max_h) = heights.last() {
            assert!(
                resp.is_finished || *max_h <= resp.height,
                "no state beyond the page's reported height"
            );
        }
        for cs in &resp.coin_states {
            assert!(names.insert(cs.coin.name()), "no duplicates across pages");
        }
        // The page's header_hash IS our main chain at the page height — that is what
        // makes the next request's reorg check pass.
        assert_eq!(resp.header_hash, chain_rec(resp.height).header_hash);
        if resp.is_finished {
            assert_eq!(resp.height, 20, "the final page reports the peak");
            break;
        }
        previous_height = Some(resp.height);
        header_hash = resp.header_hash;
        assert!(pages < 20, "the page loop must terminate");
    }
    assert!(pages > 1, "the scenario must actually page");
    assert_eq!(names.len(), 12, "the loop converges to the seeded set");
}

// The subscribe side effect and its caps: subscribe_when_finished registers against the
// SAME per-peer cap the
// register handlers use — an over-cap request rejects EXCEEDED_SUBSCRIPTION_LIMIT (the
// exact reject Sage maps to SubscriptionLimitReached), cumulative across requests; a
// non-subscribing request of the same size still serves.
#[tokio::test]
async fn puzzle_and_coin_state_subscribe_against_the_shared_cap() {
    let ph = Bytes32::from([0x5B; 32]);
    let store = paging_store(ph, &[10], 1, 12).await;
    // cap: 4 combined items per peer.
    let wallet = Arc::new(WalletNotifier::with_limits(8, 4));
    let api = api_tuned(
        store,
        wallet.clone(),
        MAX_SUBSCRIBE_RESPONSE_ITEMS,
        default_sem(),
    );
    let peer = Bytes32::from([0xB2; 32]);
    let phs = |tags: std::ops::Range<u8>| -> Vec<Bytes32> {
        tags.map(|t| Bytes32::from([t; 32])).collect()
    };
    let req = |puzzle_hashes: Vec<Bytes32>, subscribe: bool| RequestPuzzleState {
        puzzle_hashes,
        previous_height: None,
        header_hash: MAINNET.genesis_challenge,
        filters: all_filters(),
        subscribe_when_finished: subscribe,
    };

    // Over the cap in one request → EXCEEDED, and NOTHING was subscribed.
    let reply = api.puzzle_state(peer, None, req(phs(1..6), true)).await;
    assert!(
        matches!(
            reply,
            PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit)
        ),
        "5 subscriptions against a cap of 4 must reject EXCEEDED"
    );
    assert_eq!(wallet.peer_subscription_count(&peer).await, 0);

    // The same 5 hashes WITHOUT the subscribe flag serve fine (the cap gates only the
    // side effect, `request.subscribe_when_finished and ...`).
    assert!(matches!(
        api.puzzle_state(peer, None, req(phs(1..6), false)).await,
        PuzzleStateReply::Respond(..)
    ));
    assert_eq!(wallet.peer_subscription_count(&peer).await, 0);

    // 3 subscribe, then 2 more blow the cap CUMULATIVELY (3 + 2 > 4)…
    let PuzzleStateReply::Respond(_, rx) = api.puzzle_state(peer, None, req(phs(1..4), true)).await
    else {
        panic!("3 subscriptions fit the cap");
    };
    assert!(
        rx.is_some(),
        "first registration yields the delivery receiver"
    );
    assert_eq!(wallet.peer_subscription_count(&peer).await, 3);
    assert!(matches!(
        api.puzzle_state(peer, None, req(phs(4..6), true)).await,
        PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit)
    ));

    // …and the coin leg counts against the SAME combined cap
    // (peer_subscription_count sums both sets): 3 ph + 2 coin ids > 4 → EXCEEDED; 1 fits.
    let coin_req = |coin_ids: Vec<Bytes32>, subscribe: bool| RequestCoinState {
        coin_ids,
        previous_height: None,
        header_hash: MAINNET.genesis_challenge,
        subscribe,
    };
    assert!(matches!(
        api.coin_state(peer, None, coin_req(phs(0x10..0x12), true))
            .await,
        CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit)
    ));
    let CoinStateReply::Respond(_, rx) = api
        .coin_state(peer, None, coin_req(phs(0x10..0x11), true))
        .await
    else {
        panic!("1 more subscription fits the cap exactly");
    };
    assert!(rx.is_none(), "one delivery channel per peer");
    assert_eq!(wallet.peer_subscription_count(&peer).await, 4);

    // remove-all returns each leg's subscribed set and frees the cap.
    let removed_ph = api.remove_puzzle_subscriptions(peer, None).await;
    assert_eq!(removed_ph.len(), 3);
    let removed_coins = api.remove_coin_subscriptions(peer, None).await;
    assert_eq!(removed_coins, vec![Bytes32::from([0x10; 32])]);
    assert_eq!(wallet.peer_subscription_count(&peer).await, 0);
    assert!(matches!(
        api.puzzle_state(peer, None, req(phs(1..5), true)).await,
        PuzzleStateReply::Respond(..)
    ));
}

// request_coin_state truncates the id list to the response budget BEFORE serving and
// echoes the truncated, deduped list (via the list_limits parse cap +
// dict.fromkeys) — with the budget injected small enough to see it.
#[tokio::test]
async fn coin_state_truncates_the_id_list_to_the_response_budget() {
    let ph = Bytes32::from([0x5C; 32]);
    let store = paging_store(ph, &[10], 3, 12).await;
    let api = api_tuned(store, Arc::new(WalletNotifier::new()), 2, default_sem());
    let peer = Bytes32::from([0xB3; 32]);
    let ids: Vec<Bytes32> = (1u8..=4).map(|t| Bytes32::from([t; 32])).collect();
    let CoinStateReply::Respond(resp, _) = api
        .coin_state(
            peer,
            None,
            RequestCoinState {
                coin_ids: ids.clone(),
                previous_height: None,
                header_hash: MAINNET.genesis_challenge,
                subscribe: false,
            },
        )
        .await
    else {
        panic!("must serve");
    };
    assert_eq!(
        resp.coin_ids,
        ids[..2].to_vec(),
        "the echoed list is the budget-truncated request"
    );
}

#[tokio::test]
async fn coin_state_response_budget_is_trusted_for_configured_node_id() {
    let trusted = Bytes32::from([0xaa; 32]);
    let untrusted = Bytes32::from([0xbb; 32]);
    // untrusted response cap 2, trusted response cap 4 (subscription caps irrelevant here).
    let policy = Arc::new(TrustPolicy::with_caps(
        std::collections::HashSet::from([trusted]),
        usize::MAX,
        usize::MAX,
        2,
        4,
    ));
    let store = store_with(&[]).await;
    let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
    let api = api_trust(store, wallet, policy, default_sem());
    let ids: Vec<Bytes32> = (1u8..=4).map(|t| Bytes32::from([t; 32])).collect();
    let req = |coin_ids: Vec<Bytes32>| RequestCoinState {
        coin_ids,
        previous_height: None,
        header_hash: MAINNET.genesis_challenge,
        subscribe: false,
    };

    let CoinStateReply::Respond(untrusted_resp, _) =
        api.coin_state(untrusted, None, req(ids.clone())).await
    else {
        panic!("untrusted must serve");
    };
    assert_eq!(
        untrusted_resp.coin_ids,
        ids[..2].to_vec(),
        "untrusted echoes only the 100k-tier budget (2 here)"
    );

    let CoinStateReply::Respond(trusted_resp, _) =
        api.coin_state(trusted, None, req(ids.clone())).await
    else {
        panic!("trusted must serve");
    };
    assert_eq!(
        trusted_resp.coin_ids, ids,
        "trusted echoes the whole 500k-tier budget (4 here)"
    );
}

#[tokio::test]
async fn on_transaction_gives_trusted_peer_high_priority() {
    let trusted = Bytes32::from([0xaa; 32]);
    let untrusted = Bytes32::from([0xbb; 32]);
    let policy = Arc::new(TrustPolicy::new(std::collections::HashSet::from([trusted])));
    let store = store_with(&[]).await;
    let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
    let api = api_trust(store, wallet, policy, default_sem());

    let bundle = |b: u8| SpendBundle {
        coin_spends: vec![],
        aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::from([b; 96]),
    };
    let untrusted_tx = bundle(0x01);
    let trusted_tx = bundle(0x02);
    let untrusted_name = untrusted_tx.name().expect("name");
    let trusted_name = trusted_tx.name().expect("name");
    // Pre-solicit both ids so on_transaction accepts the bodies.
    {
        let mut req = api.tx_requested.lock().await;
        req.insert(
            untrusted_name,
            PendingTx {
                at: Instant::now(),
                advertised_fee: 0,
                advertised_cost: 1,
            },
        );
        req.insert(
            trusted_name,
            PendingTx {
                at: Instant::now(),
                advertised_fee: 0,
                advertised_cost: 1,
            },
        );
    }

    // Untrusted arrives FIRST, trusted SECOND.
    api.on_respond_transaction(untrusted, None, untrusted_tx)
        .await;
    api.on_respond_transaction(trusted, None, trusted_tx).await;

    let batch = api.tx_inbox.lock().await.drain_batch();
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch[0].0, trusted,
        "trusted bundle drains first (high-priority lane)"
    );
    assert_eq!(batch[1].0, untrusted, "untrusted bundle follows");
}

#[tokio::test]
async fn coin_state_response_budget_is_trusted_for_localhost_host() {
    let peer = Bytes32::from([0xcc; 32]);
    // EMPTY trusted node-id set; response caps untrusted 2 / trusted 4.
    let policy = Arc::new(TrustPolicy::with_caps(
        std::collections::HashSet::new(),
        usize::MAX,
        usize::MAX,
        2,
        4,
    ));
    let store = store_with(&[]).await;
    let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
    let api = api_trust(store, wallet, policy, default_sem());
    let ids: Vec<Bytes32> = (1u8..=4).map(|t| Bytes32::from([t; 32])).collect();
    let req = |coin_ids: Vec<Bytes32>| RequestCoinState {
        coin_ids,
        previous_height: None,
        header_hash: MAINNET.genesis_challenge,
        subscribe: false,
    };

    // A remote host (not localhost, not in any CIDR) → the untrusted 2-item budget.
    let CoinStateReply::Respond(remote_resp, _) = api
        .coin_state(peer, Some("203.0.113.7".parse().unwrap()), req(ids.clone()))
        .await
    else {
        panic!("remote must serve");
    };
    assert_eq!(
        remote_resp.coin_ids,
        ids[..2].to_vec(),
        "remote peer echoes only the untrusted budget (2 here)"
    );

    // The SAME peer id from 127.0.0.1 → the trusted 4-item budget, with no config change.
    let CoinStateReply::Respond(local_resp, _) = api
        .coin_state(peer, Some("127.0.0.1".parse().unwrap()), req(ids.clone()))
        .await
    else {
        panic!("localhost must serve");
    };
    assert_eq!(
        local_resp.coin_ids, ids,
        "localhost peer echoes the whole trusted budget (4 here)"
    );
}

#[tokio::test]
async fn on_transaction_gives_localhost_peer_high_priority() {
    let local_peer = Bytes32::from([0xcc; 32]);
    let remote_peer = Bytes32::from([0xdd; 32]);
    // Empty policy: trust comes purely from the host being localhost.
    let policy = Arc::new(TrustPolicy::default());
    let store = store_with(&[]).await;
    let wallet = Arc::new(WalletNotifier::with_trust(policy.clone()));
    let api = api_trust(store, wallet, policy, default_sem());
    let bundle = |b: u8| SpendBundle {
        coin_spends: vec![],
        aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::from([b; 96]),
    };
    let remote_tx = bundle(0x01);
    let local_tx = bundle(0x02);
    let remote_name = remote_tx.name().expect("name");
    let local_name = local_tx.name().expect("name");
    {
        let mut req = api.tx_requested.lock().await;
        req.insert(
            remote_name,
            PendingTx {
                at: Instant::now(),
                advertised_fee: 0,
                advertised_cost: 1,
            },
        );
        req.insert(
            local_name,
            PendingTx {
                at: Instant::now(),
                advertised_fee: 0,
                advertised_cost: 1,
            },
        );
    }

    // Remote (untrusted) arrives FIRST, localhost SECOND.
    api.on_respond_transaction(
        remote_peer,
        Some("198.51.100.9".parse().unwrap()),
        remote_tx,
    )
    .await;
    api.on_respond_transaction(local_peer, Some("127.0.0.1".parse().unwrap()), local_tx)
        .await;

    let batch = api.tx_inbox.lock().await.drain_batch();
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch[0].0, local_peer,
        "localhost bundle drains first (high-priority lane)"
    );
    assert_eq!(batch[1].0, remote_peer, "remote bundle follows");
}
