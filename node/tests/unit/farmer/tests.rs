use super::*;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
use dg_xch_core::blockchain::sized_bytes::Bytes48;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::constants::MAINNET;

fn vdf(challenge: u8) -> VdfInfo {
    VdfInfo {
        challenge: Bytes32::from([challenge; 32]),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    }
}

fn empty_proof() -> VdfProof {
    VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::default(),
        normalized_to_identity: false,
    }
}

fn sp(cc_challenge: u8) -> SignagePoint {
    SignagePoint {
        cc_vdf: Some(vdf(cc_challenge)),
        cc_proof: Some(empty_proof()),
        rc_vdf: Some(vdf(cc_challenge.wrapping_add(1))),
        rc_proof: Some(empty_proof()),
    }
}

// A proof of space with a pool key present (so verify does not early-reject on the null-pool
// guard) but bogus proof bytes — it reaches, and fails, verify_and_get_quality_string.
fn declare(index: u8, cc_sp: u8, challenge_hash: Bytes32) -> DeclareProofOfSpace {
    DeclareProofOfSpace {
        challenge_hash,
        challenge_chain_sp: Bytes32::from([cc_sp; 32]),
        signage_point_index: index,
        reward_chain_sp: Bytes32::from([cc_sp.wrapping_add(1); 32]),
        proof_of_space: ProofOfSpace {
            version: 0,
            plot_index: 0,
            meta_group: 0,
            strength: 0,
            challenge: Bytes32::from([9; 32]),
            pool_public_key: Some(Bytes48::from([1; 48])),
            pool_contract_puzzle_hash: None,
            plot_public_key: Bytes48::from([2; 48]),
            size: 32,
            proof: ProofBytes::from(vec![0u8; 64]),
        },
        challenge_chain_sp_signature: Default::default(),
        reward_chain_sp_signature: Default::default(),
        farmer_puzzle_hash: Bytes32::from([3; 32]),
        pool_target: None,
        pool_signature: None,
        include_signature_source_data: false,
    }
}

#[test]
fn unknown_signage_point_is_rejected() {
    let d = declare(1, 5, Bytes32::from([7; 32]));
    let v = validate_declared_proof(&MAINNET, &d, 100, |_| None, |_| true);
    assert_eq!(v, DeclareVerdict::UnknownSignagePoint);
}

#[test]
fn stale_signage_point_is_rejected() {
    // index > 0, but the accepted SP's rc output hashes to something other than reward_chain_sp.
    let d = declare(3, 5, Bytes32::from([7; 32]));
    let accepted = sp(40); // rc output is the default element -> its hash != d.reward_chain_sp
    let v = validate_declared_proof(&MAINNET, &d, 100, move |_| Some(accepted.clone()), |_| true);
    assert_eq!(v, DeclareVerdict::StaleSignagePoint);
}

#[test]
fn unknown_sub_slot_is_rejected() {
    // index 0 so the rc-recency check is skipped; non-genesis challenge; sub slot absent.
    let d = declare(0, 5, Bytes32::from([7; 32]));
    let accepted = sp(5);
    let v = validate_declared_proof(
        &MAINNET,
        &d,
        100,
        move |_| Some(accepted.clone()),
        |_| false,
    );
    assert_eq!(v, DeclareVerdict::UnknownSubSlot);
}

#[test]
fn valid_lookup_reaches_pospace_check_and_rejects_bogus_proof() {
    let d = declare(0, 5, Bytes32::from([7; 32]));
    let accepted = sp(5);
    let v = validate_declared_proof(&MAINNET, &d, 100, move |_| Some(accepted.clone()), |_| true);
    assert_eq!(v, DeclareVerdict::InvalidProof);
}

#[test]
fn cc_challenge_hash_follows_the_index() {
    let sp0 = sp(5);
    let d0 = declare(0, 5, Bytes32::from([7; 32]));
    assert_eq!(cc_challenge_hash(&d0, &sp0), d0.challenge_chain_sp);
    let d1 = declare(7, 5, Bytes32::from([7; 32]));
    assert_eq!(
        cc_challenge_hash(&d1, &sp0),
        sp0.cc_vdf.as_ref().unwrap().challenge
    );
}

