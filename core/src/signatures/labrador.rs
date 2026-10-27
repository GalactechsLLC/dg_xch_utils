//! Experimental PACKED11 profiles, pinned to LaBRADOR 3f95485139ffaa65fe572da809b90772901372e5.
//! Raw aggregation is prepared by block builders. The distinct ZK profile accepts
//! wallet proofs and bounded proof sets; header signatures remain direct Falcon.
//! Both profiles are confined to explicitly identified development chains.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LabradorProfile {
    #[serde(rename = "labrador-packed11-v2")]
    Packed11V2,
    #[serde(rename = "labrador-packed11-zk-v2")]
    Packed11ZkV2,
}

#[cfg(feature = "labrador")]
impl LabradorProfile {
    pub fn worker_profile(self) -> dg_xch_labrador::Profile {
        match self {
            Self::Packed11V2 => dg_xch_labrador::Profile::Raw,
            Self::Packed11ZkV2 => dg_xch_labrador::Profile::Zk,
        }
    }
}

#[cfg(feature = "labrador")]
mod evidence;
#[cfg(feature = "labrador")]
pub use evidence::{Evidence, MAX_ZK_PROOFS, evidence_stats, verify_evidence};

#[cfg(feature = "labrador")]
pub use backend::*;

#[cfg(feature = "labrador")]
mod backend {
    use super::LabradorProfile;
    use crate::blockchain::sized_bytes::Bytes32;
    use crate::signatures::{
        AuthorizationMode, AuthorizationRules, AuthorizationVerifier, AuthorizationWitness,
        SignatureRequirement,
        falcon::{FalconDirect, FalconSignature},
    };
    use dg_xch_labrador::Input;
    use sha2::{Digest, Sha256};
    use std::{
        collections::{HashSet, VecDeque},
        io::Error,
        sync::{LazyLock, Mutex},
    };

    pub const MAX_BATCH: usize = dg_xch_labrador::MAX_BATCH;
    pub const MIN_BATCH: usize = 256;

    fn statement(
        profile: LabradorProfile,
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
    ) -> Result<([u8; 32], Vec<Input<'_>>), Error> {
        if requirements.is_empty() || requirements.len() > MAX_BATCH {
            return Err(Error::other("LaBRADOR batch limit"));
        }
        let mut hash = Sha256::new();
        hash.update(b"dg_xch/authorization/labrador/ordered-execution/v2");
        hash.update(profile.worker_profile().id());
        hash.update(genesis.const_bytes());
        hash.update((requirements.len() as u32).to_be_bytes());
        let mut seen = HashSet::new();
        let mut inputs = Vec::with_capacity(requirements.len());
        for requirement in requirements {
            let SignatureRequirement::Falcon {
                occurrence,
                public_key,
                message,
            } = requirement
            else {
                return Err(Error::other("wrong signature profile"));
            };
            if !seen.insert((occurrence.coin_id, occurrence.condition_index))
                || message.len() > dg_xch_labrador::MAX_MESSAGE
            {
                return Err(Error::other("invalid signature occurrence"));
            }
            hash.update(occurrence.coin_id.const_bytes());
            hash.update(occurrence.condition_index.to_be_bytes());
            hash.update(public_key.as_ref());
            hash.update((message.len() as u32).to_be_bytes());
            hash.update(message);
            inputs.push(Input {
                public_key,
                message,
            });
        }
        Ok((hash.finalize().into(), inputs))
    }

    static VERIFIED: LazyLock<Mutex<VecDeque<[u8; 32]>>> =
        LazyLock::new(|| Mutex::new(VecDeque::new()));
    // A node runs at most one native verification/proving request at a time.
    static WORKER: Mutex<()> = Mutex::new(());

    pub fn verify(
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
        proof: &[u8],
    ) -> Result<(), Error> {
        verify_profile(LabradorProfile::Packed11V2, genesis, requirements, proof)
    }

