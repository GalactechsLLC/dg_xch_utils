use crate::clvm::program::Program;
use crate::types::blockchain::coin_spend::CoinSpend;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes48};
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PoolState {
    pub version: u8,
    pub state: u8,
    pub target_puzzle_hash: Bytes32,
    pub owner_pubkey: Bytes48,
    pub pool_url: String,
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
        Ok(Self::from(
            extra_data_cons_boxes[0]
                .rest()?
                .as_vec()
                .unwrap_or_default(),
        ))
    }
}
impl From<Vec<u8>> for PoolState {
    fn from(bytes: Vec<u8>) -> Self {
        let version = bytes[0];
        let state = bytes[1];
        let target_puzzle_hash: Bytes32 = bytes[2..=34].to_vec().into();
        let owner_pubkey: Bytes48 = bytes[34..=81].to_vec().into();
        let has_url = bytes[82];
        let mut pool_url: String = String::new();
        let relative_lock_height: u32;
        if has_url == 1 {
            let mut len_ary: [u8; 4] = [0; 4];
            len_ary.copy_from_slice(&bytes[83..87]);
            let length = u32::from_be_bytes(len_ary);
            let url_length: usize = (87 + length) as usize;
            let mut url_vec = Vec::new();
            url_vec.append(&mut bytes[87..url_length].to_vec());
            pool_url = match String::from_utf8(url_vec) {
                Ok(string) => string,
                Err(_) => String::new(),
            };
            let mut lh_ary: [u8; 4] = [0; 4];
            lh_ary.copy_from_slice(&bytes[url_length..(url_length + 4)]);
            relative_lock_height = u32::from_be_bytes(lh_ary);
        } else {
            let mut lh_ary: [u8; 4] = [0; 4];
            lh_ary.copy_from_slice(&bytes[83..87]);
            relative_lock_height = u32::from_be_bytes(lh_ary);
        }
        PoolState {
            version,
            state,
            target_puzzle_hash,
            owner_pubkey,
            pool_url,
            relative_lock_height,
        }
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