#[test]
fn index_zero_declare_validates_against_the_sub_slot() {
    // Index-0 path: the all-None sub-slot-start SP resolves, the rc-recency check is skipped,
    // cc_hash == challenge_chain_sp, and the sub-slot presence check passes — reaching (and
    // here failing) the PoSpace verify, never UnknownSignagePoint.
    let d = declare(0, 5, Bytes32::from([7; 32]));
    let sub_slot_start = SignagePoint::sub_slot_start();
    let v = validate_declared_proof(
        &MAINNET,
        &d,
        100,
        move |_| Some(sub_slot_start.clone()),
        |_| true,
    );
    assert_eq!(
        v,
        DeclareVerdict::InvalidProof,
        "index-0 declare reaches the PoSpace verify (bogus proof), not UnknownSignagePoint"
    );
}

#[test]
fn timelord_rc_prev_index_gt_0_uses_sp_reward_chain_challenge() {
    // index > 0: rc_prev = reward_chain_sp_vdf.challenge.
    let rc = vdf(9);
    let got = timelord_rc_prev(
        MAINNET.genesis_challenge,
        5,
        Bytes32::from([1; 32]),
        Some(&rc),
        None,
    );
    assert_eq!(got, Some(rc.challenge));
}

#[test]
fn timelord_rc_prev_index_0_uses_resolved_sub_slot_rc_hash() {
    // index 0 with the pos sub-slot held: rc_prev = that sub-slot's reward-chain hash.
    let resolved = Bytes32::from([44; 32]);
    let got = timelord_rc_prev(
        MAINNET.genesis_challenge,
        0,
        Bytes32::from([1; 32]),
        None,
        Some(resolved),
    );
    assert_eq!(got, Some(resolved));
}

#[test]
fn timelord_rc_prev_index_0_genesis_falls_back_to_genesis_challenge() {
    // index 0, pos sub-slot not held but it IS the genesis challenge: rc_prev = genesis.
    let got = timelord_rc_prev(
        MAINNET.genesis_challenge,
        0,
        MAINNET.genesis_challenge,
        None,
        None,
    );
    assert_eq!(got, Some(MAINNET.genesis_challenge));
}

#[test]
fn timelord_rc_prev_index_0_missing_non_genesis_sub_slot_is_none() {
    // index 0, pos sub-slot unknown and not genesis -> None.
    let got = timelord_rc_prev(
        MAINNET.genesis_challenge,
        0,
        Bytes32::from([1; 32]),
        None,
        None,
    );
    assert_eq!(got, None);
}

#[test]
fn assemble_index_zero_candidate_nulls_the_sp_vdfs() {
    // Index-0: the reward/unfinished block carries no signage VDFs. Passing sp=None threads
    // that through.
    let declare = declare(0, 5, MAINNET.genesis_challenge);
    let prev = CandidatePrev {
        is_transaction_block: true,
        prev_block_hash: MAINNET.genesis_challenge,
        prev_transaction_block_hash: MAINNET.genesis_challenge,
        prev_transaction_block_height: 0,
        reward_claims: Vec::new(),
    };
    let pool_target = PoolTarget {
        puzzle_hash: MAINNET.genesis_pre_farm_pool_puzzle_hash,
        max_height: 0,
    };
    let (block, _request) = assemble_candidate(
        &MAINNET,
        &declare,
        Bytes32::from([42; 32]),
        None, // index-0: sub-slot-start, no signage VDFs
        Vec::new(),
        &iters(),
        0,
        &prev,
        None,
        pool_target,
        MAINNET.genesis_pre_farm_farmer_puzzle_hash,
        1_700_000_000,
        MAINNET.genesis_challenge,
    )
    .expect("index-0 genesis candidate assembles");
    assert_eq!(block.reward_chain_block.signage_point_index, 0);
    assert!(block.reward_chain_block.challenge_chain_sp_vdf.is_none());
    assert!(block.reward_chain_block.reward_chain_sp_vdf.is_none());
    assert!(block.challenge_chain_sp_proof.is_none());
    assert!(block.reward_chain_sp_proof.is_none());
}

#[test]
fn candidate_store_is_bounded_fifo() {
    let mut store = ProofCandidateStore::new(4);
    for i in 0..10u8 {
        store.insert(AcceptedProof {
            declare: declare(0, i, Bytes32::from([7; 32])),
            quality_string: Bytes32::from([i; 32]),
        });
    }
    assert_eq!(store.len(), 4, "bounded to the cap");
    assert!(
        store.get(&Bytes32::from([9; 32])).is_some(),
        "newest retained"
    );
    assert!(
        store.get(&Bytes32::from([0; 32])).is_none(),
        "oldest evicted"
    );
}

