use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Spend {
    pub parent_id: Bytes32,
    pub coin_amount: u64,
    pub puzzle_hash: Bytes32,
    pub coin_id: Bytes32,
    pub height_relative: Option<u32>,
    pub seconds_relative: Option<u64>,
    pub before_height_relative: Option<u32>,
    pub before_seconds_relative: Option<u64>,
    pub birth_height: Option<u32>,
    pub birth_seconds: Option<u64>,
    pub create_coin: HashSet<NewCoin>,
    pub agg_sig_me: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub agg_sig_parent: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub agg_sig_puzzle: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub agg_sig_amount: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub agg_sig_puzzle_amount: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub agg_sig_parent_amount: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub agg_sig_parent_puzzle: Vec<(UnsizedBytes, UnsizedBytes)>,
    pub flags: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewCoin {
    pub puzzle_hash: Bytes32,
    pub amount: u64,
    pub hint: Option<Vec<u8>>,
}
impl Hash for NewCoin {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.puzzle_hash.hash(h);
        self.amount.hash(h);
    }
}
