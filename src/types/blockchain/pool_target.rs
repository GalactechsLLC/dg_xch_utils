use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::ChiaSerialize;
use serde::{Deserialize, Serialize};
use std::io::Error;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PoolTarget {
    pub puzzle_hash: Bytes32,
    pub max_height: u32,
}
impl ChiaSerialize for PoolTarget {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.puzzle_hash.to_sized_bytes());
        bytes.extend(self.max_height.to_be_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let (puzzle_hash, rest) = bytes.split_at(32);
        let mut u32_len_ary: [u8; 4] = [0; 4];
        let (max_height, _) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&max_height[0..4]);
        let max_height = u32::from_be_bytes(u32_len_ary);
        Ok(Self {
            puzzle_hash: Bytes32::from(puzzle_hash),
            max_height,
        })
    }
}
