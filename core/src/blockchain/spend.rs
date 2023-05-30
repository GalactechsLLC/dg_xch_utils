use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Spend {
    pub coin_id: Vec<u8>,
    pub puzzle_hash: Vec<u8>,
    pub height_relative: Option<u32>,
    pub seconds_relative: Option<u64>,
    pub before_height_relative: Option<u32>,
    pub before_seconds_relative: Option<u64>,
    pub birth_height: Option<u32>,
    pub birth_seconds: Option<u64>,
    pub create_coin: Vec<(Vec<u8>, u64, Optional<Vec<u8>>)>,
    pub agg_sig_me: Vec<(Vec<u8>, Vec<u8>)>,
    pub flags: u32
}