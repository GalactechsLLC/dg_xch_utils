pub mod block_filter;
// Block validation and production verify BLS signatures on their main paths; they are only
// available with the `bls` feature (default-on). A `bls`-less build keeps the pure data/wire
// types but is not consensus-capable.
#[cfg(feature = "bls")]
pub mod block_generator;
#[cfg(feature = "bls")]
pub mod block_header_validation;
pub mod block_rewards;
pub mod chain_definition;
pub mod coinbase;
pub mod constants;
pub mod deficit;
pub mod difficulty_adjustment;
pub mod fast_forward;
pub mod full_block_to_block_record;
pub mod generator_puzzles;
pub mod get_block_challenge;
pub mod make_sub_epoch_summary;
pub mod merkle_set;
pub mod overrides;
pub mod pot_iterations;
#[cfg(feature = "bls")]
pub mod producer;
pub mod vdf_info_computation;

use crate::blockchain::sized_bytes::Bytes32;
use std::io::{Error, ErrorKind};

pub const CREATE_COIN_COST: u64 = 1_800_000;
pub const AGG_SIG_COST: u64 = 1_200_000;

pub(crate) fn missing(hash: Bytes32) -> Error {
    Error::new(
        ErrorKind::NotFound,
        format!("block record not found: {hash}"),
    )
}

pub(crate) fn rejected(msg: &'static str) -> Error {
    Error::new(ErrorKind::InvalidData, msg)
}
