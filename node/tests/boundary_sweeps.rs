// Geometric-boundary sweeps. Boundary bugs cluster AT boundaries: the fixed-256-window drops at
// peak % 384 >= ~256 and the --sync-from anchor's epoch-retarget wall at 4,575,744. This file
// generalizes the 384-offset sweep (`computed_depth_suffices_for_every_sub_epoch_offset`,
// core difficulty_adjustment tests) into offset-class sweeps {B-1, B, B+1, mid-band} for
// B ∈ {SUB_EPOCH_BLOCKS = 384, EPOCH_BLOCKS = 4608} over each lookback computation on the stage
// path — and, the crown jewel, over the --sync-from anchor's whole first follow window: an anchor
// placed at ANY boundary-offset class must reach a computable state (no NotFound wall) with
// `epoch_backfill_low` coverage, and must reproduce the wall without it.

mod common;

use common::sweep::{
    EPOCH_BOUNDARY, SES_INCLUSION_OFFSET, SUB_EPOCH_BOUNDARY, chain, h32, offset_classes,
    pending_epoch_boundary, plain_ses,
};
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::consensus::difficulty_adjustment::{
    difficulty_record_depth, get_next_sub_slot_iters_and_difficulty,
};
use dg_xch_core::consensus::make_sub_epoch_summary::next_sub_epoch_summary;
use dg_xch_node::sync::{EPOCH_BACKFILL_SLACK, EpochSchedule, epoch_backfill_low};
use std::io::ErrorKind;

fn offset_from(prev_height: u32, boundary: u32) -> i64 {
    i64::from(prev_height) - i64::from(boundary)
}

// Sweep 1a — get_next_sub_slot_iters_and_difficulty at every EPOCH-boundary offset class, with
// realistic geometry: history summaries below the boundary, the boundary's own summary still
// pending. The three boundary-adjacent classes are all epoch-retarget triggers
// (height_can_be_first_in_epoch spans [B, B+384)); the mid-band class takes only the shallow
// sub-epoch walk. Red side: the trigger classes must reproduce the fixed-256-window failure
// while the mid-band class must not.
#[test]
fn ssi_difficulty_walk_computes_at_every_epoch_boundary_offset_class() {
    let e = EPOCH_BOUNDARY;
    for prev_height in offset_classes(e, MAINNET.epoch_blocks) {
        let depth = difficulty_record_depth(&MAINNET, prev_height);
        let blocks = chain(prev_height - depth + 1, prev_height, e);
        let prev = blocks.get(&h32(prev_height)).unwrap();
        let (ssi, difficulty) =
            get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(prev), &blocks)
                .unwrap_or_else(|err| {
                    panic!(
                        "epoch offset {}: computed depth walled: {err}",
                        offset_from(prev_height, e)
                    )
                });
        assert!(ssi > 0 && difficulty > 0);

        let shallow = chain(prev_height - 255, prev_height, e);
        let shallow_result =
            get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(prev), &shallow);
        if prev_height <= e + 1 {
            // An epoch turn cannot fit a fixed 256-record window.
            let err = shallow_result.expect_err("256 records must not cover an epoch turn");
            assert_eq!(err.kind(), ErrorKind::NotFound);
        } else {
            // Mid-band: the sub-epoch walk stays a few records deep — 256 suffices there.
            shallow_result.expect("mid-band must not need the deep window");
        }
    }
}

// Sweep 1b — the same walk at every SUB-EPOCH-boundary offset class in mid-epoch (no epoch turn
// reachable), worst case: the boundary's own summary pending, so the can_finish walk runs its full
// offset depth. Every class must compute inside the per-anchor computed window, and mid-epoch the
// computed depth must stay in the mid-epoch cache band (never the 5,120 epoch-turn depth).
#[test]
fn ssi_difficulty_walk_computes_at_every_sub_epoch_boundary_offset_class() {
    let s = SUB_EPOCH_BOUNDARY;
    for prev_height in offset_classes(s, MAINNET.sub_epoch_blocks) {
        let depth = difficulty_record_depth(&MAINNET, prev_height);
        assert!(
            depth <= MAINNET.sub_epoch_blocks + 4 * MAINNET.max_sub_slot_blocks + 1,
            "mid-epoch offset {}: depth {depth} must stay in the mid-epoch band",
            offset_from(prev_height, s)
        );
        let blocks = chain(prev_height - depth + 1, prev_height, s);
        let prev = blocks.get(&h32(prev_height)).unwrap();
        let (ssi, difficulty) =
            get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(prev), &blocks)
                .unwrap_or_else(|err| {
                    panic!(
                        "sub-epoch offset {}: computed depth walled: {err}",
                        offset_from(prev_height, s)
                    )
                });
        assert!(ssi > 0 && difficulty > 0);
    }
}

