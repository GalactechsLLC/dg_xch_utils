use super::*;
use crate::NativePrimitives;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_stores::SqliteStore;
use std::sync::atomic::Ordering;

fn block() -> FullBlock {
    serde_json::from_str(include_str!("../fixtures/full_block_5000000.json")).unwrap()
}

fn record(index: u64, height: u32) -> CoinRecord {
    CoinRecord {
        coin: Coin {
            parent_coin_info: Bytes32::from([7; 32]),
            puzzle_hash: Bytes32::from([8; 32]),
            amount: index,
        },
        confirmed_block_index: height,
        spent_block_index: 0,
        spent: false,
        coinbase: false,
        timestamp: u64::from(height),
    }
}

async fn engine() -> (tempfile::TempDir, Engine<SqliteStore, NativePrimitives>) {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("coins.sqlite"))
        .await
        .unwrap();
    (
        directory,
        Engine::new(store, NativePrimitives, MAINNET).with_enforced_coin_rules(),
    )
}

#[tokio::test]
async fn incremental_view_matches_reconstruction_including_ancestor_precedence() {
    let (_directory, mut engine) = engine().await;
    engine.stage_coins = Some(StageCoins::default());
    let records: Vec<BlockRecord> =
        serde_json::from_str(include_str!("../fixtures/block_records.json")).unwrap();
    let mut parent = Bytes32::from([0; 32]);
    let mut branch_parent = parent;
    let mut previous_pointer = None;
    for height in 0..128 {
        let mut block = block();
        block.reward_chain_block.height = height;
        block.foliage.prev_block_hash = parent;
        let cached = engine.fork_view(&block).await.unwrap();
        let reference = engine.rebuild_fork_view(&block).await.unwrap();
        assert_eq!(cached.fork_height, reference.fork_height);
        assert_eq!(cached.additions, reference.additions);
        assert_eq!(cached.removals, reference.removals);
        let pointer = std::sync::Arc::as_ptr(&cached);
        if let Some(previous) = previous_pointer {
            assert_eq!(pointer, previous);
        }
        previous_pointer = Some(pointer);
        drop(cached);
        let header_hash = block.header_hash().unwrap();
        let mut template = records[0].clone();
        template.header_hash = header_hash;
        template.prev_hash = parent;
        template.height = height;
        let delta = BlockDelta {
            header_hash,
            prev_hash: parent,
            height,
            weight: u128::from(height) + 1,
            timestamp: u64::from(height),
            record: template,
            additions: vec![record(u64::from(height), height), record(1000, height)],
            removals: if height > 0 {
                vec![record(u64::from(height - 1), height - 1).coin.name()]
            } else {
                vec![]
            },
            hints: vec![],
        };
        engine.finish_stage(&block, delta);
        parent = header_hash;
        if height == 32 {
            branch_parent = parent;
        }
    }
    assert_eq!(
        engine
            .store
            .telemetry()
            .unwrap()
            .coin_view_reused
            .load(Ordering::Relaxed),
        127
    );
    let mut branch = block();
    branch.reward_chain_block.height = 33;
    branch.foliage.prev_block_hash = branch_parent;
    let cached = engine.fork_view(&branch).await.unwrap();
    let reference = engine.rebuild_fork_view(&branch).await.unwrap();
    assert_eq!(cached.additions, reference.additions);
    assert_eq!(cached.removals, reference.removals);
    engine.clear_stage_preload();
    assert!(engine.stage_coins.is_none());
}

