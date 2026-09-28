use crate::error::SimError;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes100};
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::traits::SizedBytes;
use dg_xch_vdf::proof::prove_result;

/// Serialized size of a class group element, and so of each half of a one-wesolowski proof. Fixed
/// by `FORM_SIZE`, independent of the discriminant actually in use.
const FORM_BYTES: usize = 100;

/// Run one VDF and return what a block needs to carry it.
///
/// `discriminant_size_bits` sets the group size the prover and verifier run over; a simulated
/// chain shrinks it so a proof costs microseconds rather than seconds.
pub fn prove_vdf(
    challenge: Bytes32,
    input: &ClassgroupElement,
    iterations: u64,
    discriminant_size_bits: u64,
) -> Result<(VdfInfo, VdfProof), SimError> {
    let bits = usize::try_from(discriminant_size_bits).map_err(|_| {
        SimError::Invariant(format!("discriminant {discriminant_size_bits} too wide"))
    })?;
    let bytes = prove_result(&challenge.bytes(), &input.data.bytes(), bits, iterations)?;
    if bytes.len() < FORM_BYTES {
        return Err(SimError::Invariant(format!(
            "vdf returned {} bytes, need at least {FORM_BYTES}",
            bytes.len()
        )));
    }
    let (output, witness) = bytes.split_at(FORM_BYTES);
    Ok((
        VdfInfo {
            challenge,
            number_of_iterations: iterations,
            output: ClassgroupElement {
                data: Bytes100::parse(output).map_err(|e| {
                    SimError::Invariant(format!("vdf output is not a class group element: {e:?}"))
                })?,
            },
        },
        VdfProof {
            witness_type: 0,
            witness: UnsizedBytes::new(witness.to_vec()),
            normalized_to_identity: false,
        },
    ))
}

#[cfg(test)]
#[path = "../tests/unit/timelord/tests.rs"]
mod tests;
