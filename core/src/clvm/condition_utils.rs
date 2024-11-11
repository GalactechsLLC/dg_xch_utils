use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::{u64_to_bytes, Bytes32, SizedBytes};
use crate::blockchain::utils::atom_to_int;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::{IntoSExp, SExp};
use dg_xch_serialize::hash_256;
use log::{info, warn};
use num_traits::ToPrimitive;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

pub type ConditionsDict<S> = HashMap<ConditionOpcode, Vec<ConditionWithArgs>, S>;

pub fn parse_sexp_to_condition(sexp: &SExp) -> Result<ConditionWithArgs, Error> {
    let mut opcode = ConditionOpcode::Unknown;
    let mut vars = vec![];
    let mut first = true;
    for arg in sexp.iter().take(4) {
        match arg {
            SExp::Atom(arg) => {
                if first {
                    first = false;
                    if arg.data.len() != 1 {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            "Invalid OpCode for Condition",
                        ));
                    }
                    opcode = ConditionOpcode::from(arg.data[0]);
                } else {
                    vars.push(arg.data.clone());
                }
            }
            SExp::Pair(_) => {
                warn!("Got pair in opcode args");
                break;
            }
        }
    }
    if vars.is_empty() {
        Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid Condition No Vars",
        ))
    } else {
        Ok(ConditionWithArgs { opcode, vars })
    }
}

pub fn parse_sexp_to_conditions(sexp: &SExp) -> Result<Vec<ConditionWithArgs>, Error> {
    let mut results = Vec::new();
    for arg in sexp {
        match parse_sexp_to_condition(arg) {
            Ok(condition) => {
                results.push(condition);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(results)
}

#[must_use]
pub fn conditions_by_opcode<S: std::hash::BuildHasher + Default>(
    conditions: Vec<ConditionWithArgs>,
) -> ConditionsDict<S> {
    let mut hm: ConditionsDict<S> = HashMap::with_hasher(S::default());
    for cvp in conditions {
        match hm.get_mut(&cvp.opcode) {
            Some(list) => {
                list.push(cvp.clone());
            }
            None => {
                hm.insert(cvp.opcode, vec![cvp.clone()]);
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
        for cvp in args {
            let amount = atom_to_int(&cvp.vars[1]).to_u64().ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "Failed to convert atom to int")
            })?;
            let coin = Coin {
                parent_coin_info: input_coin_name,
                puzzle_hash: Bytes32::new(&cvp.vars[0]),
                amount,
            };
            output_coins.push(coin);
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
        Ok((cost, r)) => match parse_sexp_to_conditions(&r.to_sexp()) {
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
    let mut buffer = agg_sig_data.bytes.to_vec();
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

pub fn make_aggsig_final_message<S: std::hash::BuildHasher + Default>(
    opcode: ConditionOpcode,
    msg: &[u8],
    coin: Coin,
    agg_sig_additional_data: HashMap<ConditionOpcode, Bytes32, S>,
) -> Result<Vec<u8>, Error> {
    let addendum = match opcode {
        ConditionOpcode::AggSigParent => coin.parent_coin_info.bytes.to_vec(),
        ConditionOpcode::AggSigPuzzle => coin.puzzle_hash.bytes.to_vec(),
        ConditionOpcode::AggSigAmount => u64_to_bytes(coin.amount),
        ConditionOpcode::AggSigPuzzleAmount => coin
            .puzzle_hash
            .bytes
            .iter()
            .chain(&u64_to_bytes(coin.amount))
            .copied()
            .collect(),
        ConditionOpcode::AggSigParentAmount => coin
            .parent_coin_info
            .bytes
            .iter()
            .chain(&u64_to_bytes(coin.amount))
            .copied()
            .collect(),
        ConditionOpcode::AggSigParentPuzzle => coin
            .parent_coin_info
            .bytes
            .iter()
            .chain(&coin.puzzle_hash.bytes)
            .copied()
            .collect(),
        ConditionOpcode::AggSigMe => coin.name().bytes.to_vec(),
        _ => vec![],
    };
    let additional_data = agg_sig_additional_data
        .get(&opcode)
        .map(|b| b.bytes.to_vec())
        .unwrap_or_default();
    Ok(msg
        .iter()
        .chain(addendum.iter())
        .chain(additional_data.iter())
        .copied()
        .collect())
}
