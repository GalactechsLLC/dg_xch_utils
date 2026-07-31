use crate::error::{Error, Result};
use crate::proof::verify_vdf;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::constants::ConsensusConstants;

#[must_use]
pub fn default_classgroup_element() -> ClassgroupElement {
    ClassgroupElement::get_default_element()
}

#[must_use]
pub fn validate_vdf_info(
    constants: &ConsensusConstants,
    input_el: &ClassgroupElement,
    info: &VdfInfo,
    proof: &VdfProof,
    target_vdf_info: Option<&VdfInfo>,
) -> bool {
    validate_vdf_info_result(constants, input_el, info, proof, target_vdf_info).is_ok()
}

pub fn validate_vdf_info_result(
    constants: &ConsensusConstants,
    input_el: &ClassgroupElement,
    info: &VdfInfo,
    proof: &VdfProof,
    target_vdf_info: Option<&VdfInfo>,
) -> Result<()> {
    if target_vdf_info.is_some_and(|target| target != info) {
        return Err(Error::TargetVdfMismatch);
    }
    if u64::from(proof.witness_type) + 1 > constants.max_vdf_witness_size {
        return Err(Error::WitnessTooLarge {
            witness_type: proof.witness_type,
            max_vdf_witness_size: constants.max_vdf_witness_size,
        });
    }

    let mut proof_blob = Vec::with_capacity(100 + proof.witness.bytes.len());
    proof_blob.extend_from_slice(info.output.data.as_ref());
    proof_blob.extend_from_slice(proof.witness.as_slice());

    if verify_vdf(
        info.challenge.as_ref(),
        input_el.data.as_ref(),
        &proof_blob,
        constants.discriminant_size_bits as usize,
        info.number_of_iterations,
        u64::from(proof.witness_type),
    ) {
        Ok(())
    } else {
        Err(Error::InvalidForm)
    }
}

#[must_use]
pub fn validate_vdf_with_normalization(
    constants: &ConsensusConstants,
    input_el: &ClassgroupElement,
    info: &VdfInfo,
    proof: &VdfProof,
    target_vdf_info: Option<&VdfInfo>,
) -> bool {
    validate_vdf_with_normalization_result(constants, input_el, info, proof, target_vdf_info)
        .is_ok()
}

pub fn validate_vdf_with_normalization_result(
    constants: &ConsensusConstants,
    input_el: &ClassgroupElement,
    info: &VdfInfo,
    proof: &VdfProof,
    target_vdf_info: Option<&VdfInfo>,
) -> Result<()> {
    if proof.normalized_to_identity {
        validate_vdf_info_result(
            constants,
            &default_classgroup_element(),
            info,
            proof,
            target_vdf_info,
        )
    } else {
        validate_vdf_info_result(constants, input_el, info, proof, target_vdf_info)
    }
}
