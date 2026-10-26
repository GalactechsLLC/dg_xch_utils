// Full single-block PoW/VDF header validation outside the weight-proof path. A real mainnet recent
// chain (heights 9054524..=9054620, sliced from the committed weight proof) is rebuilt into block records,
// then target blocks are validated through the node's PrimitiveVerifier — the derived required_iters must
// equal the reference the weight-proof path produces for the same block.

use std::collections::HashMap;
use std::io::Cursor;

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::weight_proof::RecentChainData;
use dg_xch_core::consensus::block_header_validation::{
    ValidationState, validate_pospace_and_get_required_iters,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::consensus::deficit::calculate_deficit;
use dg_xch_core::consensus::full_block_to_block_record::header_block_to_sub_block_record;
use dg_xch_core::consensus::get_block_challenge::pre_sp_tx_block_height;
use dg_xch_core::consensus::pot_iterations::is_overflow_block;
use dg_xch_node::{NativePrimitives, PrimitiveVerifier, validate_finished_header};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};

// Golden ssi/difficulty for the sliced region (no sub-epoch boundary inside it), and per-height reference
// required_iters — both produced by the weight-proof recent-block validator over the full committed proof.
const SSI: u64 = 574_619_648;
const DIFF: u64 = 2608;
const GOLDEN: &[(u32, u64)] = &[(9_054_611, 2_155_695), (9_054_612, 7_701_062)];

fn load_chain() -> Vec<HeaderBlock> {
    let bytes = include_bytes!("fixtures/recent_chain_mainnet_9054336_9054620.bin");
    RecentChainData::from_bytes(&mut Cursor::new(&bytes[..]), ChiaProtocolVersion::default())
        .expect("recent chain slice deserializes")
        .recent_chain_data
}

// Light-path (proof-of-space only, no VDF) required_iters — used to seed ancestor records so a target's
// full validation reads a correct pb.ip_iters.
fn light_required_iters(
    ancestors: &HashMap<Bytes32, BlockRecord>,
    block: &HeaderBlock,
    challenge: Bytes32,
    prev_challenge: Bytes32,
    overflow: bool,
) -> u64 {
    let rcb = &block.reward_chain_block;
    let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
        None => challenge,
        Some(v) => v.output.hash().expect("cc sp hash"),
    };
    let pre = pre_sp_tx_block_height(
        &MAINNET,
        ancestors,
        block.prev_header_hash(),
        rcb.signage_point_index,
        block.finished_sub_slots.len(),
    )
    .expect("pre_sp_tx_block_height");
    validate_pospace_and_get_required_iters(
        &PrimitiveVerifier(&NativePrimitives),
        &MAINNET,
        &rcb.proof_of_space,
        if overflow { prev_challenge } else { challenge },
        cc_sp_hash,
        block.height(),
        DIFF,
        pre,
    )
    .expect("pospace")
    .expect("valid pospace")
}

// Rebuild block records across the slice (light path), returning the ancestor map keyed by header hash.
fn build_ancestors(chain: &[HeaderBlock]) -> HashMap<Bytes32, BlockRecord> {
    let c = &MAINNET;
    let mut ancestors: HashMap<Bytes32, BlockRecord> = HashMap::new();
    let mut challenge = Some(chain[0].reward_chain_block.pos_ss_cc_challenge_hash);
    let mut prev_challenge: Option<Bytes32> = None;
    let mut prev_rec: Option<BlockRecord> = None;
    let mut deficit: u8 = 0;
    let mut tx_blocks: u32 = 0;
    let mut prev_tx_height = 0;

    for block in chain {
        let rcb = &block.reward_chain_block;
        let h = block.height();
        let mut overflow = false;
        let mut ses = None;
        for ss in &block.finished_sub_slots {
            prev_challenge = Some(ss.challenge_chain.challenge_chain_end_of_slot_vdf.challenge);
            challenge = Some(ss.challenge_chain.hash().expect("cc hash"));
            deficit = ss.reward_chain.deficit;
            if let Some(ses_hash) = ss.challenge_chain.subepoch_summary_hash {
                ses = Some(
                    dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary {
                        prev_subepoch_summary_hash: ses_hash,
                        reward_chain_hash: ses_hash,
                        num_blocks_overflow: 0,
                        new_difficulty: None,
                        new_sub_slot_iters: None,
                    },
                );
            }
        }
        let mut required_iters = 0u64;
        if let (Some(ch), Some(pc)) = (challenge, prev_challenge)
            && tx_blocks > 2
        {
            overflow = is_overflow_block(c, rcb.signage_point_index).expect("overflow");
            deficit = calculate_deficit(
                c,
                h,
                prev_rec.as_ref(),
                overflow,
                block.finished_sub_slots.len(),
            );
            required_iters = light_required_iters(&ancestors, block, ch, pc, overflow);
        }
        let rec = header_block_to_sub_block_record(
            c,
            required_iters,
            block,
            SSI,
            overflow,
            deficit,
            prev_tx_height,
            ses,
        )
        .expect("record");
        ancestors.insert(rec.header_hash, rec.clone());
        if rcb.is_transaction_block {
            tx_blocks += 1;
            prev_tx_height = h;
        }
        prev_rec = Some(rec);
    }
    ancestors
}

