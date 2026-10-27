//! Runtime chain policy for the regular full-node service. Each validation binds
//! immutable evidence from its own block; no global witness cache is involved.
use crate::primitives::{ConsensusPrimitives, NativePrimitives};
use dg_xch_core::{
    blockchain::{
        class_group_element::ClassgroupElement,
        full_block::FullBlock,
        proof_of_space::ProofOfSpace,
        sized_bytes::{Bytes32, Bytes48, Bytes96},
        spend_bundle_conditions::SpendBundleConditions,
        unfinished_block::UnfinishedBlock,
        vdf_info::VdfInfo,
        vdf_proof::VdfProof,
    },
    consensus::{
        block_generator::{BlockGeneratorInput, validate_block_authorization},
        constants::ConsensusConstants,
    },
    errors::ChiaError,
    signatures::{SignatureProfile, extension::VerifiedFalconAuthorization},
};
use std::sync::Arc;

#[derive(Clone)]
pub struct ChainPrimitives {
    profile: SignatureProfile,
    authorization: Option<Arc<VerifiedFalconAuthorization>>,
}
impl ChainPrimitives {
    pub fn new(constants: &ConsensusConstants) -> Result<Self, std::io::Error> {
        if constants.falcon_aggregation.is_some() && !cfg!(feature = "labrador") {
            return Err(std::io::Error::other(
                "this chain requires LaBRADOR support",
            ));
        }
        if !constants.signature_profile.is_bls() && !cfg!(feature = "falcon") {
            return Err(std::io::Error::other(
                "this node was built without Falcon support",
            ));
        }
        Ok(Self {
            profile: constants.signature_profile,
            authorization: None,
        })
    }
    pub fn unfinished(
        block: &UnfinishedBlock,
        constants: &ConsensusConstants,
    ) -> Result<Self, ChiaError> {
        let mut result = Self::new(constants).map_err(|_| ChiaError::BadAggregateSignature)?;
        if result.profile.is_bls() {
            if !block.authorization.is_none() {
                return Err(ChiaError::BadAggregateSignature);
            }
        } else {
            let auth = block
                .authorization
                .as_ref()
                .ok_or(ChiaError::BadAggregateSignature)?;
            let verified = auth
                .check_unfinished(block, constants.genesis_challenge)
                .map_err(|_| ChiaError::BadAggregateSignature)?;
            result.authorization = Some(Arc::new(verified));
        }
        Ok(result)
    }
}
impl ConsensusPrimitives for ChainPrimitives {
    fn signature_profile(&self) -> SignatureProfile {
        self.profile
    }
    fn for_block(
        &self,
        block: &FullBlock,
        constants: &ConsensusConstants,
    ) -> Result<Option<Self>, ChiaError> {
        if self.profile != constants.signature_profile {
            return Err(ChiaError::BadAggregateSignature);
        }
        let mut result = self.clone();
        if self.profile.is_bls() {
            if !block.authorization.is_none() {
                return Err(ChiaError::BadAggregateSignature);
            }
        } else {
            let auth = block
                .authorization
                .as_ref()
                .ok_or(ChiaError::BadAggregateSignature)?;
            let verified = auth
                .check_full(block, constants.genesis_challenge)
                .map_err(|_| ChiaError::BadAggregateSignature)?;
            result.authorization = Some(Arc::new(verified));
        }
        Ok(Some(result))
    }
    fn check_block_context(&self, block: &FullBlock) -> bool {
        if self.profile.is_bls() {
            block.authorization.is_none()
        } else {
            self.authorization
                .as_deref()
                .is_some_and(|a| Some(a.authorization()) == block.authorization.as_ref())
        }
    }
    fn verify_header_signature(&self, key: &Bytes48, message: &[u8], signature: &Bytes96) -> bool {
        if self.profile.is_bls() {
            NativePrimitives.verify_header_signature(key, message, signature)
        } else {
            self.authorization
                .as_ref()
                .is_some_and(|a| a.verify_header(key, message, signature))
        }
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
        if self.profile != constants.signature_profile {
            return Err(ChiaError::BadAggregateSignature);
        }
        if self.profile.is_bls() {
            return NativePrimitives
                .verify_block_aggregate_signature(conditions, aggregate, constants);
        }
        let auth = self
            .authorization
            .as_ref()
            .ok_or(ChiaError::BadAggregateSignature)?;
        if *aggregate
            != dg_xch_core::signatures::falcon_condition::transaction_reference(
                constants.genesis_challenge,
                &conditions.falcon_signatures,
            )
        {
            return Err(ChiaError::BadAggregateSignature);
        }
        validate_block_authorization(
            conditions,
            &auth
                .authorization()
                .transaction_witness()
                .map_err(|_| ChiaError::BadAggregateSignature)?,
            constants,
        )
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
