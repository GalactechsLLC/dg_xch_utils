use super::*;
use dg_xch_core::consensus::constants::SIMULATOR;
use dg_xch_core::consensus::overrides::{ConsensusOverrides, apply_overrides};

const K: u8 = 18;
const STRENGTH: u8 = 2;

fn constants() -> ConsensusConstants {
    apply_overrides(
        SIMULATOR,
        &ConsensusOverrides {
            plot_size_v2: Some(K),
            number_zero_bits_plot_filter_v2: Some(0),
            difficulty_constant_factor: Some(2u128.pow(25)),
            difficulty_starting: Some(7),
            discriminant_size_bits: Some(num_bigint::BigInt::from(16)),
            sub_slot_iters_starting: Some(65_536),
            ..Default::default()
        },
    )
}

async fn store() -> dg_xch_stores::SqliteStore {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chain.sqlite");
    std::mem::forget(dir);
    dg_xch_stores::SqliteStore::open(&path)
        .await
        .expect("store")
}

#[tokio::test]
async fn a_multi_block_chain_is_farmed_and_confirmed() {
    let dir = std::env::temp_dir().join("dgxch_sim_chain");
    let _ = std::fs::remove_dir_all(&dir);
    let plots = PlotSet::setup(&dir, 13, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));

    assert!(matches!(
        chain.farm_genesis().await.expect("genesis"),
        AddBlockOutcome::NewPeak { height: 0 }
    ));
    for expected in 1..=3u32 {
        let outcome = chain.farm_next().await.expect("successor");
        // Genesis is NewPeak; a forward extension of the peak reports Extended.
        let height = match outcome {
            AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
            other => panic!("block {expected} was not confirmed onto the peak: {other:?}"),
        };
        assert_eq!(height, expected, "block confirmed at the wrong height");
    }
    assert_eq!(chain.blocks().len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_transaction_block_pays_and_claims_the_reward_coins() {
    use dg_xch_stores::traits::BlockStore;
    let dir = std::env::temp_dir().join("dgxch_sim_txblock");
    let _ = std::fs::remove_dir_all(&dir);
    let plots = PlotSet::setup(&dir, 15, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
    chain.farm_genesis().await.expect("genesis");
    // A transaction block after genesis claims genesis's reward coins.
    let outcome = chain.farm_next_tx().await.expect("tx block");
    let height = match outcome {
        AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
        other => panic!("tx block not confirmed: {other:?}"),
    };
    assert_eq!(height, 1);
    // The confirmed record is a transaction block (carries a timestamp).
    let record = chain
        .engine()
        .store()
        .get_block_record_by_height(1)
        .await
        .expect("store")
        .expect("record at 1");
    assert!(
        record.is_transaction_block(),
        "block 1 is not a transaction block"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_reorg_replaces_the_chain_with_a_heavier_branch() {
    use dg_xch_stores::traits::BlockStore;
    let dir = std::env::temp_dir().join("dgxch_sim_reorg");
    let _ = std::fs::remove_dir_all(&dir);
    let plots = PlotSet::setup(&dir, 17, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
    chain.farm_genesis().await.expect("genesis");
    chain.farm_next().await.expect("b1");
    chain.farm_next().await.expect("b2");
    let (main_tip, main_h) = chain
        .engine()
        .store()
        .get_peak()
        .await
        .expect("peak")
        .expect("has peak");
    assert_eq!(main_h, 2);

    // Fork at height 1 and farm three blocks on a seeded branch, reaching height 4 — heavier
    // than the height-2 incumbent, so the engine reorgs onto it.
    let outcome = chain.reorg(1, 3, 42).await.expect("reorg");
    assert!(
        matches!(
            outcome,
            AddBlockOutcome::Reorg { .. } | AddBlockOutcome::NewPeak { .. }
        ),
        "the branch did not trigger a reorg: {outcome:?}"
    );
    let (new_tip, new_h) = chain
        .engine()
        .store()
        .get_peak()
        .await
        .expect("peak")
        .expect("has peak");
    assert_eq!(new_h, 4, "reorg did not reach the new height");
    assert_ne!(new_tip, main_tip, "the peak did not change");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_chain_crosses_into_a_new_sub_slot() {
    use dg_xch_stores::traits::BlockStore;
    let dir = std::env::temp_dir().join("dgxch_sim_subslot");
    let _ = std::fs::remove_dir_all(&dir);
    let plots = PlotSet::setup(&dir, 21, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
    chain.farm_genesis().await.expect("genesis");
    chain.farm_next().await.expect("b1");
    // Close the sub-slot and open the next, several times over — a chain that outlives a single
    // sub-slot's signage points.
    let mut height = 1u32;
    for _ in 0..4 {
        let outcome = chain.farm_next_slot().await.expect("cross sub-slot");
        height = match outcome {
            AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
            other => panic!("cross-slot block not confirmed: {other:?}"),
        };
        let record = chain
            .engine()
            .store()
            .get_block_record_by_height(height)
            .await
            .expect("store")
            .expect("cross-slot record");
        assert!(
            record.first_in_sub_slot(),
            "the cross-slot block is not first in its sub-slot"
        );
        // The chain keeps growing within the new sub-slot before the next crossing.
        chain.farm_next().await.expect("successor in the new slot");
        height += 1;
    }
    assert_eq!(
        height, 9,
        "expected four crossings each followed by a successor"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Difficulty can outgrow the plot set until whole sub-slots pass with no eligible signage
// point. The chain's shape for that drought is empty finished sub-slots carried by the next
// block; a farm that only ever closes one slot per attempt wedges on the second barren slot
// (a long-running simulator did, once its difficulty had ratcheted for a day). Difficulty 65
// with plot campaign 37 is a pinned fixture: genesis lands, and the drought hits within the
// farmed span.
#[tokio::test]
async fn a_difficulty_drought_farms_through_empty_sub_slots() {
    let dir = std::env::temp_dir().join("dgxch_sim_drought");
    let _ = std::fs::remove_dir_all(&dir);
    let c = apply_overrides(
        constants(),
        &ConsensusOverrides {
            difficulty_starting: Some(65),
            ..Default::default()
        },
    );
    let plots = PlotSet::setup(&dir, 37, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, c, plots, Bytes32::from([0xAB; 32]));
    chain.farm_genesis().await.expect("genesis");
    let mut widest = 0usize;
    let mut after_drought = 0u32;
    for next in 1..=24u32 {
        let outcome = match chain.farm_next().await {
            Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
            other => other,
        }
        .unwrap_or_else(|e| panic!("block {next} wedged the farm: {e}"));
        let height = match outcome {
            AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
            other => panic!("block {next} was not confirmed onto the peak: {other:?}"),
        };
        assert_eq!(height, next, "block confirmed at the wrong height");
        let crossed = chain
            .blocks()
            .last()
            .expect("confirmed block")
            .finished_sub_slots
            .len();
        widest = widest.max(crossed);
        // The point is made once a multi-slot block confirmed and the chain kept growing
        // past it; stop there rather than paying for the full span every run.
        if widest >= 2 {
            after_drought += 1;
            if after_drought >= 3 {
                break;
            }
        }
    }
    assert!(
        widest >= 2,
        "no block carried an empty sub-slot (widest crossing {widest}); the fixture no \
         longer exercises the drought"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// Sub-epoch boundaries at 16 and 32, with the epoch turn out at 64. min_blocks_per_challenge_block
// is 16, so the deficit reaches zero at heights 15 and 31 — the two positions a sub-epoch can
// finish once the next block starts a sub-slot.
fn sub_epoch_constants() -> ConsensusConstants {
    apply_overrides(
        constants(),
        &ConsensusOverrides {
            sub_epoch_blocks: Some(16),
            epoch_blocks: Some(64),
            max_sub_slot_blocks: Some(8),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn a_chain_farms_through_two_sub_epoch_boundaries() {
    use dg_xch_stores::traits::BlockStore;
    let dir = std::env::temp_dir().join("dgxch_sim_subepoch");
    let _ = std::fs::remove_dir_all(&dir);
    let c = sub_epoch_constants();
    let plots = PlotSet::setup(&dir, 23, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, c, plots, Bytes32::from([0xAB; 32]));
    chain.farm_genesis().await.expect("genesis");

    // Cross a sub-slot every fourth block, so a crossing lands on each height the deficit has
    // run down to zero — the summary positions.
    let target = c.sub_epoch_blocks * 2 + 4;
    for next in 1..=target {
        let outcome = if next.is_multiple_of(4) {
            chain.farm_next_slot().await
        } else {
            match chain.farm_next().await {
                Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
                other => other,
            }
        }
        .unwrap_or_else(|e| panic!("block {next}: {e}"));
        let height = match outcome {
            AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
            other => panic!("block {next} was not confirmed onto the peak: {other:?}"),
        };
        assert_eq!(height, next, "block confirmed at the wrong height");
    }

    let mut summaries = Vec::new();
    for height in 0..=target {
        let record = chain
            .engine()
            .store()
            .get_block_record_by_height(height)
            .await
            .expect("store")
            .expect("record");
        if record.sub_epoch_summary_included.is_some() {
            summaries.push(height);
        }
    }
    assert_eq!(
        summaries,
        vec![c.sub_epoch_blocks, c.sub_epoch_blocks * 2],
        "the chain did not include a summary at each sub-epoch boundary"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// The epoch turn at height 64. sub_slot_time_target and slot_blocks_target are set against the
// sim's own cadence — a block every 20 seconds, four to a sub-slot — so the retarget lands at a
// workable multiple of the starting values instead of collapsing the sub-slot.
fn epoch_constants() -> ConsensusConstants {
    apply_overrides(
        sub_epoch_constants(),
        &ConsensusOverrides {
            sub_slot_time_target: Some(num_bigint::BigInt::from(120)),
            slot_blocks_target: Some(4),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn an_epoch_turn_retargets_the_difficulty_and_sub_slot_iters() {
    use dg_xch_stores::traits::BlockStore;
    let dir = std::env::temp_dir().join("dgxch_sim_epoch");
    let _ = std::fs::remove_dir_all(&dir);
    let c = epoch_constants();
    let plots = PlotSet::setup(&dir, 29, 12, K, STRENGTH, false).expect("plots");
    let mut chain = ChainBuilder::new(store().await, c, plots, Bytes32::from([0xAB; 32]));
    chain.farm_genesis().await.expect("genesis");

    // The epoch retarget reads timestamps and weights off transaction blocks, so the chain has
    // to carry them: every other block within a sub-slot is a transaction block.
    let target = c.epoch_blocks + 4;
    for next in 1..=target {
        let outcome = if next.is_multiple_of(4) {
            chain.farm_next_slot().await
        } else if next.is_multiple_of(2) {
            chain.farm_next_tx().await
        } else {
            chain.farm_next().await
        }
        .unwrap_or_else(|e| panic!("block {next}: {e}"));
        let height = match outcome {
            AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
            other => panic!("block {next} was not confirmed onto the peak: {other:?}"),
        };
        assert_eq!(height, next, "block confirmed at the wrong height");
    }

    // The summary at the epoch boundary carries the new epoch values, and the chain runs at them
    // afterwards.
    let turn = chain
        .engine()
        .store()
        .get_block_record_by_height(c.epoch_blocks)
        .await
        .expect("store")
        .expect("epoch record");
    let ses = turn
        .sub_epoch_summary_included
        .expect("the epoch boundary block includes a sub-epoch summary");
    assert_eq!(ses.new_sub_slot_iters, Some(turn.sub_slot_iters));
    assert_ne!(
        turn.sub_slot_iters, c.sub_slot_iters_starting,
        "the epoch turn did not retarget the sub-slot iterations"
    );
    let new_difficulty = ses
        .new_difficulty
        .expect("the epoch summary carries a new difficulty");
    assert_ne!(
        new_difficulty, c.difficulty_starting,
        "the epoch turn did not retarget the difficulty"
    );

    let tip = chain
        .engine()
        .store()
        .get_block_record_by_height(target)
        .await
        .expect("store")
        .expect("tip record");
    assert_eq!(
        tip.sub_slot_iters, turn.sub_slot_iters,
        "the new sub-slot iterations did not carry forward"
    );
    let prev = chain
        .engine()
        .store()
        .get_block_record_by_height(target - 1)
        .await
        .expect("store")
        .expect("record below the tip");
    assert_eq!(
        tip.weight - prev.weight,
        u128::from(new_difficulty),
        "the new difficulty did not carry forward"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_transaction_block_spends_a_reward_coin() {
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::blockchain::coin_spend::CoinSpend;
    use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
    use dg_xch_core::blockchain::sized_bytes::Bytes96;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::consensus::coinbase::create_farmer_coin;
    use dg_xch_stores::traits::CoinStore;

    let dir = std::env::temp_dir().join("dgxch_sim_spend");
    let _ = std::fs::remove_dir_all(&dir);
    let plots = PlotSet::setup(&dir, 19, 12, K, STRENGTH, false).expect("plots");
    // Reward coins pay the `1` identity puzzle, so the farmer reward is spendable: run with the
    // solution as its condition list.
    let identity_ph = Program::to(1_u8).tree_hash();
    let mut chain = ChainBuilder::new(store().await, constants(), plots, identity_ph);
    chain.farm_genesis().await.expect("genesis");
    // A transaction block claims genesis's rewards, creating the farmer reward coin at height 1.
    chain.farm_next_tx().await.expect("tx block");

    // Genesis's farmer reward coin (its 1/8 of the pre-farm), created at height 1 by the claim.
    let genesis = chain.constants().genesis_challenge;
    let spendable = create_farmer_coin(0, identity_ph, calculate_base_farmer_reward(0), genesis);
    assert!(
        chain
            .engine()
            .store()
            .get_coin_record(&spendable.name())
            .await
            .expect("store")
            .is_some_and(|r| !r.spent),
        "the reward coin is not a spendable coin in the store"
    );

    // Spend the reward coin in full to a fresh puzzle hash.
    let out_ph = Bytes32::from([0x77; 32]);
    let puzzle = Program::to(1_u8);
    let solution = Program::to(vec![
        SExp::from(&ConditionWithArgs::CreateCoin(
            out_ph,
            spendable.amount,
            vec![],
        ))
        .to_owned(),
    ]);
    let mut infinity = [0u8; 96];
    infinity[0] = 0xc0;
    let bundle = SpendBundle {
        coin_spends: vec![CoinSpend {
            coin: spendable,
            puzzle_reveal: puzzle.serialized().expect("puzzle"),
            solution: solution.serialized().expect("solution"),
        }],
        aggregated_signature: Bytes96::from(infinity),
    };
    chain
        .farm_next_tx_with_bundle(bundle)
        .await
        .expect("spend block");

    // The reward coin is spent and the created coin is present and unspent.
    let spent = chain
        .engine()
        .store()
        .get_coin_record(&spendable.name())
        .await
        .expect("store")
        .expect("reward coin record");
    assert!(spent.spent, "the reward coin was not spent");
    let created = Coin {
        parent_coin_info: spendable.name(),
        puzzle_hash: out_ph,
        amount: spendable.amount,
    };
    let created = chain
        .engine()
        .store()
        .get_coin_record(&created.name())
        .await
        .expect("store")
        .expect("created coin record");
    assert!(!created.spent, "the created coin should be unspent");
    assert_eq!(created.coin.puzzle_hash, out_ph);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_node_syncs_the_farmed_chain() {
    use dg_xch_node::sync::BlockRangeSource;
    use dg_xch_node::{Chaser, SyncConfig, SyncError};
    use dg_xch_stores::traits::BlockStore;
    use std::collections::HashMap;
    use std::sync::Arc;

    // A block source over an in-memory height -> block map.
    struct FarmedSource {
        blocks: HashMap<u32, FullBlock>,
    }
    #[async_trait::async_trait]
    impl BlockRangeSource for FarmedSource {
        fn peer_id(&self) -> u64 {
            1
        }
        fn is_closed(&self) -> bool {
            false
        }
        async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
            Ok((start..=end)
                .filter_map(|h| self.blocks.get(&h).cloned())
                .collect())
        }
    }

    // Producer: farm a chain, capturing every block and the reward coins genesis created.
    let dir = std::env::temp_dir().join("dgxch_sim_sync");
    let _ = std::fs::remove_dir_all(&dir);
    let plots = PlotSet::setup(&dir, 21, 12, K, STRENGTH, false).expect("plots");
    let mut producer =
        ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
    producer.farm_genesis().await.expect("genesis");
    producer.farm_next().await.expect("successor");
    // Cross a sub-slot so the corpus carries a block with a finished sub-slot bundle: a fresh
    // node must validate the end-of-slot VDFs on sync, not just within-slot infusions.
    producer.farm_next_slot().await.expect("cross sub-slot");
    producer
        .farm_next()
        .await
        .expect("successor in the new slot");
    let corpus: HashMap<u32, FullBlock> = producer
        .blocks()
        .iter()
        .map(|b| (b.reward_chain_block.height, b.clone()))
        .collect();
    let last = *corpus.keys().max().expect("nonempty");

    // Consumer: a fresh node with an empty store follows the same blocks through the sync
    // pipeline, window by window.
    let consumer_store = Arc::new(store().await);
    let engine = Engine::new(consumer_store.clone(), NativePrimitives, constants());
    let mut chaser = Chaser::new(engine, SyncConfig::default());
    let source: Arc<dyn BlockRangeSource> = Arc::new(FarmedSource { blocks: corpus });
    let mut next = 0u32;
    while next <= last {
        let to = last.min(next + 31);
        chaser
            .follow_to(&source, next, to)
            .await
            .expect("sync window");
        next = to + 1;
    }

    // The synced node reached the producer's peak and holds the same block at every height.
    let producer_store = producer.engine().store();
    let producer_peak = producer_store.get_peak().await.expect("producer peak");
    let consumer_peak = consumer_store.get_peak().await.expect("consumer peak");
    assert_eq!(
        consumer_peak, producer_peak,
        "synced peak differs from the producer"
    );
    assert_eq!(
        consumer_peak.map(|(_, h)| h),
        Some(last),
        "did not sync to the tip"
    );
    for height in 0..=last {
        let p = producer_store
            .get_block_record_by_height(height)
            .await
            .expect("producer record");
        let cs = consumer_store
            .get_block_record_by_height(height)
            .await
            .expect("consumer record");
        assert_eq!(
            p.map(|r| r.header_hash),
            cs.map(|r| r.header_hash),
            "records differ at height {height}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
