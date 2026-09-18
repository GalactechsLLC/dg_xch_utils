use crate::blockchain::sized_bytes::Bytes32;
use crate::consensus::constants::ConsensusConstants;
use num_bigint::BigInt;
use num_traits::ToPrimitive;

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConsensusOverrides {
    pub slot_blocks_target: Option<u32>,
    pub min_blocks_per_challenge_block: Option<u8>,
    pub max_sub_slot_blocks: Option<u32>,
    pub num_sps_sub_slot: Option<u32>,
    pub sub_slot_iters_starting: Option<u64>,
    pub difficulty_constant_factor: Option<u128>,
    pub difficulty_starting: Option<u64>,
    pub difficulty_change_max_factor: Option<u32>,
    pub sub_epoch_blocks: Option<u32>,
    pub epoch_blocks: Option<u32>,
    pub significant_bits: Option<BigInt>,
    pub discriminant_size_bits: Option<BigInt>,
    pub number_zero_bits_plot_filter: Option<u8>,
    pub min_plot_size: Option<u8>,
    pub max_plot_size: Option<u8>,
    pub sub_slot_time_target: Option<BigInt>,
    pub num_sp_intervals_extra: Option<u64>,
    pub max_future_time: Option<BigInt>,
    pub max_future_time2: Option<BigInt>,
    pub number_of_timestamps: Option<BigInt>,
    pub genesis_challenge: Option<Bytes32>,
    pub agg_sig_me_additional_data: Option<Bytes32>,
    pub genesis_pre_farm_pool_puzzle_hash: Option<Bytes32>,
    pub genesis_pre_farm_farmer_puzzle_hash: Option<Bytes32>,
    pub max_vdf_witness_size: Option<BigInt>,
    pub mempool_block_buffer: Option<BigInt>,
    pub max_coin_amount: Option<BigInt>,
    pub max_block_cost_clvm: Option<BigInt>,
    pub cost_per_byte: Option<BigInt>,
    pub weight_proof_threshold: Option<u8>,
    pub weight_proof_recent_blocks: Option<u32>,
    pub max_block_count_per_requests: Option<u32>,
    pub blocks_cache_size: Option<u32>,
    pub max_generator_size: Option<u32>,
    pub max_generator_ref_list_size: Option<u32>,
    pub pool_sub_slot_iters: Option<u64>,
    pub soft_fork2_height: Option<u32>,
    pub soft_fork3_height: Option<u32>,
    pub hard_fork_height: Option<u32>,
    pub hard_fork_fix_height: Option<u32>,
    pub hard_fork2_height: Option<u32>,
    pub plot_filter_128_height: Option<u32>,
    pub plot_filter_64_height: Option<u32>,
    pub plot_filter_32_height: Option<u32>,
    pub number_zero_bits_plot_filter_v2: Option<u8>,
    pub plot_size_v2: Option<u8>,
    pub min_plot_strength: Option<u8>,
    pub max_plot_strength: Option<u8>,
    pub plot_v1_phase_out_epoch_bits: Option<u8>,
    pub plot_filter_v2_first_adjustment_height: Option<u32>,
    pub plot_filter_v2_second_adjustment_height: Option<u32>,
    pub plot_filter_v2_third_adjustment_height: Option<u32>,
    pub bech32_prefix: Option<String>,
    pub is_testnet: Option<bool>,
}

