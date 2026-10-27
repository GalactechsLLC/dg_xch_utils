use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::slots::SlotState;
use std::collections::HashMap;

fn proof() -> VdfProof {
    VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::from([0_u8; 33].as_slice()),
        normalized_to_identity: false,
    }
}

fn vdf(challenge: Bytes32, iterations: u64) -> VdfInfo {
    VdfInfo {
        challenge,
        number_of_iterations: iterations,
        output: ClassgroupElement::get_default_element(),
    }
}

// An empty-slot EOS chaining onto `cc_challenge`/`rc_challenge` with the full starting
// sub-slot iters (the shape a real peer gossips during a quiet slot, minus valid proofs).
fn empty_slot_eos(cc_challenge: Bytes32, rc_challenge: Bytes32) -> EndOfSubSlotBundle {
    let cc_vdf = vdf(cc_challenge, MAINNET.sub_slot_iters_starting);
    let cc = ChallengeChainSubSlot {
        challenge_chain_end_of_slot_vdf: cc_vdf,
        infused_challenge_chain_sub_slot_hash: None,
        subepoch_summary_hash: None,
        new_sub_slot_iters: None,
        new_difficulty: None,
    };
    EndOfSubSlotBundle {
        reward_chain: RewardChainSubSlot {
            end_of_slot_vdf: vdf(rc_challenge, MAINNET.sub_slot_iters_starting),
            challenge_chain_sub_slot_hash: cc.hash().expect("cc hash"),
            infused_challenge_chain_sub_slot_hash: None,
            deficit: MAINNET.min_blocks_per_challenge_block,
        },
        challenge_chain: cc,
        infused_challenge_chain: None,
        proofs: SubSlotProofs {
            challenge_chain_slot_proof: proof(),
            infused_challenge_chain_slot_proof: None,
            reward_chain_slot_proof: proof(),
        },
    }
}

fn sp_at(index: u8, cc_challenge: Bytes32, rc_challenge: Bytes32) -> SignagePoint {
    let checkpoint = MAINNET.sub_slot_iters_starting / u64::from(MAINNET.num_sps_sub_slot);
    let delta = checkpoint * u64::from(index);
    SignagePoint {
        cc_vdf: Some(vdf(cc_challenge, delta)),
        cc_proof: Some(proof()),
        rc_vdf: Some(vdf(rc_challenge, delta)),
        rc_proof: Some(proof()),
    }
}

#[test]
fn genesis_slot_is_initialized() {
    let state = SlotState::new(MAINNET);
    assert_eq!(state.slot_count(), 1);
}

#[test]
fn eos_with_wrong_challenge_rejected_and_never_cached() {
    let mut state = SlotState::new(MAINNET);
    let eos = empty_slot_eos(Bytes32::from([9; 32]), Bytes32::from([9; 32]));
    let blocks = HashMap::new();
    assert!(
        state
            .new_finished_sub_slot(&eos, &blocks, None, 0, 0, true)
            .is_none(),
        "an EOS that does not chain onto our next slot must be rejected"
    );
    assert_eq!(state.slot_count(), 1);
}

#[test]
fn empty_slot_eos_chains_from_genesis_and_is_idempotent() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let eos = empty_slot_eos(MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    assert!(
        state
            .new_finished_sub_slot(&eos, &blocks, None, 0, 0, true)
            .is_some()
    );
    assert_eq!(state.slot_count(), 2);
    // Same bundle again: idempotent accept, no duplicate slot.
    assert!(
        state
            .new_finished_sub_slot(&eos, &blocks, None, 0, 0, true)
            .is_some()
    );
    assert_eq!(state.slot_count(), 2);
    // The appended slot is findable by its cc hash.
    let cc_hash = eos.challenge_chain.hash().expect("hash");
    assert!(state.get_sub_slot(&cc_hash).is_some());
}

#[test]
fn second_empty_slot_must_chain_onto_the_first() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let first = empty_slot_eos(MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    state
        .new_finished_sub_slot(&first, &blocks, None, 0, 0, true)
        .expect("first slot");
    // A bundle still chaining from genesis no longer appends.
    let mut stale = empty_slot_eos(MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    stale.challenge_chain.new_sub_slot_iters = None;
    stale.reward_chain.deficit = MAINNET.min_blocks_per_challenge_block - 1; // differ from `first`
    assert!(
        state
            .new_finished_sub_slot(&stale, &blocks, None, 0, 0, true)
            .is_none()
    );
    // One chaining onto the first slot's cc hash does.
    let second = empty_slot_eos(
        first.challenge_chain.hash().expect("cc"),
        first.reward_chain.hash().expect("rc"),
    );
    assert!(
        state
            .new_finished_sub_slot(&second, &blocks, None, 0, 0, true)
            .is_some()
    );
    assert_eq!(state.slot_count(), 3);
}

#[test]
fn empty_slot_eos_with_ses_rejected() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let mut eos = empty_slot_eos(MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    eos.challenge_chain.subepoch_summary_hash = Some(Bytes32::from([7; 32]));
    assert!(
        state
            .new_finished_sub_slot(&eos, &blocks, None, 0, 0, true)
            .is_none(),
        "an empty slot can never carry a sub-epoch summary"
    );
}

#[test]
fn empty_slot_eos_with_icc_rejected() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let mut eos = empty_slot_eos(MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    eos.reward_chain.infused_challenge_chain_sub_slot_hash = Some(Bytes32::from([7; 32]));
    assert!(
        state
            .new_finished_sub_slot(&eos, &blocks, None, 0, 0, true)
            .is_none(),
        "the chain's first empty slot has no infused challenge chain"
    );
}

#[test]
fn signage_point_index_bounds() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let sp = sp_at(1, MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    assert!(!state.new_signage_point(0, &blocks, None, 0, &sp, true));
    let oob = u8::try_from(MAINNET.num_sps_sub_slot).expect("fits");
    assert!(!state.new_signage_point(oob, &blocks, None, 0, &sp, true));
}

#[test]
fn signage_point_cached_in_genesis_slot_and_retrievable() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let sp = sp_at(3, MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    assert!(state.new_signage_point(3, &blocks, None, 0, &sp, true));
    let key = sp
        .cc_vdf
        .as_ref()
        .expect("cc_vdf present")
        .output
        .hash()
        .expect("output hash");
    assert_eq!(state.get_signage_point(&key).as_ref(), Some(&sp));
}

#[test]
fn signage_point_with_unknown_challenge_not_cached_in_slots() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    let sp = sp_at(3, Bytes32::from([9; 32]), Bytes32::from([9; 32]));
    assert!(!state.new_signage_point(3, &blocks, None, 0, &sp, true));
    let key = sp
        .cc_vdf
        .as_ref()
        .expect("cc_vdf present")
        .output
        .hash()
        .expect("output hash");
    assert!(state.get_signage_point(&key).is_none());
}

