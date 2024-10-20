use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::{Bytes32, SizedBytes};
use crate::blockchain::utils::atom_to_int;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::{IntoSExp, SExp};
use num_traits::ToPrimitive;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};

pub type ConditionsDict<S> = HashMap<ConditionOpcode, Vec<ConditionWithArgs>, S>;

pub fn parse_sexp_to_condition(sexp: &SExp) -> Result<ConditionWithArgs, Error> {
    let as_atoms = sexp.as_atom_list();
    if as_atoms.is_empty() {
        Err(Error::new(ErrorKind::InvalidData, "Invalid Condition"))
    } else {
        match as_atoms.split_first() {
            Some((first, rest)) => Ok(ConditionWithArgs {
                opcode: ConditionOpcode::from(first[0]),
                vars: Vec::from(rest),
            }),
            None => Err(Error::new(ErrorKind::InvalidData, "Invalid Condition")),
        }
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
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}
