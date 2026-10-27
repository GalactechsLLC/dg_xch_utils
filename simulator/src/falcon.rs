//! Falcon keys and detached proofs for the shared native plot/chain builder.
use crate::plots::PlotSigning;
use dg_xch_core::blockchain::{
    full_block::FullBlock,
    sized_bytes::{Bytes32, Bytes48, Bytes96},
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::signatures::{
    SignatureSigner,
    falcon::{FalconSignature, FalconSigner},
    falcon_block::{FalconBlock, FalconHeaderAuthorization, public_key_id},
};
use dg_xch_node::falcon::FalconPrimitives;
use std::{
    collections::BTreeMap,
    fmt,
    io::Error,
    sync::{Arc, Mutex},
};

type HeaderProofs = Arc<Mutex<BTreeMap<[u8; 96], FalconHeaderAuthorization>>>;

#[derive(Clone)]
pub struct FalconPlotKeys {
    plot: Arc<FalconSigner>,
    pool: Arc<FalconSigner>,
    genesis: Bytes32,
    proofs: HeaderProofs,
}
impl fmt::Debug for FalconPlotKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FalconPlotKeys")
            .field("plot_public_key_id", &self.plot_public_key())
            .finish_non_exhaustive()
    }
}
impl FalconPlotKeys {
    fn authorize(&self, signer: &FalconSigner, message: &[u8]) -> Result<Bytes96, Error> {
        let proof = FalconHeaderAuthorization::sign(signer, self.genesis, message)?;
        let reference = proof.reference(self.genesis);
        let mut proofs = self
            .proofs
            .lock()
            .map_err(|_| Error::other("signing context poisoned"))?;
        if proofs.len() >= 512 && !proofs.contains_key(&reference.const_bytes()) {
            return Err(Error::other("pending signature limit"));
        }
        proofs.entry(reference.const_bytes()).or_insert(proof);
        Ok(reference)
    }
}
impl PlotSigning for FalconPlotKeys {
    fn plot_public_key(&self) -> Bytes48 {
        public_key_id(&self.plot.public_key())
    }
    fn pool_public_key(&self) -> Bytes48 {
        public_key_id(&self.pool.public_key())
    }
    fn sign(&self, message: Bytes32) -> Result<Bytes96, Error> {
        self.authorize(&self.plot, message.as_ref())
    }
    fn sign_pool(&self, message: &[u8]) -> Result<Bytes96, Error> {
        self.authorize(&self.pool, message)
    }
    fn memo(&self) -> Vec<u8> {
        // Versioned 128-byte public memo: tag/padding, pool key commitment,
        // plot key commitment. Private keys are never embedded in Falcon plots.
        let mut memo = b"DGXFAL02".to_vec();
        memo.resize(32, 0);
        memo.extend_from_slice(self.pool_public_key().as_ref());
        memo.extend_from_slice(self.plot_public_key().as_ref());
        memo
    }
}

#[derive(Clone, Default)]
pub struct FalconFarming {
    proofs: HeaderProofs,
    transactions: Arc<Mutex<std::collections::HashMap<(Bytes32, u32), FalconSignature>>>,
}
impl FalconFarming {
    pub fn admit_signatures(
        &self,
        spend: &dg_xch_core::signatures::falcon_wallet::FalconSpendBundle,
        constants: &ConsensusConstants,
        height: u32,
    ) -> Result<(), Error> {
        use dg_xch_core::consensus::block_generator::{
            conditions_from_spend_bundle, validate_block_authorization,
        };
        use dg_xch_core::signatures::AuthorizationWitness;
        let conditions =
            conditions_from_spend_bundle(&spend.bundle, height, constants).map_err(Error::other)?;
        validate_block_authorization(
            &conditions,
            &AuthorizationWitness::FalconDirect(&spend.signatures),
            constants,
        )
        .map_err(Error::other)?;
        let mut pending = self
            .transactions
            .lock()
            .map_err(|_| Error::other("transaction context poisoned"))?;
        if pending.len() + spend.signatures.len() > 1024 {
            return Err(Error::other("pending Falcon signature limit"));
        }
        for (requirement, signature) in conditions.falcon_signatures.iter().zip(&spend.signatures) {
            pending.insert(
                (requirement.coin_id, requirement.condition_index),
                *signature,
            );
        }
        Ok(())
    }

    pub fn load_keys(
        &self,
        genesis: Bytes32,
        plot: FalconSigner,
        pool: FalconSigner,
    ) -> FalconPlotKeys {
        FalconPlotKeys {
            plot: Arc::new(plot),
            pool: Arc::new(pool),
            genesis,
            proofs: self.proofs.clone(),
        }
    }

    pub fn create_keys(&self, genesis: Bytes32) -> Result<FalconPlotKeys, Error> {
        Ok(FalconPlotKeys {
            plot: Arc::new(FalconSigner::generate().map_err(Error::other)?),
            pool: Arc::new(FalconSigner::generate().map_err(Error::other)?),
            genesis,
            proofs: self.proofs.clone(),
        })
    }

    pub fn primitives(
        &self,
        block: &FullBlock,
        constants: &ConsensusConstants,
    ) -> Result<FalconPrimitives, Error> {
        let references = [
            Some(block.reward_chain_block.challenge_chain_sp_signature),
            Some(block.reward_chain_block.reward_chain_sp_signature),
            Some(block.foliage.foliage_block_data_signature),
            block.foliage.foliage_transaction_block_signature,
            block.foliage.foliage_block_data.pool_signature,
        ];
        let mut proofs = self
            .proofs
            .lock()
            .map_err(|_| Error::other("signing context poisoned"))?;
        let mut selected = BTreeMap::new();
        for reference in references.into_iter().flatten() {
            let key = reference.const_bytes();
            selected.insert(
                key,
                proofs
                    .get(&key)
                    .ok_or_else(|| Error::other("missing Falcon signature"))?
                    .clone(),
            );
        }
        let mut pending = self
            .transactions
            .lock()
            .map_err(|_| Error::other("transaction signing context poisoned"))?;
        let mut transactions = Vec::new();
        let mut used = Vec::new();
        if let Some(program) = &block.transactions_generator {
            use dg_xch_core::consensus::block_generator::{
                BlockGeneratorFlags, BlockGeneratorInput, execute_block_generator_result,
            };
            let conditions = execute_block_generator_result(&BlockGeneratorInput {
                transactions_generator: program.clone(),
                generator_refs: vec![],
                constants: *constants,
                height: block.height(),
                flags: BlockGeneratorFlags::for_height(constants, block.height()),
            })
            .map_err(Error::other)?;
            for requirement in &conditions.falcon_signatures {
                used.push((requirement.coin_id, requirement.condition_index));
                transactions.push(
                    *pending
                        .get(&(requirement.coin_id, requirement.condition_index))
                        .ok_or_else(|| Error::other("missing transaction signature"))?,
                );
            }
        }
        let envelope = FalconBlock {
            genesis: constants.genesis_challenge,
            block: block.clone(),
            header: selected.into_values().collect(),
            transactions,
        };
        let result = FalconPrimitives::new(envelope, constants)?;
        proofs.clear();
        for key in used {
            pending.remove(&key);
        }
        Ok(result)
    }
}
