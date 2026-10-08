use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::CoinStore;
use std::collections::HashSet;

// A synthetic unspent coin record keyed by `tag` — lets a test place additions and removals at exact
// heights (the real adds/rems fixtures don't guarantee a coin created and spent across a chosen fork).
fn synth_coin(tag: u8, amount: u64) -> CoinRecord {
    let mut parent = [tag; 32];
    parent[0] = 0xaa;
    CoinRecord {
        coin: Coin {
            parent_coin_info: Bytes32::from(parent),
            puzzle_hash: Bytes32::from([tag; 32]),
            amount,
        },
        confirmed_block_index: 0,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    }
}

const HEIGHTS: [u32; 4] = [5_000_000, 5_000_004, 5_000_007, 5_000_012];
const FORK: u32 = 5_000_004;

fn ts_for(records: &[BlockRecord], height: u32) -> u64 {
    records
        .iter()
        .find(|r| r.height == height)
        .and_then(|r| r.timestamp)
        .expect("transaction block has a timestamp")
}

fn sort_by_name(mut v: Vec<CoinRecord>) -> Vec<CoinRecord> {
    v.sort_by_key(|c| c.coin.name().bytes());
    v
}

fn bytes(records: &[CoinRecord]) -> Vec<Vec<u8>> {
    records
        .iter()
        .map(|c| c.to_bytes(ChiaProtocolVersion::default()).unwrap())
        .collect()
}

/// The reference unspent set after applying blocks `<= fork` in height order: every addition not spent by a
/// block `<= fork`, in the exact shape the store reads back (unspent, confirmed at its creation height).
fn expected_unspent(records: &[BlockRecord], fork: u32) -> Vec<CoinRecord> {
    let mut removed: HashSet<Bytes32> = HashSet::new();
    for h in HEIGHTS.into_iter().filter(|h| *h <= fork) {
        let (_, rems) = common::load_adds_rems(h);
        removed.extend(rems.iter().map(|c| c.coin.name()));
    }
    let mut out = Vec::new();
    for h in HEIGHTS.into_iter().filter(|h| *h <= fork) {
        let ts = ts_for(records, h);
        let (adds, _) = common::load_adds_rems(h);
        for a in adds {
            if !removed.contains(&a.coin.name()) {
                out.push(CoinRecord {
                    coin: a.coin,
                    confirmed_block_index: h,
                    spent_block_index: 0,
                    coinbase: a.coinbase,
                    timestamp: ts,
                    spent: false,
                });
            }
        }
    }
    sort_by_name(out)
}

#[tokio::test]
async fn apply_real_block_additions_removals_then_rollback_byte_matches() {
    let records = common::load_records();
    let store = common::new_store().await;

    for h in HEIGHTS {
        let ts = ts_for(&records, h);
        let (adds, rems) = common::load_adds_rems(h);
        let removal_names: Vec<Bytes32> = rems.iter().map(|c| c.coin.name()).collect();
        store
            .apply_block(h, ts, &adds, &removal_names)
            .await
            .unwrap();
    }

    let reverted = store.rollback_to(FORK).await.unwrap();
    assert!(reverted > 0, "rollback_to returns a count, not a set");

    let mut names_le_fork: Vec<Bytes32> = Vec::new();
    for h in HEIGHTS.into_iter().filter(|h| *h <= FORK) {
        let (adds, _) = common::load_adds_rems(h);
        names_le_fork.extend(adds.iter().map(|c| c.coin.name()));
    }
    let got = store.get_coin_records(&names_le_fork).await.unwrap();
    let unspent_got = sort_by_name(
        got.into_iter()
            .filter(|c| c.spent_block_index == 0)
            .collect(),
    );

    let expected = expected_unspent(&records, FORK);
    assert_eq!(unspent_got.len(), expected.len(), "unspent set size");
    assert_eq!(unspent_got, expected, "unspent set structural match");
    assert_eq!(
        bytes(&unspent_got),
        bytes(&expected),
        "unspent set byte match"
    );

    let mut names_gt_fork: Vec<Bytes32> = Vec::new();
    for h in HEIGHTS.into_iter().filter(|h| *h > FORK) {
        let (adds, _) = common::load_adds_rems(h);
        names_gt_fork.extend(adds.iter().map(|c| c.coin.name()));
    }
    let above = store.get_coin_records(&names_gt_fork).await.unwrap();
    assert!(
        above.is_empty(),
        "additions above the fork are deleted by rollback"
    );
}

