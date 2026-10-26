use super::{
    AuthorizationError, AuthorizationMode, AuthorizationRules, AuthorizationVerifier,
    AuthorizationWitness, SignatureProfile, SignatureRequirement, SignatureScheme, SignatureSigner,
    SigningError,
};
use crate::blockchain::sized_bytes::Bytes48;
use crate::constants::AUG_SCHEME_DST;
use blst::BLST_ERROR;
use blst::min_pk::{PublicKey, SecretKey, Signature};
use std::collections::{HashMap, HashSet};

pub struct Bls12381Aug;

impl SignatureScheme for Bls12381Aug {
    type PublicKey = PublicKey;
    type Signature = Signature;

    const PROFILE: SignatureProfile = SignatureProfile::Bls12381Aug;

    fn verify(&self, public_key: &PublicKey, message: &[u8], signature: &Signature) -> bool {
        matches!(
            signature.verify(
                true,
                message,
                AUG_SCHEME_DST,
                &public_key.to_bytes(),
                public_key,
                true,
            ),
            BLST_ERROR::BLST_SUCCESS
        )
    }
}

/// A borrowed BLS signing key. The common signer interface does not expose it.
pub struct BlsSigner<'a> {
    secret_key: &'a SecretKey,
}

impl<'a> BlsSigner<'a> {
    #[must_use]
    pub const fn new(secret_key: &'a SecretKey) -> Self {
        Self { secret_key }
    }
}

impl SignatureSigner for BlsSigner<'_> {
    type Scheme = Bls12381Aug;

    fn public_key(&self) -> PublicKey {
        self.secret_key.sk_to_pk()
    }

    fn sign(&mut self, message: &[u8]) -> Result<Signature, SigningError> {
        Ok(sign(self.secret_key, message))
    }
}

#[must_use]
pub fn verify_signature(public_key: &PublicKey, msg: &[u8], signature: &Signature) -> bool {
    Bls12381Aug.verify(public_key, msg, signature)
}

pub fn aggregate_verify_signature(
    public_keys: &[Bytes48],
    msgs: &Vec<&[u8]>,
    signature: &Signature,
) -> bool {
    if public_keys.len() != msgs.len() {
        return false;
    }
    if public_keys.is_empty() {
        let mut identity = [0u8; 96];
        identity[0] = 0xc0;
        return signature.to_bytes() == identity;
    }
    let mut new_msgs: Vec<Vec<u8>> = Vec::new();
    let mut keys: Vec<PublicKey> = Vec::new();
    for (key, msg) in public_keys.iter().zip(msgs) {
        let mut combined = Vec::new();
        combined.extend(*key);
        combined.extend(*msg);
        new_msgs.push(combined);
        let Ok(public_key) = PublicKey::key_validate(key.as_ref()) else {
            return false;
        };
        keys.push(public_key);
    }
    matches!(
        signature.aggregate_verify(
            true,
            &new_msgs.iter().map(Vec::as_slice).collect::<Vec<&[u8]>>(),
            AUG_SCHEME_DST,
            &keys.iter().collect::<Vec<&PublicKey>>(),
            true,
        ),
        BLST_ERROR::BLST_SUCCESS
    )
}

/// Aggregate compressed G2 signatures for a block. Empty input produces the group identity.
///
/// # Errors
/// Returns `Err` with the malformed signature's index if any input fails to deserialize.
pub fn aggregate_signatures<
    'a,
    I: IntoIterator<Item = &'a crate::blockchain::sized_bytes::Bytes96>,
>(
    signatures: I,
) -> Result<crate::blockchain::sized_bytes::Bytes96, String> {
    use blst::min_pk::AggregateSignature;
    let mut parsed: Vec<Signature> = Vec::new();
    for (i, sig) in signatures.into_iter().enumerate() {
        parsed.push(
            Signature::from_bytes(sig.as_ref())
                .map_err(|e| format!("signature {i} failed to deserialize: {e:?}"))?,
        );
    }
    let Some((first, rest)) = parsed.split_first() else {
        let mut infinity = [0_u8; 96];
        infinity[0] = 0xc0;
        return Ok(crate::blockchain::sized_bytes::Bytes96::from(infinity));
    };
    let mut agg = AggregateSignature::from_signature(first);
    for sig in rest {
        agg.add_signature(sig, false)
            .map_err(|e| format!("aggregation failed: {e:?}"))?;
    }
    Ok(crate::blockchain::sized_bytes::Bytes96::from(
        agg.to_signature().to_bytes(),
    ))
}

#[must_use]
pub fn sign(local_sk: &SecretKey, msg: &[u8]) -> Signature {
    local_sk.sign(msg, AUG_SCHEME_DST, &local_sk.sk_to_pk().to_bytes())
}

#[must_use]
pub fn sign_prepend(local_sk: &SecretKey, msg: &[u8], prepend_pk: &PublicKey) -> Signature {
    local_sk.sign(msg, AUG_SCHEME_DST, &prepend_pk.to_bytes())
}

/// The legacy block aggregate verifier, including its repeated-key optimization.
pub struct BlsAggregate;

