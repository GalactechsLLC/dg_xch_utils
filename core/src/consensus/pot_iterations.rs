use crate::blockchain::sized_bytes::Bytes32;
use crate::consensus::constants::ConsensusConstants;
use crate::constants::TWO_POW_256;
use crate::utils::hash_256;
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use std::cmp::max;
use std::io::{Error, ErrorKind};
use std::ops::Mul;

pub fn is_overflow_block(
    constants: &ConsensusConstants,
    signage_point_index: u8,
) -> Result<bool, Error> {
    if u32::from(signage_point_index) >= constants.num_sps_sub_slot {
        Err(Error::new(ErrorKind::InvalidData, "SP index too high"))
    } else {
        Ok(u64::from(signage_point_index)
            >= u64::from(constants.num_sps_sub_slot) - constants.num_sp_intervals_extra)
    }
}

pub fn calculate_sp_interval_iters(
    constants: &ConsensusConstants,
    sub_slot_iters: u64,
) -> Result<u64, Error> {
    if sub_slot_iters % u64::from(constants.num_sps_sub_slot) != 0 {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("Invalid SubSlot Iterations: {sub_slot_iters}"),
        ))
    } else {
        Ok(sub_slot_iters / u64::from(constants.num_sps_sub_slot))
    }
}

pub fn calculate_sp_iters(
    constants: &ConsensusConstants,
    sub_slot_iters: u64,
    signage_point_index: u8,
) -> Result<u64, Error> {
    if u32::from(signage_point_index) >= constants.num_sps_sub_slot {
        Err(Error::new(ErrorKind::InvalidData, "SP index too high"))
    } else {
        Ok(
            calculate_sp_interval_iters(constants, sub_slot_iters)?
                * u64::from(signage_point_index),
        )
    }
}

pub fn calculate_ip_iters(
    constants: &ConsensusConstants,
    sub_slot_iters: u64,
    signage_point_index: u8,
    required_iters: u64,
) -> Result<u64, Error> {
    let sp_iters = calculate_sp_iters(constants, sub_slot_iters, signage_point_index)?;
    let sp_interval_iters = calculate_sp_interval_iters(constants, sub_slot_iters)?;
    if sp_iters % sp_interval_iters != 0 || sp_iters >= sub_slot_iters {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("Invalid sp iters {sp_iters} for this ssi {sub_slot_iters}"),
        ))
    } else if required_iters >= sp_interval_iters || required_iters == 0 {
        Err(Error::new(ErrorKind::InvalidData, format!("Required iters {required_iters} is not below the sp interval iters {sp_interval_iters}, {sub_slot_iters} or not > 0.")))
    } else {
        Ok(
            (sp_iters + constants.num_sp_intervals_extra * sp_interval_iters + required_iters)
                % sub_slot_iters,
        )
    }
}

#[must_use]
pub fn expected_plot_size(k: u8) -> u64 {
    ((2 * u64::from(k)) + 1) * 2u64.pow(u32::from(k) - 1)
}

#[must_use]
pub fn calculate_iterations_quality(
    difficulty_constant_factor: u128,
    quality_string: Bytes32,
    size: u8,
    difficulty: u64,
    cc_sp_output_hash: Bytes32,
) -> u64 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(quality_string);
    to_hash.extend(cc_sp_output_hash);
    let hashed = hash_256(to_hash);
    let quality_int = BigUint::from_bytes_be(hashed.as_slice());
    let difficulty_int = BigUint::from(difficulty);
    let difficulty_constant_factor_int = BigUint::from(difficulty_constant_factor);
    let top: BigUint = difficulty_int * difficulty_constant_factor_int * quality_int;
    let bottom: BigUint = (*TWO_POW_256).clone().mul(expected_plot_size(size));
    let bigint: BigUint = top / bottom;
    if bigint.gt(&u64::MAX.into()) {
        return u64::MAX;
    }
    max(1, bigint.to_u64().unwrap_or(0))
}