#[test]
fn validates_mainnet_header_outside_weight_proof_with_reference_required_iters() {
    let chain = load_chain();
    assert!(chain.len() > 80, "slice present");
    let ancestors = build_ancestors(&chain);

    let by_height: HashMap<u32, &HeaderBlock> = chain.iter().map(|b| (b.height(), b)).collect();
    let vs = ValidationState {
        ssi: SSI,
        difficulty: DIFF,
    };

    for &(height, expected) in GOLDEN {
        let block = by_height.get(&height).expect("target in slice");
        // The node's promoted validator, driven by the ConsensusPrimitives-backed verifier — not the
        // weight-proof loop. check_sub_epoch_summary=false: the slice carries no sub-epoch boundary.
        let required_iters =
            validate_finished_header(&NativePrimitives, &MAINNET, &ancestors, block, vs, false)
                .unwrap_or_else(|e| panic!("validate header at {height}: {e}"));
        assert_eq!(
            required_iters, expected,
            "required_iters at {height} matches the weight-proof reference"
        );
    }
}

#[test]
fn unfinished_mainnet_overflow_accepts_missing_eos_but_finished_validation_stays_strict() {
    use dg_xch_core::blockchain::unfinished_header_block::UnfinishedHeaderBlock;
    use dg_xch_core::consensus::block_header_validation::validate_unfinished_header_block;
    let chain = load_chain();
    let ancestors = build_ancestors(&chain);
    let vs = ValidationState {
        ssi: SSI,
        difficulty: DIFF,
    };
    let mut missing = 0;
    let mut ordinary = 0;
    for block in chain.iter().filter(|b| b.height() >= 9_054_550) {
        let overflow =
            is_overflow_block(&MAINNET, block.reward_chain_block.signage_point_index).unwrap();
        let expected =
            validate_finished_header(&NativePrimitives, &MAINNET, &ancestors, block, vs, false)
                .unwrap();
        let mut header = UnfinishedHeaderBlock {
            finished_sub_slots: block.finished_sub_slots.clone(),
            reward_chain_block: block.reward_chain_block.get_unfinished(),
            challenge_chain_sp_proof: block.challenge_chain_sp_proof.clone(),
            reward_chain_sp_proof: block.reward_chain_sp_proof.clone(),
            foliage: block.foliage,
            foliage_transaction_block: block.foliage_transaction_block,
            transactions_filter: Default::default(),
        };
        if overflow && !header.finished_sub_slots.is_empty() {
            header.finished_sub_slots.pop();
            missing += 1;
            let mut incomplete_finished = block.clone();
            incomplete_finished.finished_sub_slots = header.finished_sub_slots.clone();
            assert!(
                validate_finished_header(
                    &NativePrimitives,
                    &MAINNET,
                    &ancestors,
                    &incomplete_finished,
                    vs,
                    false
                )
                .is_err()
            );
        } else if !overflow {
            ordinary += 1;
        }
        assert_eq!(
            validate_unfinished_header_block(
                &MAINNET,
                &PrimitiveVerifier(&NativePrimitives),
                &ancestors,
                &header,
                vs,
                false
            )
            .unwrap_or_else(|e| panic!("height={} overflow={overflow}: {e}", block.height())),
            expected
        );
        if overflow {
            let mut invalid_signature = header.clone();
            invalid_signature
                .reward_chain_block
                .reward_chain_sp_signature = Default::default();
            assert!(
                validate_unfinished_header_block(
                    &MAINNET,
                    &PrimitiveVerifier(&NativePrimitives),
                    &ancestors,
                    &invalid_signature,
                    vs,
                    false
                )
                .unwrap_err()
                .to_string()
                .contains("INVALID_RC_SIGNATURE")
            );
            let mut invalid_vdf = header.clone();
            invalid_vdf
                .challenge_chain_sp_proof
                .as_mut()
                .unwrap()
                .witness = Default::default();
            assert!(
                validate_unfinished_header_block(
                    &MAINNET,
                    &PrimitiveVerifier(&NativePrimitives),
                    &ancestors,
                    &invalid_vdf,
                    vs,
                    false
                )
                .is_err()
            );
        }
        let mut invalid = header.clone();
        invalid.reward_chain_block.pos_ss_cc_challenge_hash = Bytes32::default();
        assert!(
            validate_unfinished_header_block(
                &MAINNET,
                &PrimitiveVerifier(&NativePrimitives),
                &ancestors,
                &invalid,
                vs,
                false
            )
            .unwrap_err()
            .to_string()
            .contains("INVALID_CC_CHALLENGE")
        );
    }
    assert!(missing > 0, "fixture must cover the missing EOS case");
    assert!(ordinary > 0);
}

