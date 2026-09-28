use crate::blockchain::sized_bytes::Bytes32;
use crate::consensus::block_rewards::RewardSchedule;
use crate::consensus::chain_baseline::DGX_V1_BASELINE;
use crate::consensus::constants::{ChiaNetwork, ConsensusConstants};
use crate::utils::hash_256;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainDefinition {
    pub network_id: String,
    pub genesis_seed: String,
    pub rewards: RewardSchedule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consensus: Option<LaunchConsensus>,
}

impl Default for ChainDefinition {
    fn default() -> Self {
        Self {
            network_id: "dgx".into(),
            genesis_seed: "dg_xch/dgx/no-prefarm/v1".into(),
            rewards: RewardSchedule::NO_PREFARM,
            consensus: None,
        }
    }
}

impl ChainDefinition {
    #[must_use]
    pub fn dgx() -> Self {
        Self {
            genesis_seed: "dg_xch/dgx/no-prefarm/pos2/v2".into(),
            consensus: Some(LaunchConsensus::default()),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn development(genesis_seed: String) -> Self {
        Self {
            network_id: "dgx-dev".into(),
            genesis_seed,
            consensus: Some(LaunchConsensus {
                development: true,
                difficulty_starting: 1,
                difficulty_constant_factor_bits: 38,
                sub_slot_iters_starting: 16_384,
                plot_filter_bits: 0,
                plot_filter_v2_bits: 0,
                ..LaunchConsensus::default()
            }),
            ..Self::default()
        }
    }

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
        self.rewards.validate(DGX_V1_BASELINE.max_coin_amount)?;
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
            ..DGX_V1_BASELINE
        };
        let identity = if let Some(parameters) = &self.consensus {
            parameters.apply(&mut constants)?;
            let mut identity = b"dg_xch/chain-definition/v2\0".to_vec();
            identity.extend(serde_json::to_vec(self).map_err(|error| error.to_string())?);
            identity
        } else {
            let mut identity = b"dg_xch/chain-definition/v1\0".to_vec();
            identity
                .extend(serde_json::to_vec(&(self, constants)).map_err(|error| error.to_string())?);
            identity
        };
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchConsensus {
    pub version: u16,
    pub development: bool,
    pub pos2_activation_height: u32,
    pub difficulty_starting: u64,
    pub difficulty_constant_factor_bits: u8,
    pub sub_slot_iters_starting: u64,
    pub sub_slot_time_target: u64,
    pub plot_filter_bits: u8,
    pub plot_filter_v2_bits: u8,
    pub plot_size_v2: u8,
    pub min_plot_strength: u8,
    pub max_plot_strength: u8,
}

impl Default for LaunchConsensus {
    fn default() -> Self {
        Self {
            version: 2,
            development: false,
            pos2_activation_height: 0,
            difficulty_starting: 7,
            difficulty_constant_factor_bits: 67,
            sub_slot_iters_starting: 1 << 27,
            sub_slot_time_target: 600,
            plot_filter_bits: 9,
            plot_filter_v2_bits: 5,
            plot_size_v2: 28,
            min_plot_strength: 2,
            max_plot_strength: 32,
        }
    }
}

impl LaunchConsensus {
    fn apply(&self, constants: &mut ConsensusConstants) -> Result<(), String> {
        if self.version != 2
            || self.difficulty_starting == 0
            || !(1..=96).contains(&self.difficulty_constant_factor_bits)
            || !(128..=1 << 48).contains(&self.sub_slot_iters_starting)
            || !self
                .sub_slot_iters_starting
                .is_multiple_of(u64::from(constants.num_sps_sub_slot))
            || !(30..=86_400).contains(&self.sub_slot_time_target)
            || self.plot_filter_bits > 32
            || self.plot_filter_v2_bits > 32
            || !(18..=32).contains(&self.plot_size_v2)
            || !self.plot_size_v2.is_multiple_of(2)
            || self.min_plot_strength < 2
            || self.max_plot_strength > 32
            || self.min_plot_strength > self.max_plot_strength
            || self.min_plot_strength > self.plot_size_v2.min(28).saturating_sub(3)
        {
            return Err("invalid versioned chain consensus parameters".into());
        }
        if !self.development && self.plot_size_v2 < 28 {
            return Err("production chains require PoS2 k28 or larger".into());
        }
        constants.difficulty_starting = self.difficulty_starting;
        constants.difficulty_constant_factor = 1u128 << self.difficulty_constant_factor_bits;
        constants.sub_slot_iters_starting = self.sub_slot_iters_starting;
        constants.sub_slot_time_target = self.sub_slot_time_target;
        constants.number_zero_bits_plot_filter = self.plot_filter_bits;
        constants.number_zero_bits_plot_filter_v2 = self.plot_filter_v2_bits;
        constants.plot_size_v2 = self.plot_size_v2;
        constants.min_plot_strength = self.min_plot_strength;
        constants.max_plot_strength = self.max_plot_strength;
        constants.soft_fork2_height = 0;
        constants.soft_fork3_height = 0;
        constants.soft_fork8_height = 0;
        constants.soft_fork9_height = 0;
        constants.hard_fork_height = 0;
        constants.hard_fork_fix_height = 0;
        constants.hard_fork2_height = self.pos2_activation_height;
        constants.plot_filter_128_height = u32::MAX;
        constants.plot_filter_64_height = u32::MAX;
        constants.plot_filter_32_height = u32::MAX;
        constants.plot_filter_v2_first_adjustment_height = u32::MAX;
        constants.plot_filter_v2_second_adjustment_height = u32::MAX;
        constants.plot_filter_v2_third_adjustment_height = u32::MAX;
        constants.is_testnet = self.development;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainSelection {
    Chia(ChiaNetwork),
    Dgx,
    Custom(ChainDefinition),
}

impl Default for ChainSelection {
    fn default() -> Self {
        Self::Chia(ChiaNetwork::Mainnet)
    }
}

impl serde::Serialize for ChainSelection {
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        match self {
            Self::Chia(network) => serde::Serialize::serialize(network, serializer),
            Self::Dgx => serializer.serialize_str("dgx"),
            Self::Custom(definition) => serde::Serialize::serialize(definition, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ChainSelection {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Selection {
            Network(String),
            Custom(ChainDefinition),
        }
        match Selection::deserialize(deserializer)? {
            Selection::Network(network) => {
                Self::from_config(&network, None).map_err(serde::de::Error::custom)
            }
            Selection::Custom(definition) => {
                definition.constants().map_err(serde::de::Error::custom)?;
                Ok(Self::Custom(definition))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedChain {
    pub constants: ConsensusConstants,
    pub network_id: String,
    pub handshake_network_id: String,
    pub allows_bootstrap: bool,
}

impl ChainSelection {
    pub fn from_config(
        network: &str,
        definition: Option<&ChainDefinition>,
    ) -> Result<Self, String> {
        if let Some(definition) = definition {
            if network != definition.network_id {
                return Err("network id does not match chain definition".into());
            }
            definition.constants()?;
            Ok(Self::Custom(definition.clone()))
        } else if network == "dgx" {
            Ok(Self::Dgx)
        } else {
            ChiaNetwork::from_str(network).map(Self::Chia)
        }
    }

    pub fn resolve(&self) -> Result<ResolvedChain, String> {
        match self {
            Self::Chia(network) => {
                let value = serde_json::to_value(network).map_err(|error| error.to_string())?;
                let network_id = value
                    .as_str()
                    .ok_or("invalid Chia network name")?
                    .to_owned();
                Ok(ResolvedChain {
                    constants: ConsensusConstants::from(*network),
                    handshake_network_id: network_id.clone(),
                    network_id,
                    allows_bootstrap: false,
                })
            }
            Self::Dgx => Self::Custom(ChainDefinition::dgx()).resolve(),
            Self::Custom(definition) => {
                let constants = definition.constants()?;
                Ok(ResolvedChain {
                    handshake_network_id: format!(
                        "{}-{}",
                        definition.network_id,
                        hex::encode(constants.genesis_challenge)
                    ),
                    network_id: definition.network_id.clone(),
                    constants,
                    allows_bootstrap: true,
                })
            }
        }
    }

    pub fn constants(&self) -> Result<ConsensusConstants, String> {
        self.resolve().map(|chain| chain.constants)
    }

    pub fn handshake_network_id(&self) -> Result<String, String> {
        self.resolve().map(|chain| chain.handshake_network_id)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/consensus/chain_definition.rs"]
mod tests;
