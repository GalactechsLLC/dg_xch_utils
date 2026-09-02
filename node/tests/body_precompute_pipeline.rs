mod common;

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::consensus::block_generator::GeneratorReference;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::protocols::full_node::RespondBlocks;
use dg_xch_node::engine::run_body_expensive;
use dg_xch_node::primitives::NativePrimitives;
use dg_xch_node::sync::precompute_window_bodies_standalone;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

fn load_range(bytes: &[u8]) -> Vec<FullBlock> {
    RespondBlocks::from_bytes(&mut Cursor::new(bytes), ChiaProtocolVersion::default())
        .expect("RespondBlocks deserializes")
        .blocks
}

fn window() -> Vec<FullBlock> {
    load_range(include_bytes!("fixtures/blocks_9179155_9179186.bin"))
}

// Which of the window's tx blocks the standalone path may precompute: generator refs must all
// resolve inside the window (the engine's inline path handles the rest).
fn in_window_resolvable(blocks: &[FullBlock]) -> Vec<u32> {
    let heights: std::collections::HashSet<u32> = blocks.iter().map(FullBlock::height).collect();
    blocks
        .iter()
        .filter(|b| b.is_transaction_block() && b.transactions_generator.is_some())
        .filter(|b| {
            b.transactions_generator_ref_list
                .iter()
                .all(|r| heights.contains(r))
        })
        .map(FullBlock::height)
        .collect()
}

#[test]
fn standalone_precompute_matches_the_inline_engine_path() {
    let blocks = window();
    let resolvable = in_window_resolvable(&blocks);
    assert!(
        !resolvable.is_empty(),
        "the fixture window must contain precomputable tx blocks"
    );

    let pre = precompute_window_bodies_standalone(
        &NativePrimitives,
        &MAINNET,
        0,
        &blocks,
        &std::collections::HashMap::new(),
    );
    assert_eq!(
        {
            let mut got: Vec<u32> = pre.keys().copied().collect();
            got.sort_unstable();
            got
        },
        {
            let mut want = resolvable.clone();
            want.sort_unstable();
            want
        },
        "the standalone path covers exactly the in-window-resolvable tx blocks"
    );

    // Byte-for-byte agreement with the inline computation, signature verification included.
    let by_height: std::collections::HashMap<u32, &FullBlock> =
        blocks.iter().map(|b| (b.height(), b)).collect();
    for h in resolvable {
        let block = by_height[&h];
        let refs: Vec<GeneratorReference> = block
            .transactions_generator_ref_list
            .iter()
            .enumerate()
            .map(|(i, r)| GeneratorReference {
                height: *r,
                index: u32::try_from(i).unwrap_or(u32::MAX),
                generator: by_height[r]
                    .transactions_generator
                    .clone()
                    .expect("ref generator"),
            })
            .collect();
        let (conds, verified) = run_body_expensive(&NativePrimitives, &MAINNET, block, &refs, true)
            .expect("inline body computes");
        let got = &pre[&h];
        assert_eq!(got.conds, conds, "conditions diverge at height {h}");
        assert!(got.agg_sig_verified && verified, "sig verdicts at {h}");
    }
}

#[test]
fn out_of_window_refs_resolve_from_extra_or_are_skipped() {
    let blocks = window();
    // Truncate the window just past a tx block that references an earlier height, so the
    // reference leaves the slice — the standalone path must then produce NO entry for it
    // unless the driver's confirmed-store snapshot (`extra`) serves the ref.
    let mut truncated: Vec<FullBlock> = Vec::new();
    let mut victim: Option<u32> = None;
    for b in &blocks {
        if b.is_transaction_block()
            && b.transactions_generator.is_some()
            && !b.transactions_generator_ref_list.is_empty()
            && !truncated.is_empty()
        {
            // Drop everything before this block: its refs (to earlier heights) now dangle.
            let keep_from = truncated.len();
            truncated = truncated.split_off(keep_from); // empty
            truncated.push(b.clone());
            victim = Some(b.height());
            break;
        }
        truncated.push(b.clone());
    }
    let Some(victim) = victim else {
        // No ref-carrying tx block in this fixture: the property is vacuous here; the
        // resolvability filter is still exercised by the first test.
        return;
    };
    let pre = precompute_window_bodies_standalone(
        &NativePrimitives,
        &MAINNET,
        0,
        &truncated,
        &std::collections::HashMap::new(),
    );
    assert!(
        !pre.contains_key(&victim),
        "a block whose refs neither the window nor extra serve must be skipped for the engine's inline path"
    );

    // With the dangling refs supplied the way the driver does (from the confirmed store), the
    // victim precomputes — byte-for-byte identical to the inline engine path.
    let by_height: std::collections::HashMap<u32, &FullBlock> =
        blocks.iter().map(|b| (b.height(), b)).collect();
    let block = by_height[&victim];
    let extra: std::collections::HashMap<u32, _> = block
        .transactions_generator_ref_list
        .iter()
        .map(|r| {
            (
                *r,
                by_height[r]
                    .transactions_generator
                    .clone()
                    .expect("ref generator"),
            )
        })
        .collect();
    let pre =
        precompute_window_bodies_standalone(&NativePrimitives, &MAINNET, 0, &truncated, &extra);
    let got = pre
        .get(&victim)
        .expect("extra-served refs make the block precomputable");
    let refs: Vec<GeneratorReference> = block
        .transactions_generator_ref_list
        .iter()
        .enumerate()
        .map(|(i, r)| GeneratorReference {
            height: *r,
            index: u32::try_from(i).unwrap_or(u32::MAX),
            generator: by_height[r]
                .transactions_generator
                .clone()
                .expect("ref generator"),
        })
        .collect();
    let (conds, verified) = run_body_expensive(&NativePrimitives, &MAINNET, block, &refs, true)
        .expect("inline body computes");
    assert_eq!(got.conds, conds, "conditions diverge at height {victim}");
    assert!(got.agg_sig_verified && verified, "sig verdicts at {victim}");
}