#[test]
fn unfinished_overflow_detects_boundary_already_carried_by_ancestor() {
    use dg_xch_core::consensus::get_block_challenge::final_eos_is_already_included;
    let chain = load_chain();
    let ancestors = build_ancestors(&chain);
    let previous = ancestors
        .values()
        .find(|b| b.overflow && b.first_in_sub_slot())
        .unwrap();
    let total = previous.total_iters + 1;
    assert!(
        final_eos_is_already_included(0, previous.header_hash, total, SSI, &ancestors).unwrap()
    );
    assert!(
        !final_eos_is_already_included(1, previous.header_hash, total, SSI, &ancestors).unwrap()
    );
    assert!(
        !final_eos_is_already_included(
            0,
            previous.header_hash,
            previous.total_iters + u128::from(SSI / 2),
            SSI,
            &ancestors
        )
        .unwrap()
    );
    assert!(
        !final_eos_is_already_included(
            0,
            previous.header_hash,
            previous.total_iters - 1,
            SSI,
            &ancestors
        )
        .unwrap()
    );
    assert!(final_eos_is_already_included(0, Bytes32::default(), total, SSI, &ancestors).is_err());
}

#[test]
fn timelord_slot_context_recovers_from_validated_slots_and_branch_records() {
    use dg_xch_node::farmer::validated_slot_reward_hash;
    let chain = load_chain();
    let records = build_ancestors(&chain);
    let block = chain
        .iter()
        .rev()
        .find(|b| !b.finished_sub_slots.is_empty())
        .unwrap();
    let slot = block.finished_sub_slots.last().unwrap();
    let challenge = slot.challenge_chain.hash().unwrap();
    let reward = slot.reward_chain.hash().unwrap();
    assert_eq!(
        validated_slot_reward_hash(challenge, &block.finished_sub_slots, None, &records),
        Some(reward)
    );
    let record = records.get(&block.header_hash().unwrap()).unwrap();
    assert_eq!(
        validated_slot_reward_hash(challenge, &[], Some(record), &records),
        Some(reward)
    );
    // No ancestry means no authority to borrow another branch's slot context.
    assert_eq!(
        validated_slot_reward_hash(challenge, &[], None, &records),
        None
    );
    assert_eq!(
        validated_slot_reward_hash(Bytes32::default(), &[], Some(record), &records),
        None
    );
    let mut malformed = record.clone();
    malformed.finished_reward_slot_hashes = Some(vec![]);
    assert_eq!(
        validated_slot_reward_hash(challenge, &[], Some(&malformed), &records),
        None
    );
}