#[tokio::test]
async fn point_get_and_multi_get_return_owned_records() {
    let records = common::load_records();
    let store = common::new_store().await;
    let ts = ts_for(&records, 5_000_000);
    let (adds, _) = common::load_adds_rems(5_000_000);
    store.apply_block(5_000_000, ts, &adds, &[]).await.unwrap();

    let name = adds[0].coin.name();
    let one = store
        .get_coin_record(&name)
        .await
        .unwrap()
        .expect("present");
    assert_eq!(one.coin, adds[0].coin);
    assert_eq!(one.confirmed_block_index, 5_000_000);
    assert!(!one.spent);

    let names: Vec<Bytes32> = adds.iter().take(5).map(|c| c.coin.name()).collect();
    let many = store.get_coin_records(&names).await.unwrap();
    assert_eq!(many.len(), 5);
}

// A coin created below the fork
// and spent above it is marked spent at the spend height, then un-spent by a rollback to the fork —
// the removal is reverted (spent_index -> 0) while the coin itself survives (created below the fork).
#[tokio::test]
async fn coin_spent_above_fork_is_unspent_by_rollback() {
    let store = common::new_store().await;
    let coin = synth_coin(0x51, 1234);
    let name = coin.coin.name();

    // Created at 5_000_000, unspent.
    store
        .apply_block(5_000_000, 1_700_000_000, std::slice::from_ref(&coin), &[])
        .await
        .unwrap();
    let created = store
        .get_coin_record(&name)
        .await
        .unwrap()
        .expect("present");
    assert!(!created.spent, "freshly created coin is unspent");
    assert_eq!(created.confirmed_block_index, 5_000_000);
    assert_eq!(created.spent_block_index, 0);

    // Spent at 5_000_010.
    store
        .apply_block(5_000_010, 1_700_000_100, &[], std::slice::from_ref(&name))
        .await
        .unwrap();
    let spent = store
        .get_coin_record(&name)
        .await
        .unwrap()
        .expect("present");
    assert!(spent.spent, "coin is spent after the removal block");
    assert_eq!(
        spent.spent_block_index, 5_000_010,
        "spent at the removal height"
    );
    assert_eq!(
        spent.confirmed_block_index, 5_000_000,
        "creation height unchanged"
    );

    // Rollback to a fork between creation and spend: the coin returns to unspent, still present.
    let reverted = store.rollback_to(5_000_005).await.unwrap();
    assert!(reverted >= 1, "rollback reverts at least the un-spend");
    let restored = store
        .get_coin_record(&name)
        .await
        .unwrap()
        .expect("still present");
    assert!(
        !restored.spent,
        "spend above the fork is reverted -> unspent"
    );
    assert_eq!(
        restored.spent_block_index, 0,
        "spent_index reset to 0 by rollback"
    );
    assert_eq!(
        restored.confirmed_block_index, 5_000_000,
        "creation survives the rollback"
    );
}

// After applying a batch of additions and spending a subset, exactly
// the un-spent coins read back with spent_index == 0.
#[tokio::test]
async fn unspent_count_reflects_partial_spend() {
    let store = common::new_store().await;
    let coins: Vec<CoinRecord> = (0..8)
        .map(|i| synth_coin(0x60 + i, 100 + u64::from(i)))
        .collect();
    let names: Vec<Bytes32> = coins.iter().map(|c| c.coin.name()).collect();

    store
        .apply_block(5_000_000, 1_700_000_000, &coins, &[])
        .await
        .unwrap();

    // Spend the first three at a later block.
    let spent_names: Vec<Bytes32> = names.iter().take(3).copied().collect();
    store
        .apply_block(5_000_003, 1_700_000_030, &[], &spent_names)
        .await
        .unwrap();

    let all = store.get_coin_records(&names).await.unwrap();
    assert_eq!(all.len(), 8, "every coin is still recorded (spent or not)");
    let unspent = all.iter().filter(|c| c.spent_block_index == 0).count();
    let spent = all.iter().filter(|c| c.spent).count();
    assert_eq!(unspent, 5, "five coins remain unspent");
    assert_eq!(spent, 3, "three coins are spent");
}