// A minimal BlockRecord with only the fields the difficulty selection reads set meaningfully.
fn br(height: u32, weight: u128, sub_slot_iters: u64) -> BlockRecord {
    BlockRecord {
        header_hash: Bytes32::from([height as u8; 32]),
        prev_hash: Bytes32::from([height.wrapping_sub(1) as u8; 32]),
        height,
        weight,
        total_iters: 0,
        signage_point_index: 0,
        challenge_vdf_output: ClassgroupElement::get_default_element(),
        infused_challenge_vdf_output: None,
        reward_infusion_new_challenge: Bytes32::default(),
        challenge_block_info_hash: Bytes32::default(),
        sub_slot_iters,
        pool_puzzle_hash: Bytes32::default(),
        farmer_puzzle_hash: Bytes32::default(),
        required_iters: 1,
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

#[test]
fn difficulty_starting_when_no_peak_or_near_genesis() {
    // No peak, or peak height <= MAX_SUB_SLOT_BLOCKS, uses the starting consts.
    assert_eq!(
        candidate_difficulty_and_ssi(&MAINNET, None, &[]),
        (MAINNET.difficulty_starting, MAINNET.sub_slot_iters_starting)
    );
    let low = br(MAINNET.max_sub_slot_blocks, 1_000, 999);
    assert_eq!(
        candidate_difficulty_and_ssi(&MAINNET, Some((&low, 940)), &[]),
        (MAINNET.difficulty_starting, MAINNET.sub_slot_iters_starting)
    );
}

#[test]
fn difficulty_is_peak_weight_delta_past_the_starting_window() {
    // difficulty = peak.weight - prev.weight; ssi = peak.sub_slot_iters.
    let peak = br(MAINNET.max_sub_slot_blocks + 72, 1_000, 12_345);
    assert_eq!(
        candidate_difficulty_and_ssi(&MAINNET, Some((&peak, 940)), &[]),
        (60, 12_345)
    );
}

#[test]
fn iters_filter_rejects_an_out_of_range_required_iters() {
    // An impossibly hard difficulty pushes required_iters to u64::MAX, so calculate_ip_iters
    // errors (required_iters >= sp_interval_iters) and we bail with no candidate.
    let pos = ProofOfSpace::v1(
        Bytes32::from([9; 32]),
        Some(Bytes48::from([1; 48])),
        None,
        Bytes48::from([2; 48]),
        32,
        ProofBytes::from(vec![0u8; 64]),
    );
    let out = resolve_candidate_iters(
        &MAINNET,
        Bytes32::from([3; 32]),
        &pos,
        u64::MAX,
        MAINNET.sub_slot_iters_starting,
        2,
        Bytes32::from([9; 32]),
        0,
    );
    assert!(out.is_none(), "unfarmable proof must not yield a candidate");
}

// Fixed iters so the assembly tests do not depend on a quality-hash landing in range.
fn iters() -> CandidateIters {
    CandidateIters {
        required_iters: 100,
        sp_iters: 10,
        ip_iters: 20,
        infusion_point_total_iters: 30,
        candidate_sp_total_iters: 10,
    }
}

#[test]
fn assemble_genesis_candidate_is_a_transaction_block() {
    // Genesis: prev_b None, height 0, transaction block, pre-farm targets.
    let declare = declare(2, 5, MAINNET.genesis_challenge);
    let sp = sp(5);
    let prev = CandidatePrev {
        is_transaction_block: true,
        prev_block_hash: MAINNET.genesis_challenge,
        prev_transaction_block_hash: MAINNET.genesis_challenge,
        prev_transaction_block_height: 0,
        reward_claims: Vec::new(),
    };
    let pool_target = PoolTarget {
        puzzle_hash: MAINNET.genesis_pre_farm_pool_puzzle_hash,
        max_height: 0,
    };
    let (block, request) = assemble_candidate(
        &MAINNET,
        &declare,
        Bytes32::from([42; 32]),
        Some(&sp),
        Vec::new(),
        &iters(),
        0,
        &prev,
        None,
        pool_target,
        MAINNET.genesis_pre_farm_farmer_puzzle_hash,
        1_700_000_000,
        MAINNET.genesis_challenge,
    )
    .expect("genesis candidate assembles");

    // Reward block carries the infusion-point iters + the declared SP index/challenge verbatim.
    assert_eq!(block.reward_chain_block.total_iters, 30);
    assert_eq!(block.reward_chain_block.signage_point_index, 2);
    assert_eq!(
        block.reward_chain_block.pos_ss_cc_challenge_hash,
        MAINNET.genesis_challenge
    );
    // Index > 0 keeps the real SP VDFs (not the null-out).
    assert!(block.reward_chain_block.challenge_chain_sp_vdf.is_some());
    assert!(block.reward_chain_block.reward_chain_sp_vdf.is_some());
    // SP signatures come verbatim from the declare; foliage signatures are infinity placeholders.
    assert_eq!(
        block.reward_chain_block.challenge_chain_sp_signature,
        declare.challenge_chain_sp_signature
    );
    assert_eq!(block.foliage.foliage_block_data_signature, g2_infinity());
    // Genesis is a transaction block: foliage carries a transaction block + hash.
    assert!(block.foliage_transaction_block.is_some());
    assert!(block.foliage.foliage_transaction_block_hash.is_some());
    assert_eq!(block.foliage.prev_block_hash, MAINNET.genesis_challenge);
    // RequestSignedValues exposes exactly the two hashes the farmer signs.
    assert_eq!(request.quality_string, Bytes32::from([42; 32]));
    assert_eq!(
        request.foliage_block_data_hash,
        block.foliage.foliage_block_data.hash().unwrap()
    );
    assert_eq!(
        request.foliage_transaction_block_hash,
        block.foliage.foliage_transaction_block_hash.unwrap()
    );
}

#[test]
fn assemble_non_transaction_candidate_has_zeroed_ftb_hash() {
    // A non-transaction candidate builds no foliage transaction block; its hash in
    // RequestSignedValues is zeros.
    let declare = declare(3, 5, Bytes32::from([7; 32]));
    let sp = sp(5);
    let prev = CandidatePrev {
        is_transaction_block: false,
        prev_block_hash: Bytes32::from([1; 32]),
        prev_transaction_block_hash: MAINNET.genesis_challenge,
        prev_transaction_block_height: 0,
        reward_claims: Vec::new(),
    };
    let pool_target = PoolTarget {
        puzzle_hash: Bytes32::from([8; 32]),
        max_height: 0,
    };
    let (block, request) = assemble_candidate(
        &MAINNET,
        &declare,
        Bytes32::from([1; 32]),
        Some(&sp),
        Vec::new(),
        &iters(),
        101,
        &prev,
        None,
        pool_target,
        Bytes32::from([2; 32]),
        1_700_000_000,
        Bytes32::from([9; 32]),
    )
    .expect("non-tx candidate assembles");
    assert!(block.foliage_transaction_block.is_none());
    assert!(block.foliage.foliage_transaction_block_hash.is_none());
    assert_eq!(request.foliage_transaction_block_hash, Bytes32::default());
}

// A normal-index farmer signage point must carry vdf_data (the cc/rc SP-VDF outputs), never
// sub_slot_data, and survive a wire round-trip.
#[test]
fn normal_signage_point_carries_vdf_source_data() {
    use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
    let point = sp(5);
    let out = new_signage_point_for_farmers(&point, Bytes32::from([5u8; 32]), 100, 200, 7, 9, 8)
        .expect("cc/rc VDFs present");
    assert_eq!(out.signage_point_index, 7);
    let src = out
        .sp_source_data
        .as_ref()
        .expect("sp_source_data populated at a normal index");
    assert!(src.vdf_data.is_some(), "a normal index must carry vdf_data");
    assert!(
        src.sub_slot_data.is_none(),
        "a normal index must NOT carry sub_slot_data"
    );
    let v = src.vdf_data.as_ref().unwrap();
    assert_eq!(v.cc_vdf, point.cc_vdf.as_ref().unwrap().output);
    assert_eq!(v.rc_vdf, point.rc_vdf.as_ref().unwrap().output);
    let bytes = out.to_bytes(ChiaProtocolVersion::Chia0_0_37).unwrap();
    let back = NewSignagePoint::from_bytes(
        &mut std::io::Cursor::new(bytes.as_slice()),
        ChiaProtocolVersion::Chia0_0_37,
    )
    .unwrap();
    assert_eq!(back, out);
}
