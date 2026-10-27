//! Immutable per-block Falcon authorization context for the shared consensus engine.
use crate::primitives::{ConsensusPrimitives, NativePrimitives};
use dg_xch_core::blockchain::{
    class_group_element::ClassgroupElement,
    full_block::FullBlock,
    proof_of_space::ProofOfSpace,
    sized_bytes::{Bytes32, Bytes48, Bytes96},
    spend_bundle_conditions::SpendBundleConditions,
    vdf_info::VdfInfo,
    vdf_proof::VdfProof,
};
use dg_xch_core::consensus::{
    block_generator::{BlockGeneratorInput, validate_block_authorization},
    constants::ConsensusConstants,
};
use dg_xch_core::errors::ChiaError;
use dg_xch_core::signatures::{
    AuthorizationWitness, SignatureProfile,
    falcon_block::{FalconBlock, public_key_id},
};
use std::{io::Error, sync::Arc};

#[derive(Clone, Default)]
pub struct FalconPrimitives {
    context: Option<Arc<FalconBlock>>,
}

impl FalconPrimitives {
    pub fn new(block: FalconBlock, constants: &ConsensusConstants) -> Result<Self, Error> {
        if constants.signature_profile != SignatureProfile::FnDsa512V2 {
            return Err(Error::other("Falcon witness on a different chain profile"));
        }
        block.check_shape(constants.genesis_challenge)?;
        for proof in &block.header {
            if !proof.verify(
                block.genesis,
                &public_key_id(&proof.public_key),
                &proof.message,
                &proof.reference(block.genesis),
            ) {
                return Err(Error::other("invalid Falcon header signature"));
            }
        }
        Ok(Self {
            context: Some(Arc::new(block)),
        })
    }

    pub fn block(&self) -> Option<&FalconBlock> {
        self.context.as_deref()
    }
}

impl ConsensusPrimitives for FalconPrimitives {
    fn signature_profile(&self) -> SignatureProfile {
        SignatureProfile::FnDsa512V2
    }
    fn check_block_context(&self, block: &FullBlock) -> bool {
        self.context
            .as_ref()
            .is_some_and(|context| context.block == *block)
    }
    fn verify_header_signature(&self, key: &Bytes48, message: &[u8], signature: &Bytes96) -> bool {
        self.context.as_ref().is_some_and(|context| {
            context
                .header
                .iter()
                .any(|proof| proof.verify(context.genesis, key, message, signature))
        })
    }
    fn run_block_generator(
        &self,
        input: &BlockGeneratorInput,
    ) -> Result<SpendBundleConditions, ChiaError> {
        NativePrimitives.run_block_generator(input)
    }
    fn verify_block_aggregate_signature(
        &self,
        conditions: &SpendBundleConditions,
        aggregate: &Bytes96,
        constants: &ConsensusConstants,
    ) -> Result<(), ChiaError> {
        let context = self
            .context
            .as_ref()
            .ok_or(ChiaError::BadAggregateSignature)?;
        if constants.signature_profile != SignatureProfile::FnDsa512V2
            || *aggregate
                != dg_xch_core::signatures::falcon_condition::transaction_reference(
                    constants.genesis_challenge,
                    &conditions.falcon_signatures,
                )
        {
            return Err(ChiaError::BadAggregateSignature);
        }
        validate_block_authorization(
            conditions,
            &AuthorizationWitness::FalconDirect(&context.transactions),
            constants,
        )?;
        Ok(())
    }
    fn verify_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        NativePrimitives.verify_vdf(constants, input, info, proof, target)
    }
    fn verify_vdf_serial(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        NativePrimitives.verify_vdf_serial(constants, input, info, proof, target)
    }
    fn proof_of_space_quality(
        &self,
        proof: &ProofOfSpace,
        constants: &ConsensusConstants,
        challenge: Bytes32,
        signage: Bytes32,
        height: u32,
    ) -> Option<Bytes32> {
        NativePrimitives.proof_of_space_quality(proof, constants, challenge, signage, height)
    }
}