// Regression for the batch multi-get: a name list far longer than SQLite's `IN`-list search-vs-scan
// crossover (the old `coin_name IN (...)` form planned a full table SCAN at ~100 names and stalled the
// node) must still return every present record and skip absent ones. Point-get semantics: order-
// independent, existing-only. 250 present + 50 absent exercises the long-list path both ways.
#[tokio::test]
async fn multi_get_large_batch_returns_present_skips_absent() {
    let store = common::new_store().await;
    let present: Vec<CoinRecord> = (0..250u32)
        .map(|i| {
            let mut c = synth_coin((i % 251) as u8, 1_000 + u64::from(i));
            // Disambiguate names beyond the single-byte tag so all 250 are distinct coins.
            let mut parent = c.coin.parent_coin_info.bytes().to_vec();
            parent[1] = (i >> 8) as u8;
            parent[2] = (i & 0xff) as u8;
            c.coin.parent_coin_info = Bytes32::from(<[u8; 32]>::try_from(parent).unwrap());
            c
        })
        .collect();
    store
        .apply_block(5_000_000, 1_700_000_000, &present, &[])
        .await
        .unwrap();

    let present_names: Vec<Bytes32> = present.iter().map(|c| c.coin.name()).collect();
    let absent_names: Vec<Bytes32> = (0..50u32)
        .map(|i| Bytes32::from([0xde_u8.wrapping_add(i as u8); 32]))
        .collect();
    let mut query_names = present_names.clone();
    query_names.extend(absent_names);

    let got = store.get_coin_records(&query_names).await.unwrap();
    assert_eq!(
        got.len(),
        present.len(),
        "every present coin returns, every absent name is skipped"
    );
    let got_names: HashSet<Bytes32> = got.iter().map(|c| c.coin.name()).collect();
    for n in &present_names {
        assert!(got_names.contains(n), "present coin {n} must be returned");
    }
}

// `rollback_to_in` shares the batch's transaction: dropped, the fork revert never happened;
// committed, it lands together with the branch re-applies staged after it — the engine's
// single-transaction reorg shape.
#[tokio::test]
async fn rollback_to_in_is_atomic_with_the_batch() {
    use dg_xch_stores::BlockStore;
    let store = common::new_store().await;
    let below = synth_coin(0x71, 100); // created at 10, spent at 11
    let above = synth_coin(0x72, 200); // created at 11
    store
        .apply_block(10, 1_700_000_000, std::slice::from_ref(&below), &[])
        .await
        .unwrap();
    store
        .apply_block(
            11,
            1_700_000_100,
            std::slice::from_ref(&above),
            &[below.coin.name()],
        )
        .await
        .unwrap();

    // Dropped batch: the staged revert must not have happened.
    {
        let mut batch = store.begin().await.unwrap();
        let reverted = store.rollback_to_in(&mut batch, 10).await.unwrap();
        assert_eq!(reverted, 2, "one deletion + one un-spend staged");
        // Dropped without commit (the crashed-reorg shape).
    }
    let still_above = store
        .get_coin_record(&above.coin.name())
        .await
        .unwrap()
        .expect("above-fork coin untouched by the dropped batch");
    assert_eq!(still_above.confirmed_block_index, 11);
    let still_spent = store
        .get_coin_record(&below.coin.name())
        .await
        .unwrap()
        .unwrap();
    assert!(still_spent.spent, "spend untouched by the dropped batch");

    // Committed batch: revert + a branch re-apply land as one unit.
    let branch = synth_coin(0x73, 300);
    let mut batch = store.begin().await.unwrap();
    store.rollback_to_in(&mut batch, 10).await.unwrap();
    store
        .apply_block_in(
            &mut batch,
            11,
            1_700_000_200,
            std::slice::from_ref(&branch),
            &[below.coin.name()],
        )
        .await
        .unwrap();
    store.commit(batch).await.unwrap();
    assert!(
        store
            .get_coin_record(&above.coin.name())
            .await
            .unwrap()
            .is_none(),
        "old above-fork coin reverted"
    );
    let branch_got = store
        .get_coin_record(&branch.coin.name())
        .await
        .unwrap()
        .expect("branch coin applied");
    assert_eq!(branch_got.confirmed_block_index, 11);
    let respent = store
        .get_coin_record(&below.coin.name())
        .await
        .unwrap()
        .unwrap();
    assert!(respent.spent, "re-spent by the branch in the same unit");
    assert_eq!(respent.spent_block_index, 11);
}