// An UnfinishedBlock whose parent is `prev` in the server's reconstruction shape.
// (next_sub_epoch_summary reads only the signage point index, the prev-block hash, the finished
// sub-slots, and total_iters). Non-overflow, no freshly finished sub-slots: the mid-chain steady
// state.
fn unfinished_on(prev: &BlockRecord) -> UnfinishedBlock {
    let full = common::load_full_block(5_000_000);
    let mut reward_chain_block = full.reward_chain_block.get_unfinished();
    reward_chain_block.signage_point_index = 0;
    reward_chain_block.total_iters = prev.total_iters + 10_000_000;
    let mut foliage = full.foliage;
    foliage.prev_block_hash = prev.header_hash;
    UnfinishedBlock {
        finished_sub_slots: Vec::new(),
        reward_chain_block,
        challenge_chain_sp_proof: None,
        reward_chain_sp_proof: None,
        foliage,
        foliage_transaction_block: None,
        transactions_info: None,
        transactions_generator: None,
        transactions_generator_ref_list: Vec::new(),
    }
}

// Sweep 2a — next_sub_epoch_summary at every sub-epoch offset class (mid-epoch, summary pending):
// each class must reach a computable state — Some(summary) chained from the previous inclusion
// block, never a NotFound wall — and mid-epoch no class may declare a retarget. The realistic
// mid-band AFTER the boundary's summary landed must compute None (the pending state has ended).
#[test]
fn next_sub_epoch_summary_computes_at_every_sub_epoch_offset_class() {
    let s = SUB_EPOCH_BOUNDARY;
    for prev_height in offset_classes(s, MAINNET.sub_epoch_blocks) {
        let blocks = chain(prev_height - 1_200, prev_height, s);
        let prev = blocks.get(&h32(prev_height)).unwrap();
        let ses = next_sub_epoch_summary(&MAINNET, &blocks, 1, &unfinished_on(prev), false)
            .unwrap_or_else(|err| {
                panic!(
                    "sub-epoch offset {}: summary walk walled: {err}",
                    offset_from(prev_height, s)
                )
            })
            .expect("a pending sub-epoch boundary must yield the next summary");
        assert_eq!(
            u32::from(ses.num_blocks_overflow),
            SES_INCLUSION_OFFSET,
            "the summary chains from the previous inclusion block"
        );
        assert!(
            ses.new_difficulty.is_none() && ses.new_sub_slot_iters.is_none(),
            "no retarget mid-epoch"
        );
    }

    // The boundary's summary already included (at S + 2): a mid-band stage must see it and yield
    // None — computable, not pending.
    let prev_height = s + MAINNET.sub_epoch_blocks / 2 + 3;
    let blocks = chain(prev_height - 1_200, prev_height, s + 1);
    let prev = blocks.get(&h32(prev_height)).unwrap();
    let ses = next_sub_epoch_summary(&MAINNET, &blocks, 1, &unfinished_on(prev), false)
        .expect("an included summary keeps the walk computable");
    assert!(ses.is_none(), "an included summary ends the pending state");
}

