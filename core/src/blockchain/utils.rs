use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::{ConditionWithArgs, Message};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::condition_utils::{
    agg_sig_additional_data_for_opcode, conditions_for_solution, created_outputs_for_conditions,
};
use crate::clvm::program::SerializedProgram;
use crate::consensus::constants::ConsensusConstants;
use crate::formatting::u64_to_bytes;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use num_bigint::BigInt;
use std::io::Error;

pub fn additions_for_solution(
    coin_name: Bytes32,
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> Result<Vec<Coin>, Error> {
    let (map, _cost) = conditions_for_solution(puzzle_reveal, solution, max_cost)?;
    created_outputs_for_conditions(&map, coin_name)
}

#[must_use]
pub fn fee_for_solution(
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> BigInt {
    match conditions_for_solution(puzzle_reveal, solution, max_cost) {
        Ok((conditions, _cost)) => {
            let mut total: BigInt = 0.into();
            for condition in conditions {
                match condition {
                    ConditionWithArgs::ReserveFee(fee) => {
                        total += fee;
                    }
                    _ => continue,
                }
            }
            total
        }
        Err(_error) => 0.into(),
    }
}

pub fn pkm_pairs_for_conditions(
    conditions: &[ConditionWithArgs],
    coin: Coin,
    additional_data: &[u8],
) -> Result<Vec<(ConditionOpcode, Bytes48, Message)>, Error> {
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
) -> Result<(), Error> {
    let mut buffer = consensus_constants.agg_sig_me_additional_data.clone();
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
            return Err(Error::other("Invalid Condition"));
        }
        buffer.pop();
    }
    Ok(())
}
