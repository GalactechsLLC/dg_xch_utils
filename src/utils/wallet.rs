use std::io::{Error, ErrorKind};
use num_traits::ToPrimitive;
use crate::clvm::program::Program;
use crate::types::blockchain::coin_record::CoinRecord;
use crate::types::blockchain::coin_spend::CoinSpend;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::pool::{DELAY_PUZZLEHASH_IDENTIFIER, DELAY_TIME_IDENTIFIER, PoolState};
use crate::utils::SINGLETON_LAUNCHER_PUZZLE_HASH;

#[derive(Debug)]
pub struct PlotNft {
    pub launcher_id: Bytes32,
    pub singleton_coin: CoinRecord,
    pub pool_state: PoolState,
    pub delay_time: i32,
    pub delay_puzzle_hash: Bytes32
}

pub struct PlotNftExtraData {
    pub pool_state: PoolState,
    pub delay_time: i32,
    pub delay_puzzle_hash: Bytes32
}
impl PlotNftExtraData {
    pub fn from_program(program: Program) -> Result<Self, Error> {
        let pool_state = PoolState::from_extra_data_program(&program)?;

        let extra_data_program_list = program.as_list();
        let delay_time_programs: Vec<Program> = extra_data_program_list.iter().filter(|p| {
            if let Ok(f) = p.first() {
                if let Ok(ai) = f.as_int() {
                    if let Some(au) = ai.to_u8(){
                        return char::from(au) == DELAY_TIME_IDENTIFIER
                    }
                }
            }
            false
        }).cloned().collect();
        if delay_time_programs.is_empty() || delay_time_programs.len() > 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid PlotNFT"));
        }
        let delay_time = delay_time_programs[0].rest()?.as_int()?;

        let extra_data_programs: Vec<Program> = extra_data_program_list.into_iter().filter(|p| {
            if let Ok(f) = p.first() {
                if let Ok(ai) = f.as_int() {
                    if let Some(au) = ai.to_u8(){
                        return char::from(au) == DELAY_PUZZLEHASH_IDENTIFIER
                    }
                }
            }
            false
        }).collect();
        if extra_data_programs.is_empty() || extra_data_programs.len() > 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid PlotNFT"));
        }
        Ok(PlotNftExtraData{
            pool_state,
            delay_time: delay_time.to_i32().unwrap_or_default(),
            delay_puzzle_hash: extra_data_programs[0].rest()?.as_vec().unwrap_or_default().into(),
        })
    }
}

pub fn launcher_coin_spend_to_extra_data(coin_spend: &CoinSpend) -> Result<PlotNftExtraData, Error> {
    if coin_spend.coin.puzzle_hash != *SINGLETON_LAUNCHER_PUZZLE_HASH {
        return Err(Error::new(ErrorKind::InvalidInput, "Provided coin spend is not launcher coin spend"));
    }
    return PlotNftExtraData::from_program(coin_spend.solution.to_program()?.rest()?.rest()?.first()?);
}