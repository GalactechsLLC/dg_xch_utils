use crate::blockchain::sized_bytes::Bytes32;
use crate::consensus::block_rewards::RewardSchedule;
use crate::consensus::constants::{ChiaNetwork, ConsensusConstants, MAINNET};
use crate::utils::hash_256;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainDefinition {
    pub network_id: String,
    pub genesis_seed: String,
    pub rewards: RewardSchedule,
}

impl Default for ChainDefinition {
    fn default() -> Self {
        Self {
            network_id: "dgx".into(),
            genesis_seed: "dg_xch/dgx/no-prefarm/v1".into(),
            rewards: RewardSchedule::NO_PREFARM,
        }
    }
}

impl ChainDefinition {
    pub fn constants(&self) -> Result<ConsensusConstants, String> {
        if self.network_id.is_empty()
            || self.network_id.len() > 64
            || !self
                .network_id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || ChiaNetwork::from_str(&self.network_id).is_ok()
        {
            return Err("custom network id must be 1..64 lowercase letters, digits or hyphens and cannot name a Chia network".into());
        }
        if self.genesis_seed.is_empty() || self.genesis_seed.len() > 1024 {
            return Err("genesis_seed must contain 1..1024 bytes".into());
        }
        self.rewards.validate(MAINNET.max_coin_amount)?;
        if self.rewards.genesis_pool != 0 || self.rewards.genesis_farmer != 0 {
            return Err("this fork definition requires zero genesis rewards".into());
        }
        let mut constants = ConsensusConstants {
            rewards: self.rewards,
            genesis_challenge: Bytes32::default(),
            agg_sig_me_additional_data: Bytes32::default(),
            genesis_pre_farm_pool_puzzle_hash: Bytes32::default(),
            genesis_pre_farm_farmer_puzzle_hash: Bytes32::default(),
            bech32_prefix: "dgx",
            ..MAINNET
        };
        let mut identity = b"dg_xch/chain-definition/v1\0".to_vec();
        identity.extend(serde_json::to_vec(&(self, constants)).map_err(|error| error.to_string())?);
        constants.genesis_challenge = Bytes32::const_new(hash_256(identity));
        let mut signature_domain = b"dg_xch/agg-sig/v1\0".to_vec();
        signature_domain.extend_from_slice(constants.genesis_challenge.as_ref());
        constants.agg_sig_me_additional_data = Bytes32::const_new(hash_256(signature_domain));
        Ok(constants)
    }

    pub fn handshake_network_id(&self) -> Result<String, String> {
        Ok(format!(
            "{}-{}",
            self.network_id,
            hex::encode(self.constants()?.genesis_challenge)
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/consensus/chain_definition.rs"]
mod tests;
