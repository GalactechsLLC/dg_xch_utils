use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::IntoSExp;
use crate::formatting::u64_to_bytes;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use log::info;
use std::collections::HashMap;
use std::io::Error;

pub type ConditionsDict<S> = HashMap<ConditionOpcode, Vec<ConditionWithArgs>, S>;

#[must_use]
pub fn conditions_by_opcode<S: std::hash::BuildHasher + Default>(
    conditions: Vec<ConditionWithArgs>,
) -> ConditionsDict<S> {
    let mut hm: ConditionsDict<S> = HashMap::with_hasher(S::default());
    for cvp in conditions {
        match hm.get_mut(&cvp.op_code()) {
            Some(list) => {
                list.push(cvp);
            }
            None => {
                hm.insert(cvp.op_code(), vec![cvp]);
            }
        }
    }
    hm
}

pub fn created_outputs_for_conditions_dict<S: std::hash::BuildHasher + Default>(
    conditions_dict: &ConditionsDict<S>,
    input_coin_name: Bytes32,
) -> Result<Vec<Coin>, Error> {
    let mut output_coins = Vec::new();
    if let Some(args) = conditions_dict.get(&ConditionOpcode::CreateCoin) {
        for cwa in args {
            if let ConditionWithArgs::CreateCoin(puzzle_hash, amount, _) = *cwa {
                let coin = Coin {
                    parent_coin_info: input_coin_name,
                    puzzle_hash,
                    amount,
                };
                output_coins.push(coin);
            } else {
                return Err(Error::other("Invalid Condition"));
            }
        }
    }
    Ok(output_coins)
}

pub fn conditions_dict_for_solution<S: std::hash::BuildHasher + Default>(
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> Result<(ConditionsDict<S>, u64), Error> {
    match conditions_for_solution(puzzle_reveal, solution, max_cost) {
        Ok((result, cost)) => Ok((conditions_by_opcode(result), cost)),
        Err(error) => Err(error),
    }
}

pub fn conditions_for_solution(
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> Result<(Vec<ConditionWithArgs>, u64), Error> {
    match puzzle_reveal.run_with_cost(max_cost, &solution.to_program()) {
        Ok((cost, r)) => match (&r.to_sexp()).try_into() {
            Ok(conditions) => Ok((conditions, cost)),
            Err(error) => {
                info!("{error:?}");
                Err(error)
            }
        },
        Err(error) => {
            info!("{error:?}");
            Err(error)
        }
    }
}

pub fn agg_sig_additional_data<S: std::hash::BuildHasher + Default>(
    agg_sig_data: Bytes32,
) -> HashMap<ConditionOpcode, Bytes32, S> {
    let mut ret = HashMap::default();
    let mut buffer = agg_sig_data.bytes().to_vec();
    for code in [
        ConditionOpcode::AggSigParent,
        ConditionOpcode::AggSigPuzzle,
        ConditionOpcode::AggSigAmount,
        ConditionOpcode::AggSigPuzzleAmount,
        ConditionOpcode::AggSigParentAmount,
        ConditionOpcode::AggSigParentPuzzle,
    ] {
        buffer.push(code as u8);
        ret.insert(code, Bytes32::from(hash_256(&buffer)));
        buffer.pop();
    }
    ret.insert(ConditionOpcode::AggSigMe, agg_sig_data);
    ret
}

pub fn agg_sig_additional_data_for_opcode(
    agg_sig_data: Bytes32,
    opcode: ConditionOpcode,
) -> Bytes32 {
    let mut buffer = agg_sig_data.bytes().to_vec();
    buffer.push(opcode as u8);
    Bytes32::from(hash_256(&buffer))
}

pub fn make_aggsig_final_message<S: std::hash::BuildHasher + Default>(
    opcode: ConditionOpcode,
    msg: &[u8],
    coin: Coin,
    agg_sig_additional_data: HashMap<ConditionOpcode, Bytes32, S>,
) -> Result<Vec<u8>, Error> {
    let addendum = match opcode {
        ConditionOpcode::AggSigParent => coin.parent_coin_info.bytes().to_vec(),
        ConditionOpcode::AggSigPuzzle => coin.puzzle_hash.bytes().to_vec(),
        ConditionOpcode::AggSigAmount => u64_to_bytes(coin.amount),
        ConditionOpcode::AggSigPuzzleAmount => coin
            .puzzle_hash
            .bytes()
            .iter()
            .chain(&u64_to_bytes(coin.amount))
            .copied()
            .collect(),
        ConditionOpcode::AggSigParentAmount => coin
            .parent_coin_info
            .bytes()
            .iter()
            .chain(&u64_to_bytes(coin.amount))
            .copied()
            .collect(),
        ConditionOpcode::AggSigParentPuzzle => coin
            .parent_coin_info
            .bytes()
            .iter()
            .chain(&coin.puzzle_hash.bytes())
            .copied()
            .collect(),
        ConditionOpcode::AggSigMe => coin.name().bytes().to_vec(),
        _ => vec![],
    };
    let additional_data = agg_sig_additional_data
        .get(&opcode)
        .map(|b| b.bytes().to_vec())
        .unwrap_or_default();
    Ok(msg
        .iter()
        .chain(addendum.iter())
        .chain(additional_data.iter())
        .copied()
        .collect())
}