// Sweep 2b — next_sub_epoch_summary at the epoch-turn offset classes: each must declare the
// retargeted values (the summary that carries new_difficulty/new_sub_slot_iters), fed by the same
// deep previous-epoch walk as sweep 1a. Red side: the anchor-span-sized window (256 records) must
// fail the walk with NotFound, never fabricate a summary.
#[test]
fn next_sub_epoch_summary_declares_the_retarget_at_every_epoch_offset_class() {
    let e = EPOCH_BOUNDARY;
    for prev_height in [e - 1, e, e + 1] {
        let blocks = chain(e - 5_400, prev_height, e);
        let prev = blocks.get(&h32(prev_height)).unwrap();
        let ses = next_sub_epoch_summary(&MAINNET, &blocks, 1, &unfinished_on(prev), false)
            .unwrap_or_else(|err| {
                panic!(
                    "epoch offset {}: summary walk walled: {err}",
                    offset_from(prev_height, e)
                )
            })
            .expect("an epoch turn must yield the next summary");
        assert!(
            ses.new_difficulty.is_some() && ses.new_sub_slot_iters.is_some(),
            "epoch offset {}: the turn declares the retargeted values",
            offset_from(prev_height, e)
        );
    }

    let blocks = chain(e - 255, e, e);
    let prev = blocks.get(&h32(e)).unwrap();
    let err = next_sub_epoch_summary(&MAINNET, &blocks, 1, &unfinished_on(prev), false)
        .expect_err("a 256-record window must not cover the epoch-turn summary");
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

// Sweep 3 — the weight-proof-attested epoch schedule at boundary offsets. (The proof's sampling
// internals are crate-private to dg_xch_weight_proof; the consumer-facing boundary surface is
// EpochSchedule — the per-height (ssi, difficulty) resolution whose off-by-one is exactly the
// era-anchor poison class.) Summary k activates at sub-epoch k+1: the values must flip at
// precisely the activation boundary's first block — not one early, not one late — and a
// non-activating sub-epoch boundary must not flip anything.
#[test]
fn epoch_schedule_flips_exactly_at_the_attested_activation_boundaries() {
    let seb = MAINNET.sub_epoch_blocks;
    let mut summaries: Vec<SubEpochSummary> = (0..24).map(|_| plain_ses()).collect();
    summaries[11].new_difficulty = Some(9);
    summaries[11].new_sub_slot_iters = Some(1024);
    summaries[23].new_difficulty = Some(11);
    summaries[23].new_sub_slot_iters = Some(2048);
    let sched = EpochSchedule::from_summaries(&summaries, seb, 128, 7);

    let b1 = 12 * seb; // summary 11 activates at sub-epoch 12 — an epoch boundary (12 * 384 = 4608)
    let b2 = 24 * seb; // summary 23 activates at sub-epoch 24 — the next epoch boundary
    for (h, want) in [
        (b1 - 1, (128, 7)),
        (b1, (1024, 9)),
        (b1 + 1, (1024, 9)),
        (b1 + seb / 2 + 3, (1024, 9)),
        (b2 - 1, (1024, 9)),
        (b2, (2048, 11)),
        (b2 + 1, (2048, 11)),
        (b2 + MAINNET.epoch_blocks / 2 + 3, (2048, 11)),
    ] {
        assert_eq!(sched.at(h), want, "schedule at height {h}");
    }
    for h in [b1 + seb - 1, b1 + seb, b1 + seb + 1] {
        assert_eq!(
            sched.at(h),
            (1024, 9),
            "a non-activating sub-epoch boundary (around {h}) must not flip the schedule"
        );
    }
}

// Sweep 4 — THE CROWN JEWEL: the --sync-from anchor path. For an anchor at EVERY boundary-offset
// class (both boundary periods), the whole first follow window — every stage position from the
// anchor through the pending boundary's full retarget-trigger band, mid-band included — must
// compute against `epoch_backfill_low` coverage: no NotFound wall anywhere. This is the frozen
// sync leg (anchor_epoch_gap's one geometry) generalized to every offset class. Red side: the
// anchor span alone must reproduce the wall at the first
// deep trigger for every class — the "block record not found" line the pod repeated forever.
#[test]
fn sync_from_anchor_reaches_a_computable_state_at_every_boundary_offset_class() {
    let classes = offset_classes(EPOCH_BOUNDARY, MAINNET.epoch_blocks)
        .into_iter()
        .chain(offset_classes(SUB_EPOCH_BOUNDARY, MAINNET.sub_epoch_blocks));
    for anchor in classes {
        let bp = pending_epoch_boundary(anchor);
        let low = epoch_backfill_low(anchor, MAINNET.epoch_blocks, MAINNET.sub_epoch_blocks);
        assert_eq!(
            low,
            bp - MAINNET.epoch_blocks - EPOCH_BACKFILL_SLACK,
            "anchor {anchor}: the backfill floor covers the pending boundary's surpass depth"
        );

        // GREEN: with backfill coverage, every stage position from the anchor through the pending
        // boundary's trigger band computes — the follow can never wall.
        let top = bp + MAINNET.sub_epoch_blocks / 2 + 3;
        let blocks = chain(low, top, bp);
        for prev_height in anchor..=top {
            let prev = blocks.get(&h32(prev_height)).unwrap();
            if let Err(err) =
                get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(prev), &blocks)
            {
                panic!(
                    "anchor {anchor} (pending boundary {bp}): walled at stage position \
                     {prev_height}: {err}"
                );
            }
        }

        // RED: the anchor span alone must fall off the record floor at the first deep trigger.
        let span = chain(anchor - 64, top, bp);
        let first_trigger = anchor.max(bp - 1);
        let prev = span.get(&h32(first_trigger)).unwrap();
        let err = get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(prev), &span)
            .expect_err("the anchor span alone must reproduce the wall");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("block record not found"),
            "anchor {anchor}: unexpected wall error: {err}"
        );
    }
}
