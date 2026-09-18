//! End-to-end derivation over real store backends: build the same 3-block chain through the
//! public `dg_xch_stores` write API on BOTH the SQLite and mmap backends, derive with the
//! read-only tool, and require (a) both backends produce identical roots (the cross-backend
//! agreement the live quorum run scales up), (b) the store path equals feeding the
//! accumulator directly in canonical order, and (c) the tip root matches a frozen vector.

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_dev_tools::coin_roots::CoinSetAccumulator;
use dg_xch_dev_tools::coin_roots::derive::{StoreUrl, derive};
use dg_xch_stores::{BlockStore, CoinStore, MmapStore, SqliteStore};

fn synth_hash(tag: u8, height: u32) -> Bytes32 {
    let mut h = [tag; 32];
    h[28..32].copy_from_slice(&height.to_be_bytes());
    Bytes32::from(h)
}

// A synthetic record cloned off a real mainnet template, re-linked into a 1..=3 chain (the
// same shape the stores contract tests use).
fn linked(template: &BlockRecord, height: u32) -> BlockRecord {
    let mut r = template.clone();
    r.header_hash = synth_hash(0x10, height);
    r.prev_hash = synth_hash(0x10, height.wrapping_sub(1));
    r.height = height;
    r.weight = u128::from(height);
    r.total_iters = u128::from(height);
    r.sub_epoch_summary_included = None;
    r
}

fn template() -> BlockRecord {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../stores/tests/fixtures/block_records.json"
    ))
    .expect("fixture");
    let records: Vec<BlockRecord> = serde_json::from_str(&raw).expect("fixture parse");
    records.into_iter().next().expect("nonempty fixture")
}

fn coin(tag: u8, amount: u64) -> Coin {
    Coin {
        parent_coin_info: Bytes32::from([tag; 32]),
        puzzle_hash: Bytes32::from([tag ^ 0xFF; 32]),
        amount,
    }
}

fn record(c: Coin, height: u32, timestamp: u64) -> CoinRecord {
    CoinRecord {
        coin: c,
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase: false,
        timestamp,
        spent: false,
    }
}

// The 3-block script: (height, timestamp, additions, removals-by-tag).
// A(h1) is spent at h2; D(h2) is spent at h3.
struct Script {
    blocks: Vec<(u32, u64, Vec<Coin>, Vec<Bytes32>)>,
}

fn script() -> Script {
    let a = coin(1, 100);
    let b = coin(2, 200);
    let c = coin(3, 300);
    let d = coin(4, 400);
    let e = coin(5, 500);
    let f = coin(6, 600);
    Script {
        blocks: vec![
            (1, 1111, vec![a, b, c], vec![]),
            (2, 2222, vec![d], vec![a.name()]),
            (3, 3333, vec![e, f], vec![d.name()]),
        ],
    }
}

async fn populate<S: BlockStore + CoinStore>(store: &S) {
    let tmpl = template();
    let records: Vec<BlockRecord> = (1..=3).map(|h| linked(&tmpl, h)).collect();
    store.add_block_records(&records).await.expect("records");
    store.set_peak(&synth_hash(0x10, 3)).await.expect("peak");
    for (height, ts, adds, rems) in script().blocks {
        let adds: Vec<CoinRecord> = adds.into_iter().map(|c| record(c, height, ts)).collect();
        store
            .apply_block(height, ts, &adds, &rems)
            .await
            .expect("apply");
    }
}