/// Fold optional per-field overrides onto a base `ConsensusConstants`. Only `Some` fields are
/// applied; `BigInt`-typed overrides are narrowed to the constant's integer width and keep the base
/// value if they do not fit. `bech32_prefix` is a `&'static str` and is not overridable.
#[must_use]
pub fn apply_overrides(mut c: ConsensusConstants, o: &ConsensusOverrides) -> ConsensusConstants {
    if let Some(v) = o.slot_blocks_target {
        c.slot_blocks_target = v;
    }
    if let Some(v) = o.min_blocks_per_challenge_block {
        c.min_blocks_per_challenge_block = v;
    }
    if let Some(v) = o.max_sub_slot_blocks {
        c.max_sub_slot_blocks = v;
    }
    if let Some(v) = o.num_sps_sub_slot {
        c.num_sps_sub_slot = v;
    }
    if let Some(v) = o.sub_slot_iters_starting {
        c.sub_slot_iters_starting = v;
    }
    if let Some(v) = o.difficulty_constant_factor {
        c.difficulty_constant_factor = v;
    }
    if let Some(v) = o.difficulty_starting {
        c.difficulty_starting = v;
    }
    if let Some(v) = o.difficulty_change_max_factor {
        c.difficulty_change_max_factor = v;
    }
    if let Some(v) = o.sub_epoch_blocks {
        c.sub_epoch_blocks = v;
    }
    if let Some(v) = o.epoch_blocks {
        c.epoch_blocks = v;
    }
    if let Some(v) = &o.significant_bits {
        c.significant_bits = v.to_u64().unwrap_or(c.significant_bits);
    }
    if let Some(v) = &o.discriminant_size_bits {
        c.discriminant_size_bits = v.to_u64().unwrap_or(c.discriminant_size_bits);
    }
    if let Some(v) = o.number_zero_bits_plot_filter {
        c.number_zero_bits_plot_filter = v;
    }
    if let Some(v) = o.min_plot_size {
        c.min_plot_size = v;
    }
    if let Some(v) = o.max_plot_size {
        c.max_plot_size = v;
    }
    if let Some(v) = &o.sub_slot_time_target {
        c.sub_slot_time_target = v.to_u64().unwrap_or(c.sub_slot_time_target);
    }
    if let Some(v) = o.num_sp_intervals_extra {
        c.num_sp_intervals_extra = v;
    }
    if let Some(v) = &o.max_future_time {
        c.max_future_time = v.to_u64().unwrap_or(c.max_future_time);
    }
    if let Some(v) = &o.max_future_time2 {
        c.max_future_time2 = v.to_u64().unwrap_or(c.max_future_time2);
    }
    if let Some(v) = &o.number_of_timestamps {
        c.number_of_timestamps = v.to_u64().unwrap_or(c.number_of_timestamps);
    }
    if let Some(v) = o.genesis_challenge {
        c.genesis_challenge = v;
    }
    if let Some(v) = o.agg_sig_me_additional_data {
        c.agg_sig_me_additional_data = v;
    }
    if let Some(v) = o.genesis_pre_farm_pool_puzzle_hash {
        c.genesis_pre_farm_pool_puzzle_hash = v;
    }
    if let Some(v) = o.genesis_pre_farm_farmer_puzzle_hash {
        c.genesis_pre_farm_farmer_puzzle_hash = v;
    }
    if let Some(v) = &o.max_vdf_witness_size {
        c.max_vdf_witness_size = v.to_u64().unwrap_or(c.max_vdf_witness_size);
    }
    if let Some(v) = &o.mempool_block_buffer {
        c.mempool_block_buffer = v.to_u64().unwrap_or(c.mempool_block_buffer);
    }
    if let Some(v) = &o.max_coin_amount {
        c.max_coin_amount = v.to_u64().unwrap_or(c.max_coin_amount);
    }
    if let Some(v) = &o.max_block_cost_clvm {
        c.max_block_cost_clvm = v.to_u64().unwrap_or(c.max_block_cost_clvm);
    }
    if let Some(v) = &o.cost_per_byte {
        c.cost_per_byte = v.to_u64().unwrap_or(c.cost_per_byte);
    }
    if let Some(v) = o.weight_proof_threshold {
        c.weight_proof_threshold = v;
    }
    if let Some(v) = o.weight_proof_recent_blocks {
        c.weight_proof_recent_blocks = v;
    }
    if let Some(v) = o.max_block_count_per_requests {
        c.max_block_count_per_requests = v;
    }
    if let Some(v) = o.blocks_cache_size {
        c.blocks_cache_size = v;
    }
    if let Some(v) = o.max_generator_size {
        c.max_generator_size = v;
    }
    if let Some(v) = o.max_generator_ref_list_size {
        c.max_generator_ref_list_size = v;
    }
    if let Some(v) = o.pool_sub_slot_iters {
        c.pool_sub_slot_iters = v;
    }
    if let Some(v) = o.soft_fork2_height {
        c.soft_fork2_height = v;
    }
    if let Some(v) = o.soft_fork3_height {
        c.soft_fork3_height = v;
    }
    if let Some(v) = o.hard_fork_height {
        c.hard_fork_height = v;
    }
    if let Some(v) = o.hard_fork_fix_height {
        c.hard_fork_fix_height = v;
    }
    if let Some(v) = o.hard_fork2_height {
        c.hard_fork2_height = v;
    }
    if let Some(v) = o.plot_filter_128_height {
        c.plot_filter_128_height = v;
    }
    if let Some(v) = o.plot_filter_64_height {
        c.plot_filter_64_height = v;
    }
    if let Some(v) = o.plot_filter_32_height {
        c.plot_filter_32_height = v;
    }
    if let Some(v) = o.number_zero_bits_plot_filter_v2 {
        c.number_zero_bits_plot_filter_v2 = v;
    }
    if let Some(v) = o.plot_size_v2 {
        c.plot_size_v2 = v;
    }
    if let Some(v) = o.min_plot_strength {
        c.min_plot_strength = v;
    }
    if let Some(v) = o.max_plot_strength {
        c.max_plot_strength = v;
    }
    if let Some(v) = o.plot_v1_phase_out_epoch_bits {
        c.plot_v1_phase_out_epoch_bits = v;
    }
    if let Some(v) = o.plot_filter_v2_first_adjustment_height {
        c.plot_filter_v2_first_adjustment_height = v;
    }
    if let Some(v) = o.plot_filter_v2_second_adjustment_height {
        c.plot_filter_v2_second_adjustment_height = v;
    }
    if let Some(v) = o.plot_filter_v2_third_adjustment_height {
        c.plot_filter_v2_third_adjustment_height = v;
    }
    if let Some(v) = o.is_testnet {
        c.is_testnet = v;
    }
    c
}

#[cfg(test)]
#[path = "../../tests/unit/consensus/overrides/tests.rs"]
mod tests;