// The wallet-serve read caps: `max_items` lives IN the store query — a running LIMIT budget for
// the sqlite/postgres backends, a scan cut-off for mmap. Never fetch-then-truncate: a dust-storm
// puzzle hash must not materialize an unbounded row set.
#[cfg(feature = "coin-index")]
#[tokio::test]
async fn coin_state_queries_are_bounded_by_max_items() {
    let store = common::new_store().await;
    let ph = Bytes32::from([0x77; 32]);
    let records: Vec<CoinRecord> = (1u8..=5)
        .map(|t| {
            let mut c = synth_coin(t, u64::from(t));
            c.coin.puzzle_hash = ph;
            c
        })
        .collect();
    store
        .apply_block(5_000_000, 1_700_000_000, &records, &[])
        .await
        .unwrap();

    let states = store
        .get_coin_states_by_puzzle_hashes(&[ph], 0, true, 3)
        .await
        .unwrap();
    assert_eq!(states.len(), 3, "the LIMIT budget bounds the ph query");
    let unbounded = store
        .get_coin_states_by_puzzle_hashes(&[ph], 0, true, 50_000)
        .await
        .unwrap();
    assert_eq!(unbounded.len(), 5, "under the cap, everything serves");

    let ids: Vec<Bytes32> = records.iter().map(|r| r.coin.name()).collect();
    let states = store
        .get_coin_states_by_ids(&ids, 0, true, 2)
        .await
        .unwrap();
    assert_eq!(states.len(), 2, "max_items bounds the by-ids read");

    #[cfg(feature = "hint")]
    {
        let hint = Bytes32::from([0x88; 32]);
        let pairs: Vec<(Bytes32, Bytes32)> = ids.iter().map(|id| (hint, *id)).collect();
        store.apply_hints(&pairs).await.unwrap();
        let got = store.get_coins_for_hint(&hint, 2).await.unwrap();
        assert_eq!(got.len(), 2, "the hint id lookup is LIMIT-bounded");
        let all = store.get_coins_for_hint(&hint, 50_000).await.unwrap();
        assert_eq!(all.len(), 5);
    }
}

// ---- batch_coin_states_by_puzzle_hashes — the RequestPuzzleState read ----

#[cfg(feature = "coin-index")]
mod batch_puzzle_state {
    use super::{common, synth_coin};
    use dg_xch_core::blockchain::coin_record::CoinRecord;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use dg_xch_core::protocols::wallet::{CoinState, CoinStateFilters};
    use dg_xch_stores::CoinStore;

    #[allow(non_snake_case)]
    fn PH() -> Bytes32 {
        Bytes32::from([0x55; 32])
    }

    fn filters(spent: bool, unspent: bool, hinted: bool, min_amount: u64) -> CoinStateFilters {
        CoinStateFilters {
            include_spent: spent,
            include_unspent: unspent,
            include_hinted: hinted,
            min_amount,
        }
    }

    fn state_height(cs: &CoinState) -> u32 {
        cs.created_height
            .unwrap_or(0)
            .max(cs.spent_height.unwrap_or(0))
    }

    // Seed: heights 10..=14, `per_height` coins each on PH (tags encode (height, i)); the coin at
    // (h, 0) is SPENT at h+1... no — spends complicate height bookkeeping; keep creations only and
    // spend selected tags explicitly from the caller.
    async fn seed(store: &impl CoinStore, heights: &[u32], per_height: usize) -> Vec<CoinRecord> {
        let mut all = Vec::new();
        for (hi, h) in heights.iter().enumerate() {
            let recs: Vec<CoinRecord> = (0..per_height)
                .map(|i| {
                    let mut c = synth_coin(
                        u8::try_from(hi * per_height + i + 1).expect("small test set"),
                        1_000 + (i as u64),
                    );
                    c.coin.puzzle_hash = PH();
                    c.confirmed_block_index = *h;
                    c
                })
                .collect();
            store
                .apply_block(*h, 1_700_000_000, &recs, &[])
                .await
                .unwrap();
            all.extend(recs);
        }
        all
    }

