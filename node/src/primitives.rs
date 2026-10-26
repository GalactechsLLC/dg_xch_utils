use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorInput, execute_block_generator_result, validate_block_aggregate_signature,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::errors::ChiaError;
use dg_xch_core::signatures::AuthorizationWitness;

// The consensus primitives the block-validation engine invokes, behind one swap seam. The engine
// binds to this trait, never to a concrete implementation.
pub trait ConsensusPrimitives {
    /// Bind any detached evidence before validation or parallel body precomputation.
    fn for_block(
        &self,
        _block: &dg_xch_core::blockchain::full_block::FullBlock,
        _constants: &ConsensusConstants,
    ) -> Result<Option<Self>, ChiaError>
    where
        Self: Sized,
    {
        Ok(None)
    }
    fn signature_profile(&self) -> dg_xch_core::signatures::SignatureProfile {
        dg_xch_core::signatures::SignatureProfile::Bls12381Aug
    }

    fn verify_header_signature(
        &self,
        public_key: &dg_xch_core::blockchain::sized_bytes::Bytes48,
        message: &[u8],
        signature: &Bytes96,
    ) -> bool {
        dg_xch_core::consensus::block_header_validation::bls_verify(public_key, message, signature)
    }

    /// Bind an attached authorization context to exactly the block being validated.
    fn check_block_context(&self, block: &dg_xch_core::blockchain::full_block::FullBlock) -> bool {
        block.authorization.is_none()
    }

    /// Run a block's transactions generator, returning its conditions and cost.
    ///
    /// # Errors
    /// Propagates the generator's `ChiaError` (invalid solution, cost exceeded,
    /// too many generator refs, runtime error).
    fn run_block_generator(
        &self,
        input: &BlockGeneratorInput,
    ) -> Result<SpendBundleConditions, ChiaError>;

    /// Aggregate-verify a block's full AGG_SIG signature set.
    ///
    /// # Errors
    /// Returns `ChiaError::BadAggregateSignature` on a failed or malformed
    /// aggregate signature.
    fn verify_block_aggregate_signature(
        &self,
        conditions: &SpendBundleConditions,
        aggregate: &Bytes96,
        constants: &ConsensusConstants,
    ) -> Result<(), ChiaError>;

    /// Verify transaction authorization under the active chain rules. The default
    /// preserves existing implementers' BLS verification hook; future modes need
    /// an explicit consensus implementation and cannot fall back to BLS.
    fn verify_block_authorization(
        &self,
        conditions: &SpendBundleConditions,
        witness: &AuthorizationWitness<'_>,
        constants: &ConsensusConstants,
    ) -> Result<(), ChiaError> {
        match witness {
            AuthorizationWitness::BlsAggregate(aggregate) => {
                self.verify_block_aggregate_signature(conditions, aggregate, constants)
            }
            _ => Err(ChiaError::BadAggregateSignature),
        }
    }

    fn verify_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool;

    /// [`Self::verify_vdf`] with any internal parallelism disabled — same result, verified on the
    /// calling thread only. The window drain calls this when its worker pool already saturates
    /// every core. The default is `verify_vdf` itself.
    fn verify_vdf_serial(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        self.verify_vdf(constants, input, info, proof, target)
    }

    fn proof_of_space_quality(
        &self,
        pos: &ProofOfSpace,
        constants: &ConsensusConstants,
        original_challenge_hash: Bytes32,
        signage_point: Bytes32,
        height: u32,
    ) -> Option<Bytes32>;
}

// Delegates each call point to dg_xch's existing native primitive functions.
pub struct NativePrimitives;

impl ConsensusPrimitives for NativePrimitives {
    fn run_block_generator(
        &self,
        input: &BlockGeneratorInput,
    ) -> Result<SpendBundleConditions, ChiaError> {
        execute_block_generator_result(input)
    }

    fn verify_block_aggregate_signature(
        &self,
        conditions: &SpendBundleConditions,
        aggregate: &Bytes96,
        constants: &ConsensusConstants,
    ) -> Result<(), ChiaError> {
        validate_block_aggregate_signature(conditions, aggregate, constants)
    }

    fn verify_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        dg_xch_vdf::validate_vdf_info(constants, input, info, proof, target)
    }

    fn verify_vdf_serial(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        dg_xch_vdf::validate_vdf_info_serial(constants, input, info, proof, target)
    }

    fn proof_of_space_quality(
        &self,
        pos: &ProofOfSpace,
        constants: &ConsensusConstants,
        original_challenge_hash: Bytes32,
        signage_point: Bytes32,
        height: u32,
    ) -> Option<Bytes32> {
        dg_xch_pos::verify_and_get_quality_string(
            pos,
            constants,
            original_challenge_hash,
            signage_point,
            height,
        )
    }
}

/// Generator step bound to the seam rather than a concrete type.
///
/// # Errors
/// Propagates whatever `ConsensusPrimitives::run_block_generator` returns.
pub fn run_block_generator<P: ConsensusPrimitives>(
    primitives: &P,
    input: &BlockGeneratorInput,
) -> Result<SpendBundleConditions, ChiaError> {
    primitives.run_block_generator(input)
}

#[cfg(test)]
#[path = "../tests/unit/primitives/tests.rs"]
mod tests;
