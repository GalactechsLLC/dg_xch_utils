use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::{ConditionWithArgs, Message};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::condition_utils::agg_sig_additional_data_for_opcode;
use crate::clvm::program::Program;
use crate::consensus::constants::ConsensusConstants;
use crate::errors::ClvmError;
use crate::formatting::u64_to_bytes;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use num_bigint::BigInt;

pub fn additions_for_solution(
    coin_name: Bytes32,
    puzzle_reveal: &Program,
    solution: &Program,
    max_cost: u64,
) -> Result<Vec<Coin>, ClvmError> {
    additions_for_solution_with_flags(coin_name, puzzle_reveal, solution, max_cost, 0)
}

pub fn additions_for_solution_with_flags(
    coin_name: Bytes32,
    puzzle_reveal: &Program,
    solution: &Program,
    max_cost: u64,
    flags: u32,
) -> Result<Vec<Coin>, ClvmError> {
    let (_, output) = puzzle_reveal.run(max_cost, flags, solution)?;
    let mut coins = Vec::new();
    for condition in output.sexp().ref_list() {
        if condition
            .ref_list()
            .first()
            .is_some_and(|op| ConditionOpcode::from(*op) == ConditionOpcode::CreateCoin)
            && let ConditionWithArgs::CreateCoin(puzzle_hash, amount, _) =
                ConditionWithArgs::try_from(condition)?
        {
            coins.push(Coin {
                parent_coin_info: coin_name,
                puzzle_hash,
                amount,
            });
        }
    }
    Ok(coins)
}

pub fn fee_for_solution(
    puzzle_reveal: &Program,
    solution: &Program,
    max_cost: u64,
) -> Result<BigInt, ClvmError> {
    let (_, output) = puzzle_reveal.run_with_cost(max_cost, solution)?;
    let mut total: BigInt = 0.into();
    for condition in output.sexp().ref_list() {
        if condition
            .ref_list()
            .first()
            .is_some_and(|op| ConditionOpcode::from(*op) == ConditionOpcode::ReserveFee)
            && let ConditionWithArgs::ReserveFee(fee) = ConditionWithArgs::try_from(condition)?
        {
            total += fee;
        }
    }
    Ok(total)
}

pub fn pkm_pairs_for_conditions(
    conditions: &[ConditionWithArgs],
    coin: Coin,
    additional_data: &[u8],
) -> Result<Vec<(ConditionOpcode, Bytes48, Message)>, ClvmError> {
    let mut ret = vec![];
    let additional_data = Bytes32::parse(additional_data)?;
    for condition in conditions {
        let agg_sig_additional_data =
            agg_sig_additional_data_for_opcode(additional_data, condition.op_code());
        match condition {
            ConditionWithArgs::AggSigParent(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(coin.parent_coin_info.as_ref());
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigPuzzle(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(coin.puzzle_hash.as_ref());
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigAmount(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(&u64_to_bytes(coin.amount));
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigPuzzleAmount(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(coin.puzzle_hash.as_ref());
                msg.extend_from_slice(&u64_to_bytes(coin.amount));
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigParentAmount(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(coin.parent_coin_info.as_ref());
                msg.extend_from_slice(&u64_to_bytes(coin.amount));
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigParentPuzzle(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(coin.parent_coin_info.as_ref());
                msg.extend_from_slice(coin.puzzle_hash.as_ref());
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigMe(key, message) => {
                let mut msg = message.data().to_vec();
                msg.extend_from_slice(coin.name().as_ref());
                msg.extend_from_slice(agg_sig_additional_data.as_ref());
                ret.push((condition.op_code(), *key, Message::new(msg)?));
            }
            ConditionWithArgs::AggSigUnsafe(key, message) => {
                ret.push((condition.op_code(), *key, *message));
            }
            _ => continue,
        };
    }
    Ok(ret)
}

pub fn verify_agg_sig_unsafe_message(
    message: &Message,
    consensus_constants: &ConsensusConstants,
) -> Result<(), ClvmError> {
    let mut buffer = consensus_constants
        .agg_sig_me_additional_data
        .bytes()
        .to_vec();
    let mut forbidden_message_suffix;
    for code in [
        ConditionOpcode::AggSigParent,
        ConditionOpcode::AggSigPuzzle,
        ConditionOpcode::AggSigAmount,
        ConditionOpcode::AggSigPuzzleAmount,
        ConditionOpcode::AggSigParentAmount,
        ConditionOpcode::AggSigParentPuzzle,
    ] {
        buffer.push(code as u8);
        forbidden_message_suffix = Bytes32::from(hash_256(&buffer));
        if message.data().ends_with(forbidden_message_suffix.as_ref()) {
            return Err(ClvmError::InvalidSyntax("Invalid Condition".to_string()));
        }
        buffer.pop();
    }
    Ok(())
}
