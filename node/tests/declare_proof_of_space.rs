mod common;

use std::io::Cursor;

use dg_xch_core::blockchain::foliage_block_data::FoliageBlockData;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::reward_chain_block::RewardChainBlock;
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::blockchain::weight_proof::RecentChainData;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::protocols::farmer::DeclareProofOfSpace;
use dg_xch_node::farmer::{
    CandidateIters, CandidatePrev, DeclareVerdict, assemble_candidate, validate_declared_proof,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};

// ─────────────────────────────────────────── shared reconstruction ──────────────────────────────

/// The parts of a real block (`FullBlock` or `HeaderBlock`) needed to reconstruct the
/// DeclareProofOfSpace a farmer would have sent to produce it.
struct BlockView<'a> {
    rcb: &'a RewardChainBlock,
    cc_sp_proof: &'a Option<VdfProof>,
    rc_sp_proof: &'a Option<VdfProof>,
    fbd: &'a FoliageBlockData,
    prev_block_hash: Bytes32,
    prev_transaction_block_hash: Bytes32,
    is_transaction_block: bool,
    timestamp: u64,
    height: u32,
}

impl<'a> BlockView<'a> {
    fn from_full(full: &'a FullBlock) -> Self {
        let (prev_tx_hash, timestamp) = full
            .foliage_transaction_block
            .as_ref()
            .map_or((Bytes32::default(), 0u64), |ftb| {
                (ftb.prev_transaction_block_hash, ftb.timestamp)
            });
        Self {
            rcb: &full.reward_chain_block,
            cc_sp_proof: &full.challenge_chain_sp_proof,
            rc_sp_proof: &full.reward_chain_sp_proof,
            fbd: &full.foliage.foliage_block_data,
            prev_block_hash: full.foliage.prev_block_hash,
            prev_transaction_block_hash: prev_tx_hash,
            is_transaction_block: full.is_transaction_block(),
            timestamp,
            height: full.reward_chain_block.height,
        }
    }

    fn from_header(block: &'a HeaderBlock) -> Self {
        let (prev_tx_hash, timestamp) = block
            .foliage_transaction_block
            .as_ref()
            .map_or((Bytes32::default(), 0u64), |ftb| {
                (ftb.prev_transaction_block_hash, ftb.timestamp)
            });
        Self {
            rcb: &block.reward_chain_block,
            cc_sp_proof: &block.challenge_chain_sp_proof,
            rc_sp_proof: &block.reward_chain_sp_proof,
            fbd: &block.foliage.foliage_block_data,
            prev_block_hash: block.foliage.prev_block_hash,
            prev_transaction_block_hash: prev_tx_hash,
            is_transaction_block: block.foliage_transaction_block.is_some(),
            timestamp,
            height: block.height(),
        }
    }
}

/// The signage point the node's slot state would return for this block's declare — the SAME VDFs the
/// real reward chain block carries. At index > 0 it carries the real cc/rc SP VDFs (so the resolved
/// pospace challenge is `cc_vdf.challenge` = `pos_ss_cc_challenge_hash`, and the reward chain block
/// reassembles byte-identically); at index 0 it is the all-None sub-slot-start SP.
fn reconstruct_signage_point(v: &BlockView) -> SignagePoint {
    if v.rcb.signage_point_index == 0 {
        SignagePoint::sub_slot_start()
    } else {
        SignagePoint {
            cc_vdf: v.rcb.challenge_chain_sp_vdf,
            cc_proof: v.cc_sp_proof.clone(),
            rc_vdf: v.rcb.reward_chain_sp_vdf,
            rc_proof: v.rc_sp_proof.clone(),
        }
    }
}

/// The challenge-chain SP hash a farmer put in `challenge_chain_sp`: the SP's cc-VDF output hash
/// at index > 0, else the sub-slot challenge itself.
fn cc_sp_hash(rcb: &RewardChainBlock) -> Bytes32 {
    match &rcb.challenge_chain_sp_vdf {
        None => rcb.pos_ss_cc_challenge_hash,
        Some(vdf) => vdf.output.hash().expect("cc sp vdf output hashes"),
    }
}

/// The reward-chain SP hash — index > 0 only (the stale-SP guard checks it); index 0 skips the guard.
fn rc_sp_hash(rcb: &RewardChainBlock) -> Bytes32 {
    match &rcb.reward_chain_sp_vdf {
        None => Bytes32::default(),
        Some(vdf) => vdf.output.hash().expect("rc sp vdf output hashes"),
    }
}

