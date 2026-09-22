use dg_xch_core::blockchain::proof_of_space::{
    ProofOfSpace, calculate_plot_filter_input, calculate_prefix_bits, calculate_prefix_bits_v2,
    is_proof_version_active, passes_plot_filter_input,
};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::traits::SizedBytes;
use log::warn;

pub use dg_xch_pos_common as pos_common;
pub use dg_xch_pos1 as pos1;
pub use dg_xch_pos1::*;
pub use dg_xch_pos2 as pos2;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

#[must_use]
pub fn verify_and_get_quality_string(
    pos: &ProofOfSpace,
    constants: &ConsensusConstants,
    original_challenge_hash: Bytes32,
    signage_point: Bytes32,
    height: u32,
) -> Option<Bytes32> {
    verify_and_get_quality_string_with_context(
        pos,
        constants,
        original_challenge_hash,
        signage_point,
        height,
        height,
    )
}

#[must_use]
pub fn verify_and_get_quality_string_with_context(
    pos: &ProofOfSpace,
    constants: &ConsensusConstants,
    original_challenge_hash: Bytes32,
    signage_point: Bytes32,
    height: u32,
    previous_transaction_height: u32,
) -> Option<Bytes32> {
    if !is_proof_version_active(pos.version, previous_transaction_height, constants) {
        return None;
    }
    verify_and_get_quality_string_without_activation(
        pos,
        constants,
        original_challenge_hash,
        signage_point,
        height,
    )
}

#[must_use]
pub fn verify_and_get_quality_string_without_activation(
    pos: &ProofOfSpace,
    constants: &ConsensusConstants,
    original_challenge_hash: Bytes32,
    signage_point: Bytes32,
    height: u32,
) -> Option<Bytes32> {
    if pos.version > 1 {
        return None;
    }
    if pos.pool_public_key.is_none() && pos.pool_contract_puzzle_hash.is_none() {
        warn!(
            "Failed to Verify ProofOfSpace: null value for both pool_public_key and pool_contract_puzzle_hash"
        );
        return None;
    }
    if pos.pool_public_key.is_some() && pos.pool_contract_puzzle_hash.is_some() {
        warn!(
            "Failed to Verify ProofOfSpace: Non Null value for both for pool_public_key and pool_contract_puzzle_hash"
        );
        return None;
    }
    if pos.version == 0 {
        if pos.size < constants.min_plot_size {
            warn!("Failed to Verify ProofOfSpace: Plot failed MIN_PLOT_SIZE");
            return None;
        }
        if pos.size > constants.max_plot_size {
            warn!("Failed to Verify ProofOfSpace: Plot failed MAX_PLOT_SIZE");
            return None;
        }
    } else if pos.strength < constants.min_plot_strength
        || pos.strength > constants.max_plot_strength
    {
        warn!("Failed to Verify ProofOfSpace: strength outside the allowed range");
        return None;
    }
    if let Some(plot_id) = pos.get_plot_id() {
        let filter_input =
            calculate_plot_filter_input(plot_id, original_challenge_hash, signage_point);
        if pos.challenge != Bytes32::new(dg_xch_core::utils::hash_256(filter_input)) {
            warn!("Failed to Verify ProofOfSpace: New challenge is not challenge");
            return None;
        }
        // v1 and v2 plots run different filters on different schedules.
        let prefix_bits = if pos.version == 0 {
            calculate_prefix_bits(constants, height)
        } else {
            calculate_prefix_bits_v2(constants, height)
        };
        if !passes_plot_filter_input(prefix_bits, filter_input) {
            warn!("Failed to Verify ProofOfSpace: Plot Failed to Pass Filter");
            return None;
        }
        if pos.version == 0 {
            get_quality_string(pos, &plot_id)
        } else {
            get_quality_string_v2(pos, plot_id, constants)
        }
    } else {
        None
    }
}

/// Validate a v2 proof and return its quality string: the hash of the quality chain commitment the
/// proof is a witness to.
#[must_use]
pub fn get_quality_string_v2(
    pos: &ProofOfSpace,
    plot_id: Bytes32,
    constants: &ConsensusConstants,
) -> Option<Bytes32> {
    let params = match pos2::params::ProofParams::new(
        plot_id,
        constants.plot_size_v2,
        pos.strength,
        constants.is_testnet,
    ) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to Verify ProofOfSpace: {e}");
            return None;
        }
    };
    let validator = match pos2::validator::ProofValidator::new(params) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to Verify ProofOfSpace: {e}");
            return None;
        }
    };
    let fragments = validator.validate_packed_proof(pos.proof.as_ref(), pos.challenge)?;
    Some(pos2::quality::quality_hash(&fragments, pos.strength))
}
