use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::{ConditionWithArgs, Message};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::condition_utils::{
    agg_sig_additional_data, conditions_dict_for_solution, created_outputs_for_conditions_dict,
    ConditionsDict,
};
use crate::clvm::program::SerializedProgram;
use crate::consensus::constants::ConsensusConstants;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use log::warn;
use num_bigint::BigInt;
use std::hash::RandomState;
use std::io::Error;

pub fn additions_for_solution(
    coin_name: Bytes32,
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> Result<Vec<Coin>, Error> {
    let (map, _cost) =
        conditions_dict_for_solution::<RandomState>(puzzle_reveal, solution, max_cost)?;
    created_outputs_for_conditions_dict(&map, coin_name)
}

#[must_use]
pub fn fee_for_solution(
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> BigInt {
    match conditions_dict_for_solution::<RandomState>(puzzle_reveal, solution, max_cost) {
        Ok((conditions, _cost)) => {
            let mut total: BigInt = 0.into();
            match conditions.get(&ConditionOpcode::ReserveFee) {
                Some(conditions) => {
                    for cond in conditions {
                        if let ConditionWithArgs::ReserveFee(fee) = cond {
                            total += *fee;
                        } else {
                            warn!("Found Invalid Condition in Conditions Dict")
                        }
                    }
                }
                None => {
                    total = 0.into();
                }
            }
            total
        }
        Err(_error) => 0.into(),
    }
}

pub fn pkm_pairs_for_conditions_dict<S: std::hash::BuildHasher + Default>(
    conditions_dict: &ConditionsDict<S>,
    coin: Coin,
    additional_data: &[u8],
) -> Result<Vec<(Bytes48, Message)>, Error> {
    let mut ret = vec![];
    if let Some(v) = conditions_dict.get(&ConditionOpcode::AggSigUnsafe) {
        for cwa in v {
            if let ConditionWithArgs::AggSigUnsafe(key, msg) = *cwa {
                if msg.data().ends_with(additional_data) {
                    return Err(Error::other("Invalid Condition"));
                }
                ret.push((key, msg));
            } else {
                return Err(Error::other("Invalid Condition"));
            }
        }
    }
    if let Some(v) = conditions_dict.get(&ConditionOpcode::AggSigMe) {
        for cwa in v {
            if let ConditionWithArgs::AggSigMe(key, msg) = *cwa {
                let mut buf = msg.data().to_vec();
                buf.extend(coin.name());
                buf.extend(additional_data);
                ret.push((key, Message::new(buf)?));
            } else {
                return Err(Error::other("Invalid Condition"));
            }
        }
    }
    if let Some(v) = conditions_dict.get(&ConditionOpcode::AggSigPuzzle) {
        let agg_sig_map = agg_sig_additional_data::<S>(Bytes32::parse(additional_data)?);
        let additional_data = agg_sig_map
            .get(&ConditionOpcode::AggSigPuzzle)
            .copied()
            .unwrap_or_default();
        for cwa in v {
            if let ConditionWithArgs::AggSigPuzzle(key, msg) = *cwa {
                let mut buf = msg.data().to_vec();
                buf.extend(coin.puzzle_hash);
                buf.extend(additional_data);
                ret.push((key, Message::new(buf)?));
            } else {
                return Err(Error::other("Invalid Condition"));
            }
        }
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
