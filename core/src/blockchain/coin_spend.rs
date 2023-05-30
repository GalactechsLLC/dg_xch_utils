use crate::blockchain::coin::Coin;
use crate::blockchain::utils::{additions_for_solution, fee_for_solution};
use crate::clvm::program::SerializedProgram;
use crate::clvm::utils::INFINITE_COST;
use dg_xch_macros::ChiaSerial;
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use std::io::Error;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CoinSpend {
    pub coin: Coin,
    pub puzzle_reveal: SerializedProgram,
    pub solution: SerializedProgram,
}
impl CoinSpend {
    pub fn additions(&self) -> Result<Vec<Coin>, Error> {
        additions_for_solution(
            self.coin.name(),
            &self.puzzle_reveal,
            &self.solution,
            INFINITE_COST,
        )
    }
    pub fn reserved_fee(self) -> BigInt {
        fee_for_solution(&self.puzzle_reveal, &self.solution, INFINITE_COST)
    }
}