use crate::blockchain::coin::Coin;
use crate::blockchain::sized_bytes::{Bytes32};

pub fn pool_parent_id(block_height: u32, genesis_challenge: &Bytes32) -> Bytes32 {
    let mut buf: [u8; 32] = [0; 32];
    buf[0..16].copy_from_slice(&genesis_challenge[0..16]);
    buf[28..32].copy_from_slice(&block_height.to_be_bytes());
    Bytes32::from_sized_bytes(buf)
}

pub fn farmer_parent_id(block_height: u32, genesis_challenge: &Bytes32) -> Bytes32 {
    let mut buf: [u8; 32] = [0; 32];
    buf[0..16].copy_from_slice(&genesis_challenge[16..32]);
    buf[28..32].copy_from_slice(&block_height.to_be_bytes());
    Bytes32::from_sized_bytes(buf)
}

pub fn create_pool_coin(block_height: u32, puzzle_hash: &Bytes32, amount: u64, genesis_challenge: &Bytes32) -> Coin {
    let parent_coin_info = pool_parent_id(block_height, genesis_challenge);
    Coin {
        parent_coin_info,
        puzzle_hash: *puzzle_hash,
        amount,
    }
}

pub fn create_farmer_coin(block_height: u32, puzzle_hash: &Bytes32, amount: u64, genesis_challenge: &Bytes32) -> Coin {
    let parent_coin_info = farmer_parent_id(block_height, genesis_challenge);
    Coin {
        parent_coin_info,
        puzzle_hash: *puzzle_hash,
        amount,
    }
}