use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::ConditionWithArgs;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use crate::clvm::condition_utils::{agg_sig_additional_data, created_outputs_for_conditions_dict};
use crate::clvm::condition_utils::{conditions_dict_for_solution, ConditionsDict};
use crate::clvm::program::SerializedProgram;
use num_bigint::BigInt;
use std::hash::RandomState;
use std::io::{Error, ErrorKind};

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
                        total += atom_to_int(&cond.vars[0]);
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

#[must_use]
pub fn atom_to_int(bytes: &[u8]) -> BigInt {
    if bytes.is_empty() {
        0.into()
    } else {
        BigInt::from_signed_bytes_be(bytes)
    }
}

pub fn pkm_pairs_for_conditions_dict<S: std::hash::BuildHasher + Default>(
    conditions_dict: &ConditionsDict<S>,
    coin: Coin,
    additional_data: &[u8],
) -> Result<Vec<(Bytes48, Vec<u8>)>, Error> {
    let mut ret = vec![];
    let agg_sig_map = agg_sig_additional_data::<S>(Bytes32::new(additional_data));
    if let Some(v) = conditions_dict.get(&ConditionOpcode::AggSigUnsafe) {
        for cwa in v {
            validate_cwa(cwa)?;
            if ends_with(&cwa.vars[1], additional_data) {
                return Err(Error::new(ErrorKind::Other, "Invalid Condition"));
            }
            ret.push((Bytes48::new(&cwa.vars[0]), cwa.vars[1].clone()));
        }
    }
    if let Some(v) = conditions_dict.get(&ConditionOpcode::AggSigMe) {
        for cwa in v {
            validate_cwa(cwa)?;
            let mut buf = cwa.vars[1].clone();
            buf.extend(coin.name().as_slice());
            buf.extend(additional_data);
            ret.push((Bytes48::new(&cwa.vars[0]), buf));
        }
    }
    if let Some(v) = conditions_dict.get(&ConditionOpcode::AggSigPuzzle) {
        let additional_data = agg_sig_map
            .get(&ConditionOpcode::AggSigPuzzle)
            .copied()
            .unwrap_or_default();
        for cwa in v {
            validate_cwa(cwa)?;
            let mut buf = cwa.vars[1].clone();
            buf.extend(coin.puzzle_hash.as_slice());
            buf.extend(additional_data.as_slice());
            ret.push((Bytes48::new(&cwa.vars[0]), buf));
        }
    }
    Ok(ret)
}

fn validate_cwa(cwa: &ConditionWithArgs) -> Result<(), Error> {
    if cwa.vars.len() != 2 {
        return Err(Error::new(ErrorKind::Other, "Invalid Condition"));
    }
    if !(cwa.vars[0].len() == 48 && cwa.vars[1].len() <= 1024) {
        return Err(Error::new(ErrorKind::Other, "Invalid Condition"));
    }
    Ok(())
}

fn ends_with(buf: &[u8], sufix: &[u8]) -> bool {
    if buf.len() < sufix.len() {
        false
    } else {
        &buf[buf.len() - sufix.len()..] == sufix
    }
}
