use dg_xch_core::blockchain::condition_opcode::ConditionOpcode;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::sexp::{AtomBuf, IntoSExp, SExp};

pub fn make_create_coin_condition(puzzle_hash: Bytes32, amount: u64, memos: &[Vec<u8>]) -> Vec<SExp> {
    if !memos.is_empty() {
        vec![ConditionOpcode::CreateCoin.to_sexp(), puzzle_hash.to_sexp(), amount.to_sexp(), memos.to_sexp()]
    } else {
        vec![ConditionOpcode::CreateCoin.to_sexp(), puzzle_hash.to_sexp(), amount.to_sexp()]
    }
}

pub fn make_assert_aggsig_condition(public_key: &Bytes48) -> Vec<SExp> {
    vec![ConditionOpcode::AggSigUnsafe.to_sexp(), public_key.to_sexp()]
}

pub fn make_assert_my_coin_id_condition(coin_name: &Bytes32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertMyCoinId.to_sexp(), coin_name.to_sexp()]
}

pub fn make_assert_absolute_height_exceeds_condition(block_index: u32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertHeightAbsolute.to_sexp(), block_index.to_sexp()]
}

pub fn make_assert_relative_height_exceeds_condition(block_index: u32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertHeightRelative.to_sexp(), block_index.to_sexp()]
}

pub fn make_assert_absolute_seconds_exceeds_condition(time: u64) -> Vec<SExp> {
    vec![ConditionOpcode::AssertSecondsAbsolute.to_sexp(), time.to_sexp()]
}

pub fn make_assert_relative_seconds_exceeds_condition(time: u64) -> Vec<SExp> {
    vec![ConditionOpcode::AssertSecondsRelative.to_sexp(), time.to_sexp()]
}

pub fn make_reserve_fee_condition(fee: u64) -> Vec<SExp> {
    vec![ConditionOpcode::ReserveFee.to_sexp(), fee.to_sexp()]
}

pub fn make_assert_coin_announcement(announcement_hash: &Bytes32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertCoinAnnouncement.to_sexp(), announcement_hash.to_sexp()]
}

pub fn make_assert_puzzle_announcement(announcement_hash: &Bytes32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertPuzzleAnnouncement.to_sexp(), announcement_hash.to_sexp()]
}

pub fn make_create_coin_announcement(message: &[u8]) -> Vec<SExp> {
    vec![ConditionOpcode::CreateCoinAnnouncement.to_sexp(), SExp::Atom(AtomBuf::new(message.to_vec()))]
}

pub fn make_create_puzzle_announcement(message: &[u8]) -> Vec<SExp> {
    vec![ConditionOpcode::CreatePuzzleAnnouncement.to_sexp(), SExp::Atom(AtomBuf::new(message.to_vec()))]
}

pub fn make_assert_my_parent_id(parent_id: Bytes32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertMyParentId.to_sexp(), parent_id.to_sexp()]
}

pub fn make_assert_my_puzzlehash(puzzlehash: Bytes32) -> Vec<SExp> {
    vec![ConditionOpcode::AssertMyPuzzlehash.to_sexp(), puzzlehash.to_sexp()]
}

pub fn make_assert_my_amount(amount: u64) -> Vec<SExp> {
    vec![ConditionOpcode::AssertMyAmount.to_sexp(), amount.to_sexp()]
}