#[tokio::test]
async fn cached_absence_and_confirmed_records_preserve_fork_lookup_precedence() {
    let (_directory, mut engine) = engine().await;
    let addition = record(42, 1);
    let name = addition.coin.name();
    engine
        .store
        .apply_block(1, 1, &[addition], &[name])
        .await
        .unwrap();
    let spent = engine.store.get_coin_record(&name).await.unwrap().unwrap();
    let mut fork = ForkView {
        fork_height: 1,
        additions: HashMap::from([(name, addition)]),
        removals: HashSet::new(),
    };
    for cached in [false, true] {
        engine.stage_coins = cached.then(|| StageCoins {
            view: None,
            records: HashMap::from([(name, Some(spent))]),
        });
        assert!(matches!(
            engine
                .lookup_removals(2, 2, &HashMap::new(), &[name], &fork)
                .await,
            Err(NodeError::Consensus(ChiaError::DoubleSpend))
        ));
        let ephemeral = HashMap::from([(name, &addition)]);
        assert!(
            engine
                .lookup_removals(2, 2, &ephemeral, &[name], &fork)
                .await
                .is_ok()
        );
    }
    engine.store.rollback_to(0).await.unwrap();
    engine.stage_coins = Some(StageCoins {
        view: None,
        records: HashMap::from([(name, None)]),
    });
    let before = engine
        .store
        .telemetry()
        .unwrap()
        .coin_reads
        .load(Ordering::Relaxed);
    let found = engine
        .lookup_removals(2, 2, &HashMap::new(), &[name], &fork)
        .await
        .unwrap();
    assert_eq!(found[&name].coin, addition.coin);
    assert_eq!(
        engine
            .store
            .telemetry()
            .unwrap()
            .coin_reads
            .load(Ordering::Relaxed),
        before
    );
    fork.removals.insert(name);
    assert!(matches!(
        engine
            .lookup_removals(2, 2, &HashMap::new(), &[name], &fork)
            .await,
        Err(NodeError::Consensus(ChiaError::DoubleSpendInFork))
    ));
    fork.removals.clear();
    fork.additions.clear();
    assert!(matches!(
        engine
            .lookup_removals(2, 2, &HashMap::new(), &[name], &fork)
            .await,
        Err(NodeError::Consensus(ChiaError::UnknownUnspent))
    ));
    engine.clear_staged_overlay();
    assert!(engine.stage_coins.is_none());
}

#[tokio::test]
async fn prefetch_is_bounded_and_excludes_same_block_ephemeral_coins() {
    let (_directory, mut engine) = engine().await;
    let block = block();
    let (mut conditions, _) =
        run_body_expensive(&NativePrimitives, &MAINNET, &block, &[], false).unwrap();
    let mut template = conditions.spends[0].clone();
    template.create_coin.clear();
    conditions.spends = (0..COIN_PREFETCH_LIMIT + 20)
        .map(|index| {
            let mut spend = template.clone();
            spend.coin_id = record(index as u64, 1).coin.name();
            spend
        })
        .collect();
    let parent = conditions.spends[0].coin_id;
    let child = Coin {
        parent_coin_info: parent,
        puzzle_hash: Bytes32::from([9; 32]),
        amount: 55,
    };
    conditions.spends[0]
        .create_coin
        .insert(dg_xch_core::blockchain::spend::NewCoin {
            puzzle_hash: child.puzzle_hash,
            amount: child.amount,
            hint: None,
        });
    conditions.spends[1].coin_id = child.name();
    conditions.spends[1].parent_id = parent;
    conditions.spends[1].puzzle_hash = child.puzzle_hash;
    conditions.spends[1].coin_amount = child.amount;
    engine
        .preload_stage_context(std::slice::from_ref(&block))
        .await
        .unwrap();
    let bodies = HashMap::from([(
        block.height(),
        PrecomputedBody {
            conds: conditions,
            agg_sig_verified: false,
        },
    )]);
    engine
        .preload_stage_coins(std::slice::from_ref(&block), &bodies)
        .await
        .unwrap();
    let cached = &engine.stage_coins.as_ref().unwrap().records;
    assert!(cached.len() <= COIN_PREFETCH_LIMIT);
    assert!(!cached.contains_key(&child.name()));
    assert!(cached.contains_key(&parent));
    assert!(
        engine
            .store
            .telemetry()
            .unwrap()
            .coin_reads
            .load(Ordering::Relaxed)
            <= COIN_PREFETCH_LIMIT as u64
    );
    engine.confirm_staged_batch(Vec::new()).await.unwrap();
    assert!(engine.stage_coins.is_none());
}