/// Synthesize the `DeclareProofOfSpace` a farmer would have sent to produce this block, every
/// field lifted from the real block.
fn synth_declare(v: &BlockView) -> DeclareProofOfSpace {
    let rcb = v.rcb;
    DeclareProofOfSpace {
        authorization: Default::default(),
        // The resolved cc challenge (must equal the SP's cc challenge, which is
        // `pos_ss_cc_challenge_hash`); the genesis check and try_build_candidate's cc-match key off it.
        challenge_hash: rcb.pos_ss_cc_challenge_hash,
        challenge_chain_sp: cc_sp_hash(rcb),
        signage_point_index: rcb.signage_point_index,
        reward_chain_sp: rc_sp_hash(rcb),
        proof_of_space: rcb.proof_of_space.clone(),
        challenge_chain_sp_signature: rcb.challenge_chain_sp_signature,
        reward_chain_sp_signature: rcb.reward_chain_sp_signature,
        farmer_puzzle_hash: v.fbd.farmer_reward_puzzle_hash,
        pool_target: Some(v.fbd.pool_target),
        pool_signature: v.fbd.pool_signature,
        include_signature_source_data: false,
    }
}

/// The pool target `try_build_candidate` would resolve for a non-genesis declare: pool-contract
/// plots pin the pool puzzle hash, OG plots carry the farmer's pool_target.
fn resolved_pool_target(declare: &DeclareProofOfSpace) -> Option<PoolTarget> {
    if let Some(ph) = declare.proof_of_space.pool_contract_puzzle_hash {
        Some(PoolTarget {
            puzzle_hash: ph,
            max_height: 0,
        })
    } else {
        declare.pool_target
    }
}

fn ser<T: ChiaSerialize>(t: &T) -> Vec<u8> {
    t.to_bytes(ChiaProtocolVersion::default())
        .expect("streamable encode")
}

/// The outcome of driving one real block through the declare-side path.
enum Outcome {
    /// Validated (Accepted, quality computed) AND a RequestSignedValues emitted with the reward chain
    /// block byte-identical to the real wire block.
    Proven,
    /// A real defect: the proof did NOT validate, or the emission was wrong. Carries the detail.
    Fail(String),
}

/// Drive a single real block through `validate_declared_proof` (the exact validation
/// `on_declare_proof_of_space` runs) then `assemble_candidate` (the exact emission `try_build_candidate`
/// runs), asserting the full declare → accept → RequestSignedValues + real-reward-chain-block chain.
fn drive(v: &BlockView) -> Outcome {
    let declare = synth_declare(v);
    let sp = reconstruct_signage_point(v);
    let height = v.height;

    let lookup_sp = |cc_sp: &Bytes32| {
        if *cc_sp == declare.challenge_chain_sp {
            Some(sp.clone())
        } else {
            None
        }
    };
    let quality = match validate_declared_proof(&MAINNET, &declare, height, lookup_sp, |_| true) {
        DeclareVerdict::Accepted(q) => q,
        other => {
            return Outcome::Fail(format!(
                "height {height}: real proof of space did NOT validate through the declare path: \
                 verdict={} (expected Accepted)",
                other.result_label()
            ));
        }
    };

    // ── step 2: emission — the SAME call try_build_candidate makes. finished_sub_slots is left empty:
    // it feeds foliage/UnfinishedBlock.finished_sub_slots (pinned byte-for-byte by producer_differential),
    // NOT the reward chain block, which is what we assert identical here. iters carries only the real
    // infusion_point_total_iters (the sole iters field assemble_candidate reads).
    let Some(pool_target) = resolved_pool_target(&declare) else {
        return Outcome::Fail(format!(
            "height {height}: could not resolve a pool target for the declare (OG plot without pool_target)"
        ));
    };
    let iters = CandidateIters {
        required_iters: 0,
        sp_iters: 0,
        ip_iters: 0,
        infusion_point_total_iters: v.rcb.total_iters,
        candidate_sp_total_iters: 0,
    };
    let prev = CandidatePrev {
        is_transaction_block: v.is_transaction_block,
        prev_block_hash: v.prev_block_hash,
        prev_transaction_block_hash: v.prev_transaction_block_hash,
        prev_transaction_block_height: 0,
        reward_claims: Vec::new(),
    };
    let sp_for_block = if declare.signage_point_index == 0 {
        None
    } else {
        Some(&sp)
    };
    let Some((candidate, request)) = assemble_candidate(
        &MAINNET,
        &declare,
        quality,
        sp_for_block,
        Vec::new(),
        &iters,
        height,
        &prev,
        None,
        pool_target,
        declare.farmer_puzzle_hash,
        v.timestamp.max(1),
        v.rcb.pos_ss_cc_challenge_hash,
    ) else {
        return Outcome::Fail(format!(
            "height {height}: assemble_candidate returned None — no RequestSignedValues emitted for an \
             accepted proof"
        ));
    };

    // (b)/(c) RequestSignedValues correctness: it rides the real proof's quality string and the exact
    // foliage hashes of the candidate the node built.
    if request.quality_string != quality {
        return Outcome::Fail(format!(
            "height {height}: RequestSignedValues.quality_string != the validated quality"
        ));
    }
    let fbd_hash = candidate
        .foliage
        .foliage_block_data
        .hash()
        .expect("candidate foliage_block_data hashes");
    if request.foliage_block_data_hash != fbd_hash {
        return Outcome::Fail(format!(
            "height {height}: RequestSignedValues.foliage_block_data_hash != candidate foliage hash"
        ));
    }
    let expected_ftb_hash = candidate
        .foliage
        .foliage_transaction_block_hash
        .unwrap_or_default();
    if request.foliage_transaction_block_hash != expected_ftb_hash {
        return Outcome::Fail(format!(
            "height {height}: RequestSignedValues.foliage_transaction_block_hash mismatch"
        ));
    }

    // The headline real-data tie: the candidate the declare produced carries the proof of space in a
    // RewardChainBlockUnfinished that is BYTE-IDENTICAL to the one on the mainnet wire. Proves the
    // declare → candidate step reconstructs the real block's proof-bearing structure exactly.
    let produced = ser(&candidate.reward_chain_block);
    let expected = ser(&v.rcb.get_unfinished());
    if produced != expected {
        return Outcome::Fail(format!(
            "height {height}: candidate reward_chain_block is NOT byte-identical to the wire block's \
             unfinished reward chain block"
        ));
    }

    Outcome::Proven
}