/// The expected roots, computed by feeding the accumulator directly in canonical
/// `(confirmed_height, coin_id)` order — no store involved.
fn direct_roots(boundaries: &[u32]) -> Vec<dg_xch_dev_tools::coin_roots::RootV1> {
    // (coin_id, confirmed, timestamp, spent_index)
    let mut coins: Vec<(Bytes32, u32, u64, u32)> = Vec::new();
    let mut spent_at: std::collections::HashMap<[u8; 32], u32> = std::collections::HashMap::new();
    for (height, _, _, rems) in &script().blocks {
        for r in rems {
            spent_at.insert(r.const_bytes(), *height);
        }
    }
    for (height, ts, adds, _) in &script().blocks {
        for c in adds {
            let name = c.name();
            let spent = spent_at.get(&name.const_bytes()).copied().unwrap_or(0);
            coins.push((name, *height, *ts, spent));
        }
    }
    coins.sort_unstable_by_key(|x| (x.1, x.0.const_bytes()));
    let mut acc = CoinSetAccumulator::new();
    let mut out = Vec::new();
    let mut next = 0usize;
    for (cid, ch, ts, spent) in coins {
        while next < boundaries.len() && ch > boundaries[next] {
            let h = boundaries[next];
            out.push(acc.root_at(h, synth_hash(0x10, h)).expect("root"));
            next += 1;
        }
        acc.append(cid, ch, ts, spent).expect("append");
    }
    while next < boundaries.len() {
        let h = boundaries[next];
        out.push(acc.root_at(h, synth_hash(0x10, h)).expect("root"));
        next += 1;
    }
    out
}

#[tokio::test]
async fn sqlite_and_mmap_derive_identical_known_roots() {
    let boundaries = [1u32, 2, 3];

    // SQLite store.
    let sqlite_dir = tempfile::tempdir().expect("tempdir");
    let db_path = sqlite_dir.path().join("chain.sqlite");
    {
        let store = SqliteStore::open(&db_path).await.expect("open sqlite");
        populate(&store).await;
    }
    let sqlite_roots = derive(&StoreUrl::Sqlite(db_path), &boundaries)
        .await
        .expect("sqlite derive");

    // mmap store.
    let mmap_dir = tempfile::tempdir().expect("tempdir");
    {
        let store = MmapStore::open(mmap_dir.path()).await.expect("open mmap");
        populate(&store).await;
    }
    let mmap_roots = derive(&StoreUrl::Mmap(mmap_dir.path().to_path_buf()), &boundaries)
        .await
        .expect("mmap derive");

    let expected = direct_roots(&boundaries);
    assert_eq!(sqlite_roots.len(), 3);
    assert_eq!(mmap_roots.len(), 3);
    for ((s, m), e) in sqlite_roots.iter().zip(&mmap_roots).zip(&expected) {
        assert_eq!(
            s.root_v1.const_bytes(),
            e.root_v1.const_bytes(),
            "sqlite vs direct at {}",
            e.height
        );
        assert_eq!(
            m.root_v1.const_bytes(),
            e.root_v1.const_bytes(),
            "mmap vs direct at {}",
            e.height
        );
        assert_eq!(s.coin_count, e.coin_count);
        assert_eq!(s.spent_count, e.spent_count);
        assert_eq!(m.coin_count, e.coin_count);
        assert_eq!(m.spent_count, e.spent_count);
    }
    // Tallies over the script: 3/4/6 coins, 0/1/2 spends.
    assert_eq!(
        expected
            .iter()
            .map(|r| (r.coin_count, r.spent_count))
            .collect::<Vec<_>>(),
        vec![(3, 0), (4, 1), (6, 2)]
    );
    // Frozen replay vector: the tip root of this exact 3-block script, pinned so layout
    // drift in EITHER the accumulator or the derive path fails loudly.
    assert_eq!(
        format!("{}", sqlite_roots[2].root_v1),
        "0xaf2b9e0fa8cac7f3d42e2b129ea6fb877b81234c275595985d4482dd2ba13196"
    );
}

#[tokio::test]
async fn missing_boundary_header_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("chain.sqlite");
    {
        let store = SqliteStore::open(&db_path).await.expect("open sqlite");
        populate(&store).await;
    }
    let err = derive(&StoreUrl::Sqlite(db_path), &[7])
        .await
        .expect_err("no block at 7");
    assert!(err.to_string().contains("no main-chain block"), "{err}");
}