    // Everything fits max_items → (all, None), ordered by height.
    #[tokio::test]
    async fn single_page_is_finished_and_height_ordered() {
        let store = common::new_store().await;
        let seeded = seed(&store, &[10, 11, 12], 2).await;
        let (states, next) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(true, true, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(next, None, "under max_items the page is final");
        assert_eq!(states.len(), seeded.len());
        let heights: Vec<u32> = states.iter().map(state_height).collect();
        let mut sorted = heights.clone();
        sorted.sort_unstable();
        assert_eq!(heights, sorted, "ascending activity height");
    }

    // Over max_items → the last state is the next page's floor and NO
    // state from that height leaks into this page (a block is never split across pages). Driving
    // the page loop to completion recovers exactly the seeded set with no duplicates.
    #[tokio::test]
    async fn paging_never_splits_a_height_and_the_loop_recovers_everything() {
        let store = common::new_store().await;
        // 4 heights x 3 coins = 12 states; max_items=4 forces page boundaries INSIDE heights
        // (4 mod 3 != 0), so the whole-height trim must shrink pages below max_items.
        let seeded = seed(&store, &[10, 11, 12, 13], 3).await;
        let mut min_height = 0u32;
        let mut collected: Vec<CoinState> = Vec::new();
        let mut pages = 0;
        loop {
            let (states, next) = store
                .batch_coin_states_by_puzzle_hashes(
                    &[PH()],
                    min_height,
                    &filters(true, true, true, 0),
                    4,
                )
                .await
                .unwrap();
            pages += 1;
            assert!(states.len() <= 4, "no page exceeds max_items");
            if let Some(next_height) = next {
                assert!(
                    states.iter().all(|cs| state_height(cs) < next_height),
                    "no state at the boundary height leaks into the earlier page"
                );
                collected.extend(states);
                min_height = next_height;
            } else {
                collected.extend(states);
                break;
            }
            assert!(pages < 20, "the page loop must terminate");
        }
        assert!(pages > 1, "the scenario must actually page");
        let mut got: Vec<Vec<u8>> = collected
            .iter()
            .map(|cs| {
                use dg_xch_core::traits::SizedBytes;
                cs.coin.name().bytes().to_vec()
            })
            .collect();
        let before = got.len();
        got.sort_unstable();
        got.dedup();
        assert_eq!(got.len(), before, "no duplicates across pages");
        assert_eq!(
            got.len(),
            seeded.len(),
            "the loop recovers every seeded state"
        );
    }

    // The spent/unspent filter legs, and both-false short-circuiting to a finished empty page.
    #[tokio::test]
    async fn spent_unspent_filters_partition_the_set() {
        let store = common::new_store().await;
        let seeded = seed(&store, &[10, 11], 2).await;
        // spend one coin at height 12.
        let victim = seeded[0].coin.name();
        store
            .apply_block(12, 1_700_000_100, &[], &[victim])
            .await
            .unwrap();

        let (both, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(true, true, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(both.len(), 4);

        let (spent_only, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(true, false, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(spent_only.len(), 1);
        assert_eq!(spent_only[0].coin.name(), victim);
        assert_eq!(spent_only[0].spent_height, Some(12));

        let (unspent_only, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(false, true, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(unspent_only.len(), 3);
        assert!(unspent_only.iter().all(|cs| cs.spent_height.is_none()));

        let (neither, next) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(false, false, true, 0), 50_000)
            .await
            .unwrap();
        assert!(neither.is_empty(), "no coin is both spent and unspent");
        assert_eq!(next, None);

        // The min_height floor is created-OR-spent (confirmed >= ? OR spent >= ?): the coin
        // created at 10 but spent at 12 still surfaces above a floor of 12.
        let (active, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 12, &filters(true, true, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].coin.name(), victim);
    }

    // min_amount: `amount >= ?` on the big-endian blob.
    #[tokio::test]
    async fn min_amount_filters_dust() {
        let store = common::new_store().await;
        // amounts 1_000 and 1_001 per seed(); add a large-amount coin to prove the BE-blob
        // comparison is numeric (a bytewise compare of little-endian would order these wrong).
        seed(&store, &[10], 2).await;
        let mut big = synth_coin(0x70, 1 << 40);
        big.coin.puzzle_hash = PH();
        big.confirmed_block_index = 11;
        store
            .apply_block(11, 1_700_000_000, &[big], &[])
            .await
            .unwrap();

        let (all, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(true, true, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        let (rich, _) = store
            .batch_coin_states_by_puzzle_hashes(
                &[PH()],
                0,
                &filters(true, true, true, 1_001),
                50_000,
            )
            .await
            .unwrap();
        assert_eq!(rich.len(), 2, "the 1_000 coin is below the floor");
        let (whale, _) = store
            .batch_coin_states_by_puzzle_hashes(
                &[PH()],
                0,
                &filters(true, true, true, (1 << 40) - 1),
                50_000,
            )
            .await
            .unwrap();
        assert_eq!(whale.len(), 1);
        assert_eq!(whale[0].coin.amount, 1 << 40);
    }

    // include_hinted: a coin whose OWN puzzle hash is
    // foreign but whose HINT is the requested hash (the CAT/NFT shape) surfaces iff
    // include_hinted; a plain+hinted overlap dedups by coin id.
    #[cfg(feature = "hint")]
    #[tokio::test]
    async fn hinted_coins_join_and_dedup() {
        let store = common::new_store().await;
        seed(&store, &[10], 1).await;
        // A CAT-shaped coin: foreign puzzle hash, hinted at PH.
        let mut cat = synth_coin(0x71, 5_000);
        cat.confirmed_block_index = 11;
        store
            .apply_block(11, 1_700_000_000, &[cat], &[])
            .await
            .unwrap();
        store.apply_hints(&[(PH(), cat.coin.name())]).await.unwrap();
        // A plain coin ALSO hinted at PH (the overlap that must dedup).
        let (plain, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(true, true, false, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(plain.len(), 1, "hinted excluded without the flag");
        let overlap = plain[0].coin.name();
        store.apply_hints(&[(PH(), overlap)]).await.unwrap();

        let (with_hints, _) = store
            .batch_coin_states_by_puzzle_hashes(&[PH()], 0, &filters(true, true, true, 0), 50_000)
            .await
            .unwrap();
        assert_eq!(
            with_hints.len(),
            2,
            "the CAT joins; the plain+hinted overlap dedups by coin id"
        );
        assert!(
            with_hints
                .iter()
                .any(|cs| cs.coin.name() == cat.coin.name())
        );
    }
}

// apply_block sorts its addition/removal batches by coin_name before writing (pkey-locality:
// key-contiguous chunks share btree paths instead of one random descent per coin). That sort must
// be invisible: the upsert (INSERT OR REPLACE / ON CONFLICT) and the IN-list spend-update are
// order-independent, so any input order lands the identical final rows.
#[tokio::test]
async fn apply_order_never_changes_the_landed_rows() {
    let (adds, _) = common::load_adds_rems(5_000_000);
    let adds = &adds[..64];
    let spend: Vec<Bytes32> = adds[..16].iter().map(|c| c.coin.name()).collect();

    let forward = common::new_store().await;
    forward
        .apply_block(10, 1_700_000_000, adds, &[])
        .await
        .unwrap();
    forward
        .apply_block(11, 1_700_000_100, &[], &spend)
        .await
        .unwrap();

    let mut shuffled_adds: Vec<CoinRecord> = adds.to_vec();
    shuffled_adds.reverse();
    let mut shuffled_spend = spend.clone();
    shuffled_spend.reverse();
    let reversed = common::new_store().await;
    reversed
        .apply_block(10, 1_700_000_000, &shuffled_adds, &[])
        .await
        .unwrap();
    reversed
        .apply_block(11, 1_700_000_100, &[], &shuffled_spend)
        .await
        .unwrap();

    let names: Vec<Bytes32> = adds.iter().map(|c| c.coin.name()).collect();
    let a = sort_by_name(forward.get_coin_records(&names).await.unwrap());
    let b = sort_by_name(reversed.get_coin_records(&names).await.unwrap());
    assert_eq!(a.len(), 64, "every addition lands");
    assert_eq!(
        bytes(&a),
        bytes(&b),
        "input order must be invisible in the rows"
    );
    assert_eq!(
        a.iter().filter(|c| c.spent).count(),
        16,
        "exactly the spent set is marked spent"
    );
}
