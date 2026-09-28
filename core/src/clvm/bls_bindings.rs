use crate::blockchain::sized_bytes::Bytes48;
use crate::constants::AUG_SCHEME_DST;
use blst::BLST_ERROR;
use blst::min_pk::{PublicKey, SecretKey, Signature};

#[must_use]
pub fn verify_signature(public_key: &PublicKey, msg: &[u8], signature: &Signature) -> bool {
    matches!(
        signature.verify(
            true,
            msg,
            AUG_SCHEME_DST,
            &public_key.to_bytes(),
            public_key,
            true
        ),
        BLST_ERROR::BLST_SUCCESS
    )
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
