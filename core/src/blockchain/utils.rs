use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::condition_utils::conditions_dict_for_solution;
use crate::clvm::condition_utils::created_outputs_for_conditions_dict;
use crate::clvm::program::SerializedProgram;
use num_bigint::BigInt;
use std::io::Error;

pub fn additions_for_solution(
    coin_name: Bytes32,
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> Result<Vec<Coin>, Error> {
    let (map, _cost) = conditions_dict_for_solution(puzzle_reveal, solution, max_cost)?;
    created_outputs_for_conditions_dict(map, coin_name)
}

pub fn fee_for_solution(
    puzzle_reveal: &SerializedProgram,
    solution: &SerializedProgram,
    max_cost: u64,
) -> BigInt {
    match conditions_dict_for_solution(puzzle_reveal, solution, max_cost) {
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

pub fn atom_to_int(bytes: &Vec<u8>) -> BigInt {
    let len = bytes.len();
    if len == 0 {
        0.into()
    } else {
        BigInt::from_signed_bytes_be(bytes)
    }
}