    pub fn verify_profile(
        profile: LabradorProfile,
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
        proof: &[u8],
    ) -> Result<(), Error> {
        let (context, inputs) = statement(profile, genesis, requirements)?;
        dg_xch_labrador::unpack_profile(profile.worker_profile(), proof, inputs.len())?;
        let key: [u8; 32] = Sha256::digest([context.as_slice(), proof].concat()).into();
        let _worker = WORKER
            .lock()
            .map_err(|_| Error::other("LaBRADOR worker lock"))?;
        if VERIFIED
            .lock()
            .map_err(|_| Error::other("LaBRADOR cache lock"))?
            .contains(&key)
        {
            return Ok(());
        }
        dg_xch_labrador::verify_profile(profile.worker_profile(), &context, &inputs, proof)?;
        let mut cache = VERIFIED
            .lock()
            .map_err(|_| Error::other("LaBRADOR cache lock"))?;
        cache.push_back(key);
        if cache.len() > 64 {
            cache.pop_front();
        }
        Ok(())
    }

    pub fn prove(
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
        signatures: &[FalconSignature],
    ) -> Result<Vec<u8>, Error> {
        prove_profile(
            LabradorProfile::Packed11V2,
            genesis,
            requirements,
            signatures,
        )
    }

    pub fn prove_profile(
        profile: LabradorProfile,
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
        signatures: &[FalconSignature],
    ) -> Result<Vec<u8>, Error> {
        FalconDirect
            .verify(
                &AuthorizationRules::for_mode(AuthorizationMode::FalconDirect)
                    .map_err(Error::other)?,
                requirements,
                &AuthorizationWitness::FalconDirect(signatures),
            )
            .map_err(Error::other)?;
        let (context, inputs) = statement(profile, genesis, requirements)?;
        let proof = {
            let _worker = WORKER
                .lock()
                .map_err(|_| Error::other("LaBRADOR worker lock"))?;
            dg_xch_labrador::prove_profile(profile.worker_profile(), &context, &inputs, signatures)?
        };
        // Independently rebuild the verifier statement and decode the wire proof.
        verify_profile(profile, genesis, requirements, &proof)?;
        Ok(proof)
    }

    pub enum Aggregation {
        Pending,
        Direct,
        Proof(Vec<u8>),
    }
    #[derive(Default)]
    struct Prepared {
        active: bool,
        complete: VecDeque<([u8; 32], Option<Vec<u8>>)>,
    }
    static PREPARED: LazyLock<Mutex<Prepared>> = LazyLock::new(|| Mutex::new(Prepared::default()));

    /// Farming never waits for a native prover. Keep the transactions in the
    /// mempool while one bounded background job prepares their exact statement.
    /// A failed/oversized aggregate falls back to the already verified signatures.
    pub fn prepare(
        profile: LabradorProfile,
        genesis: Bytes32,
        requirements: &[SignatureRequirement],
        signatures: &[FalconSignature],
    ) -> Aggregation {
        if requirements.len() < MIN_BATCH || requirements.len() > MAX_BATCH {
            return Aggregation::Direct;
        }
        let Ok((key, _)) = statement(profile, genesis, requirements) else {
            return Aggregation::Direct;
        };
        let Ok(mut state) = PREPARED.lock() else {
            return Aggregation::Direct;
        };
        if let Some((_, proof)) = state.complete.iter().find(|(k, _)| k == &key) {
            return match proof {
                Some(proof) => Aggregation::Proof(proof.clone()),
                None => Aggregation::Direct,
            };
        }
        if state.active {
            return Aggregation::Pending;
        }
        state.active = true;
        let requirements = requirements.to_vec();
        let signatures = signatures.to_vec();
        let spawned=std::thread::Builder::new().name("labrador-prover".into()).spawn(move|| {
            let proof=match prove_profile(profile,genesis,&requirements,&signatures) {
                Ok(proof) if proof.len()<signatures.len()*666 => {
                    log::info!("LaBRADOR prepared {} authorizations: {} direct bytes -> {} proof bytes",signatures.len(),signatures.len()*666,proof.len());
                    Some(proof)
                },
                Ok(proof)=>{ log::info!("LaBRADOR kept direct signatures: {} proof bytes >= {} direct bytes",proof.len(),signatures.len()*666);None },
                Err(error)=>{ log::warn!("LaBRADOR producer fallback: {error}");None },
            };
            if let Ok(mut state)=PREPARED.lock() {
                state.active=false;
                state.complete.push_back((key,proof));
                if state.complete.len()>4 { state.complete.pop_front(); }
            }
        });
        if spawned.is_err() {
            state.active = false;
            return Aggregation::Direct;
        }
        Aggregation::Pending
    }
}
