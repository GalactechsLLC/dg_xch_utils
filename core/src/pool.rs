use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::program::Program;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Error, ErrorKind};
use std::string::String;

pub const POOL_STATE_IDENTIFIER: char = 'p';
pub const DELAY_TIME_IDENTIFIER: char = 't';
pub const DELAY_PUZZLEHASH_IDENTIFIER: char = 'h';

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Farmer {
    pub launcher_id: Bytes32,
    pub p2_singleton_puzzle_hash: Bytes32,
    pub delay_time: u64,
    pub delay_puzzle_hash: Bytes32,
    pub authentication_public_key: Bytes48,
    pub singleton_tip: CoinSpend,
    pub singleton_tip_state: PoolState,
    pub balance: u64,
    pub points: u64,
    pub difficulty: u64,
    pub payout_instructions: String,
    pub is_pool_member: bool,
    pub joined: u64,
    pub modified: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PoolState {
    pub version: u8,
    pub state: u8,
    pub target_puzzle_hash: Bytes32,
    pub owner_pubkey: Bytes48,
    pub pool_url: Option<String>,
    pub relative_lock_height: u32,
}

impl PoolState {
    pub fn from_extra_data_program(program: &Program) -> Result<Self, Error> {
        let extra_data_cons_boxes: Vec<Program> = program
            .as_list()
            .into_iter()
            .filter(|p| {
                if let Ok(f) = p.first() {
                    if let Ok(ai) = f.as_int() {
                        if let Some(au) = ai.to_u8() {
                            return char::from(au) == POOL_STATE_IDENTIFIER;
                        }
                    }
                }
                false
            })
            .collect();
        if extra_data_cons_boxes.is_empty() || extra_data_cons_boxes.len() > 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid PlotNFT"));
        }
        let mut cursor = Cursor::new(
            extra_data_cons_boxes[0]
                .rest()?
                .as_vec()
                .unwrap_or_default(),
        );
        Self::from_bytes(&mut cursor, ChiaProtocolVersion::default())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SingletonState {
    pub saved_solution: CoinSpend,
    pub saved_state: PoolState,
    pub last_not_none_state: PoolState,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ValidatedSingletonState {
    pub saved_solution: CoinSpend,
    pub saved_state: PoolState,
    pub is_pool_member: bool,
}