// ─────────────────────────────────────────── committed FullBlock fixtures ───────────────────

// The two real mainnet TRANSACTION blocks that ship in-repo (also used by producer_differential). Full
// declare → validate (Accepted) → assemble → RequestSignedValues, with the reward chain block proven
// byte-identical to mainnet. Runs with no env.
#[test]
fn declare_from_real_fixture_blocks_validates_and_emits_request_signed_values() {
    let heights = [5_000_000u32, 5_000_004u32];
    let mut proven = 0usize;
    let mut first_failure: Option<String> = None;

    for &h in &heights {
        let full = common::load_full_block(h);
        let v = BlockView::from_full(&full);
        // These fixtures are index > 0 (real signage-point VDFs present) — the common declare case.
        assert!(
            v.rcb.signage_point_index > 0,
            "fixture {h} expected at a normal (index>0) signage point"
        );
        match drive(&v) {
            Outcome::Proven => {
                proven += 1;
                eprintln!(
                    "[PROVEN] height {h}: real proof of space VALIDATED through the declare path; \
                     RequestSignedValues emitted; reward chain block byte-identical to mainnet"
                );
            }
            Outcome::Fail(why) => {
                eprintln!("[FAIL] {why}");
                first_failure.get_or_insert(why);
            }
        }
    }

    assert!(
        first_failure.is_none(),
        "declare-side proof failed on a real mainnet fixture: {first_failure:?}"
    );
    assert_eq!(proven, heights.len(), "every fixture block must be proven");
}

// ─────────────────────────────────────────── committed recent-chain slice ───────────────────

fn load_recent_chain() -> Vec<HeaderBlock> {
    let bytes = include_bytes!("fixtures/recent_chain_mainnet_9054524_9054620.bin");
    RecentChainData::from_bytes(&mut Cursor::new(&bytes[..]), ChiaProtocolVersion::default())
        .expect("recent chain slice deserializes")
        .recent_chain_data
}

// The BROAD committed proof: the whole real mainnet recent-chain slice (heights 9054524..=9054620), the
// same slice required_iters_real_blocks validates pospace against. Every block's proof of space must
// VALIDATE through the declare path and emit a correct RequestSignedValues with a byte-identical reward
// chain block. Runs with no env. A single non-accept is a real declare-handler defect.
#[test]
fn declare_from_recent_chain_slice_validates_every_real_proof() {
    let chain = load_recent_chain();
    assert!(chain.len() > 80, "recent-chain slice present (~85 blocks)");

    let mut proven = 0usize;
    let mut first_failure: Option<String> = None;

    for block in &chain {
        let v = BlockView::from_header(block);
        match drive(&v) {
            Outcome::Proven => proven += 1,
            Outcome::Fail(why) => {
                eprintln!("[FAIL] {why}");
                first_failure.get_or_insert(why);
                break;
            }
        }
    }

    eprintln!(
        "recent-chain tier: {proven}/{} real proofs validated through the declare path and emitted a \
         correct RequestSignedValues (reward chain block byte-identical)",
        chain.len()
    );
    assert!(
        first_failure.is_none(),
        "declare-side proof failed on a real recent-chain block: {first_failure:?}"
    );
    assert_eq!(
        proven,
        chain.len(),
        "every real proof in the slice must validate + emit"
    );
}
