use super::*;
use dg_xch_core::consensus::constants::MAINNET;

// A fresh SlotState holds only the genesis slot (eos == None), so the [1..] iteration is empty.

#[test]
fn get_finished_sub_slots_returns_empty_when_last_equals_chain() {
    // last_challenge_to_add == challenge_in_chain -> [] (nothing to add).
    let state = SlotState::new(MAINNET);
    let c = Bytes32::from([9; 32]);
    assert_eq!(state.get_finished_sub_slots(c, c), Some(Vec::new()));
}

#[test]
fn get_finished_sub_slots_bails_when_last_not_connected() {
    // With no finished (post-genesis) slots, a distinct last challenge cannot be found -> None.
    let state = SlotState::new(MAINNET);
    assert_eq!(
        state.get_finished_sub_slots(MAINNET.genesis_challenge, Bytes32::from([7; 32])),
        None
    );
}

#[test]
fn backtrack_rc_challenge_is_identity_without_matching_slots() {
    // The genesis slot carries no eos, so nothing matches and the challenge passes through.
    let state = SlotState::new(MAINNET);
    let rc = Bytes32::from([3; 32]);
    assert_eq!(state.backtrack_rc_challenge(rc), rc);
}
