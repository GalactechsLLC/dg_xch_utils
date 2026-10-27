// FIXTURE-VS-LIVE (the strongest proof `unfinished_block_to_full_block` closes the timelord infusion
// loop): take a REAL mainnet `FullBlock`, strip it back to the `(UnfinishedBlock, infusion-point VDFs,
// prev-block context)` a farmer + timelord would have held, then re-finish it through
// `producer::unfinished_block_to_full_block` and assert the reconstruction is BYTE-IDENTICAL to the
// captured mainnet block. If the field derivation (reward-chain infusion, foliage relink, height/weight,
// non-tx nulling) diverged by a single field, the block id would change and this test would go
// red. The blocks are the same `respond_blocks_mainnet_9138873_9138904.bin` capture the wire tests use.
//
// This is fixture-driven, not a live timelord round trip: the infusion VDFs are the ones already inside
// each captured block (decompose → recompose), so no VDF solving runs in-test. The live path is proven
// separately by the full-node emission contract.

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::consensus::producer::unfinished_block_to_full_block;
use dg_xch_core::protocols::full_node::RespondBlocks;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

const RAW: &[u8] = include_bytes!("fixtures/respond_blocks_mainnet_9138873_9138904.bin");

fn decode() -> RespondBlocks {
    let mut cur = Cursor::new(RAW);
    RespondBlocks::from_bytes(&mut cur, ChiaProtocolVersion::default())
        .expect("real mainnet RespondBlocks must decode")
}

// Strip a finished mainnet block back to the unfinished block the farmer signed — the exact inverse of
// `unfinished_block_to_full_block`: drop the three infusion-point VDFs from the reward chain
// (`get_unfinished()`) and their proofs from the block, keep the foliage + transaction payload verbatim.
fn to_unfinished(block: &FullBlock) -> UnfinishedBlock {
    UnfinishedBlock {
        authorization: Default::default(),
        finished_sub_slots: block.finished_sub_slots.clone(),
        reward_chain_block: block.reward_chain_block.get_unfinished(),
        challenge_chain_sp_proof: block.challenge_chain_sp_proof.clone(),
        reward_chain_sp_proof: block.reward_chain_sp_proof.clone(),
        foliage: block.foliage,
        foliage_transaction_block: block.foliage_transaction_block,
        transactions_info: block.transactions_info.clone(),
        transactions_generator: block.transactions_generator.clone(),
        transactions_generator_ref_list: block.transactions_generator_ref_list.clone(),
    }
}

// A minimal previous-block record carrying only the fields `unfinished_block_to_full_block` reads:
// header hash (the foliage prev link), height, and weight. `difficulty` is free — prev.weight is set so
// that `prev.weight + difficulty == block.weight`, so any positive difficulty reconstructs exactly.
fn synthetic_prev(block: &FullBlock, difficulty: u64) -> BlockRecord {
    BlockRecord {
        header_hash: block.foliage.prev_block_hash,
        prev_hash: Bytes32::default(),
        height: block.reward_chain_block.height - 1,
        weight: block.reward_chain_block.weight - u128::from(difficulty),
        total_iters: 0,
        signage_point_index: 0,
        challenge_vdf_output: ClassgroupElement::get_default_element(),
        infused_challenge_vdf_output: None,
        reward_infusion_new_challenge: Bytes32::default(),
        challenge_block_info_hash: Bytes32::default(),
        sub_slot_iters: 0,
        pool_puzzle_hash: Bytes32::default(),
        farmer_puzzle_hash: Bytes32::default(),
        required_iters: 0,
        deficit: 0,
        overflow: false,
        prev_transaction_block_height: 0,
        timestamp: None,
        prev_transaction_block_hash: None,
        fees: None,
        reward_claims_incorporated: None,
        finished_challenge_slot_hashes: None,
        finished_infused_challenge_slot_hashes: None,
        finished_reward_slot_hashes: None,
        sub_epoch_summary_included: None,
    }
}

// Re-finish an unfinished block using the infusion VDFs already inside the original block.
fn refinish(block: &FullBlock, prev: Option<&BlockRecord>, difficulty: u64) -> FullBlock {
    let ub = to_unfinished(block);
    let rcb = &block.reward_chain_block;
    unfinished_block_to_full_block(
        &ub,
        rcb.challenge_chain_ip_vdf,
        block.challenge_chain_ip_proof.clone(),
        rcb.reward_chain_ip_vdf,
        block.reward_chain_ip_proof.clone(),
        rcb.infused_challenge_chain_ip_vdf,
        block.infused_challenge_chain_ip_proof.clone(),
        block.finished_sub_slots.clone(),
        prev,
        rcb.is_transaction_block,
        difficulty,
    )
    .expect("reward block hashes")
}

#[test]
fn every_mainnet_block_reconstructs_byte_identically() {
    let resp = decode();
    let version = ChiaProtocolVersion::default();
    let mut tx_blocks = 0usize;
    let mut non_tx_blocks = 0usize;
    for block in &resp.blocks {
        // These are all non-genesis mainnet blocks (height ~9.13M), so a synthetic prev is supplied.
        let difficulty = 7; // arbitrary; prev.weight absorbs it (see synthetic_prev).
        let prev = synthetic_prev(block, difficulty);
        let rebuilt = refinish(block, Some(&prev), difficulty);

        assert_eq!(
            &rebuilt, block,
            "reconstructed FullBlock at height {} must equal the mainnet original",
            block.reward_chain_block.height
        );
        // The decisive byte-level check: identical streamable ⇒ identical block id.
        assert_eq!(
            rebuilt.to_bytes(version).expect("rebuilt encodes"),
            block.to_bytes(version).expect("original encodes"),
            "byte-identical reconstruction at height {}",
            block.reward_chain_block.height
        );
        assert_eq!(
            rebuilt.header_hash().expect("rebuilt header"),
            block.header_hash().expect("original header"),
            "identical header hash at height {}",
            block.reward_chain_block.height
        );

        if block.reward_chain_block.is_transaction_block {
            tx_blocks += 1;
        } else {
            non_tx_blocks += 1;
        }
    }
    // The capture spans both kinds, so the tx-keep and non-tx-null branches are both exercised.
    assert!(tx_blocks >= 1, "fixture must cover a transaction block");
    assert!(
        non_tx_blocks >= 1,
        "fixture must cover a non-transaction block (the null-foliage branch)"
    );
}

#[test]
fn reconstruction_survives_a_wire_round_trip() {
    // A rebuilt block re-decodes to itself: the reconstruction is not just structurally equal but a
    // valid on-wire FullBlock.
    let resp = decode();
    let version = ChiaProtocolVersion::default();
    let block = &resp.blocks[0];
    let difficulty = 3;
    let prev = synthetic_prev(block, difficulty);
    let rebuilt = refinish(block, Some(&prev), difficulty);
    let bytes = rebuilt.to_bytes(version).expect("encode");
    let back = FullBlock::from_bytes(&mut Cursor::new(bytes.as_slice()), version).expect("decode");
    assert_eq!(back, rebuilt, "rebuilt block round-trips on the wire");
}