impl AuthorizationVerifier for BlsAggregate {
    fn verify(
        &self,
        rules: &AuthorizationRules,
        requirements: &[SignatureRequirement],
        witness: &AuthorizationWitness<'_>,
    ) -> Result<(), AuthorizationError> {
        rules.check_witness(AuthorizationMode::BlsAggregate, witness)?;
        let AuthorizationWitness::BlsAggregate(aggregate) = witness else {
            return Err(AuthorizationError::WrongWitness {
                expected: AuthorizationMode::BlsAggregate,
                actual: witness.mode(),
            });
        };
        let mut keys = Vec::with_capacity(requirements.len());
        let mut messages = Vec::with_capacity(requirements.len());
        for requirement in requirements {
            let SignatureRequirement::Bls {
                public_key,
                message,
            } = requirement
            else {
                return Err(AuthorizationError::WrongProfile {
                    expected: SignatureProfile::Bls12381Aug,
                    actual: requirement.profile(),
                });
            };
            keys.push(*public_key);
            messages.push(message.as_slice());
        }
        if keys.is_empty() {
            let mut infinity = [0_u8; 96];
            infinity[0] = 0xc0;
            return if aggregate.as_ref() == infinity {
                Ok(())
            } else {
                Err(AuthorizationError::InvalidSignature)
            };
        }
        let signature =
            Signature::try_from(*aggregate).map_err(|_| AuthorizationError::InvalidSignature)?;
        if aggregate_verify_deduped(&keys, &messages, &signature) {
            Ok(())
        } else {
            Err(AuthorizationError::InvalidSignature)
        }
    }
}

const DEDUP_MAX_DISTINCT_KEYS: usize = 64;

fn aggregate_verify_deduped(keys: &[Bytes48], messages: &[&[u8]], signature: &Signature) -> bool {
    // AUG scheme: each pair verifies pk || msg with an empty aug.
    let augmented = keys
        .iter()
        .zip(messages)
        .map(|(key, msg)| {
            let mut combined = Vec::with_capacity(48 + msg.len());
            combined.extend_from_slice(key.as_ref());
            combined.extend_from_slice(msg);
            combined
        })
        .collect::<Vec<Vec<u8>>>();
    let message_refs = augmented.iter().map(Vec::as_slice).collect::<Vec<&[u8]>>();

    let mut distinct = HashSet::<Bytes48>::new();
    for key in keys {
        distinct.insert(*key);
        if distinct.len() > DEDUP_MAX_DISTINCT_KEYS {
            break;
        }
    }
    if distinct.len() <= DEDUP_MAX_DISTINCT_KEYS && distinct.len() * 2 <= keys.len() {
        // Dedup branch: uncompress + validate each DISTINCT key once, pairing skips the
        // per-occurrence checks.
        let mut validated = HashMap::<Bytes48, PublicKey>::with_capacity(distinct.len());
        for key in &distinct {
            let Ok(pk) = PublicKey::from_bytes(key.as_ref()) else {
                return false;
            };
            if pk.validate().is_err() {
                return false;
            }
            validated.insert(*key, pk);
        }
        let key_refs = keys
            .iter()
            .map(|key| &validated[key])
            .collect::<Vec<&PublicKey>>();
        matches!(
            signature.aggregate_verify(true, &message_refs, AUG_SCHEME_DST, &key_refs, false),
            BLST_ERROR::BLST_SUCCESS
        )
    } else {
        // Per-occurrence conversion (`unwrap_or_default`, matching `From<Bytes48> for
        // PublicKey`) and per-occurrence validation inside blst's workers.
        let converted = keys
            .iter()
            .map(|key| PublicKey::from_bytes(key.as_ref()).unwrap_or_default())
            .collect::<Vec<PublicKey>>();
        let key_refs = converted.iter().collect::<Vec<&PublicKey>>();
        matches!(
            signature.aggregate_verify(true, &message_refs, AUG_SCHEME_DST, &key_refs, true),
            BLST_ERROR::BLST_SUCCESS
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_aggregate_requires_identity_and_matching_message_count() {
        let secret = SecretKey::key_gen(&[42; 32], &[]).unwrap();
        let key = Bytes48::from(secret.sk_to_pk().to_bytes());
        let message = b"pool reward test";
        let signature = sign(&secret, message);
        let mut identity = [0u8; 96];
        identity[0] = 0xc0;
        let identity = Signature::from_bytes(&identity).unwrap();
        assert!(aggregate_verify_signature(&[], &vec![], &identity));
        assert!(!aggregate_verify_signature(&[], &vec![], &signature));
        assert!(!aggregate_verify_signature(&[], &vec![message], &identity));
        assert!(!aggregate_verify_signature(&[key], &vec![], &signature));
        assert!(aggregate_verify_signature(
            &[key],
            &vec![message],
            &signature
        ));
        assert!(!aggregate_verify_signature(
            &[key],
            &vec![message, message],
            &signature
        ));
        assert!(!aggregate_verify_signature(
            &[[0; 48].into()],
            &vec![message],
            &signature
        ));
    }
}
