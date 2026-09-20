#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::chain_definition::ChainDefinition;

pub mod service;
pub mod worker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisWork {
    pub challenge_chain_challenge: Bytes32,
    pub reward_chain_challenge: Bytes32,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub discriminant_size_bits: u64,
}

impl GenesisWork {
    pub fn from_chain_definition(definition: &ChainDefinition) -> Result<Self, String> {
        let constants = definition.constants()?;
        Ok(Self {
            challenge_chain_challenge: constants.genesis_challenge,
            reward_chain_challenge: constants.genesis_challenge,
            difficulty: constants.difficulty_starting,
            sub_slot_iters: constants.sub_slot_iters_starting,
            discriminant_size_bits: constants.discriminant_size_bits,
        })
    }
}