#[test]
fn signage_point_claimed_iters_must_match_its_index() {
    let mut state = SlotState::new(MAINNET);
    let blocks = HashMap::new();
    // Claims index 3 but states index 4's delta iters.
    let sp = sp_at(4, MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    assert!(!state.new_signage_point(3, &blocks, None, 0, &sp, true));
}

#[test]
fn get_signage_point_returns_sub_slot_start_for_genesis_challenge() {
    // cc_signage_point == GENESIS_CHALLENGE -> the all-None sub-slot-start SP.
    // This is the signage-point-index-0 SP; the node must serve it, not None.
    let state = SlotState::new(MAINNET);
    let sp = state
        .get_signage_point(&MAINNET.genesis_challenge)
        .expect("index-0 SP served for the genesis challenge");
    assert!(sp.is_sub_slot_start());
}

#[test]
fn get_signage_point_none_for_unknown_challenge() {
    // A challenge that is neither the genesis challenge nor any held sub-slot cc hash, and
    // matches no stored SP, resolves to None.
    let state = SlotState::new(MAINNET);
    assert!(state.get_signage_point(&Bytes32::from([200; 32])).is_none());
}

#[test]
fn rpc_history_survives_active_slot_changes_and_clears_on_reorg() {
    use dg_xch_core::blockchain::block_record::BlockRecord;
    use dg_xch_node::slots::PeakSlotContext;
    let mut state = SlotState::new(MAINNET);
    let first = empty_slot_eos(MAINNET.genesis_challenge, MAINNET.genesis_challenge);
    let first_hash = first.challenge_chain.hash().unwrap();
    state
        .new_finished_sub_slot(&first, &HashMap::new(), None, 0, 0, true)
        .unwrap();
    let first_time = state.received_time(&first_hash).unwrap();
    let second = empty_slot_eos(first_hash, first.reward_chain.hash().unwrap());
    let second_hash = second.challenge_chain.hash().unwrap();
    let mut peak: BlockRecord =
        serde_json::from_str(include_str!("fixtures/block_record_4999999.json")).unwrap();
    let context = || PeakSlotContext {
        sp_sub_slot: None,
        ip_sub_slot: Some(&second),
        fork_block: None,
    };
    state.new_peak(
        &peak,
        context(),
        &HashMap::new(),
        MAINNET.sub_slot_iters_starting,
        30,
        true,
    );
    assert!(state.get_sub_slot(&first_hash).is_none());
    assert_eq!(state.get_recent_eos(&first_hash), Some(&first));
    assert_eq!(state.received_time(&first_hash), Some(first_time));
    assert_eq!(state.get_recent_eos(&second_hash), Some(&second));
    let second_time = state.received_time(&second_hash).unwrap();
    state.new_peak(
        &peak,
        context(),
        &HashMap::new(),
        MAINNET.sub_slot_iters_starting,
        30,
        true,
    );
    assert_eq!(state.received_time(&second_hash), Some(second_time));
    peak.prev_hash = peak.header_hash;
    peak.header_hash = [31; 32].into();
    peak.height += 1;
    state.new_peak(
        &peak,
        context(),
        &HashMap::new(),
        MAINNET.sub_slot_iters_starting,
        30,
        true,
    );
    assert!(state.get_recent_eos(&first_hash).is_some());
    peak.prev_hash = [32; 32].into();
    peak.header_hash = [33; 32].into();
    state.new_peak(
        &peak,
        context(),
        &HashMap::new(),
        MAINNET.sub_slot_iters_starting,
        30,
        true,
    );
    assert!(state.get_recent_eos(&first_hash).is_none());
    assert!(state.received_time(&first_hash).is_none());
    assert_eq!(state.get_recent_eos(&second_hash), Some(&second));
    assert_eq!(state.received_time(&second_hash), Some(second_time));
    state.clear_slots();
    assert!(state.get_recent_eos(&second_hash).is_none());
}
