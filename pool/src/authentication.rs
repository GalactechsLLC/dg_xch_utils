use blst::min_pk::{PublicKey, Signature};
use chia_bls::DerivableKey;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::clvm::bls_bindings::verify_signature;
use dg_xch_core::protocols::pool::AuthenticationPayload;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Error;

pub fn validate_v1_token(token: u64, timeout_minutes: u8, now: u64) -> Result<(), Error> {
    if timeout_minutes == 0
        || token.abs_diff(now / 60 / u64::from(timeout_minutes)) > u64::from(timeout_minutes)
    {
        return Err(Error::other("invalid authentication token"));
    }
    Ok(())
}

pub fn verify(key: Bytes48, message: &[u8], signature: Bytes96) -> Result<(), Error> {
    let key = PublicKey::key_validate(key.as_ref())
        .map_err(|error| Error::other(format!("invalid public key: {error:?}")))?;
    let signature = Signature::sig_validate(signature.as_ref(), true)
        .map_err(|error| Error::other(format!("invalid signature: {error:?}")))?;
    if !verify_signature(&key, message, &signature) {
        return Err(Error::other("invalid signature"));
    }
    Ok(())
}

pub fn verify_v1_get(
    key: Bytes48,
    launcher_id: Bytes32,
    target_puzzle_hash: Bytes32,
    token: u64,
    signature: Bytes96,
) -> Result<(), Error> {
    let payload = AuthenticationPayload {
        method_name: "get_farmer".into(),
        launcher_id,
        target_puzzle_hash,
        authentication_token: token,
    };
    verify(
        key,
        &hash_256(payload.to_bytes(ChiaProtocolVersion::Chia0_0_37)?),
        signature,
    )
}

pub fn v2_auth_message(
    timestamp: u64,
    launcher_id: Bytes32,
    target_puzzle_hash: Bytes32,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(72);
    message.extend_from_slice(&timestamp.to_be_bytes());
    message.extend_from_slice(launcher_id.as_ref());
    message.extend_from_slice(target_puzzle_hash.as_ref());
    message
}

pub fn v2_auth_key(key: Bytes48) -> Result<Bytes48, Error> {
    let key = chia_bls::PublicKey::from_bytes(&key.bytes()).map_err(Error::other)?;
    Ok(key.derive_unhardened(12381).to_bytes().into())
}

pub fn verify_v2_login(
    key: Bytes48,
    launcher_id: Bytes32,
    target_puzzle_hash: Bytes32,
    timestamp: u64,
    signature: Bytes96,
    now: u64,
) -> Result<(), Error> {
    if timestamp.abs_diff(now) > 60 {
        return Err(Error::other(
            "authentication timestamp is stale or in the future",
        ));
    }
    verify(
        v2_auth_key(key)?,
        &v2_auth_message(timestamp, launcher_id, target_puzzle_hash),
        signature,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::clvm::bls_bindings::sign;

    #[test]
    fn v1_tokens_have_bounded_clock_skew() {
        assert!(validate_v1_token(10, 5, 3000).is_ok());
        assert!(validate_v1_token(9, 5, 3000).is_ok());
        assert!(validate_v1_token(4, 5, 3000).is_err());
        assert!(validate_v1_token(u64::MAX, 5, 3000).is_err());
        assert!(validate_v1_token(0, 0, 3000).is_err());
    }

    #[test]
    fn v2_auth_is_separated_from_singleton_signatures() {
        let secret = blst::min_pk::SecretKey::key_gen(&[7; 32], &[]).unwrap();
        let key = secret.sk_to_pk().to_bytes().into();
        let derived = dg_xch_keys::derive_path_unhardened(&secret, vec![12381]).unwrap();
        assert_eq!(
            v2_auth_key(key).unwrap(),
            Bytes48::from(derived.sk_to_pk().to_bytes())
        );
        let launcher = [8; 32].into();
        let target = [9; 32].into();
        let message = v2_auth_message(5000, launcher, target);
        assert_eq!(message.len(), 72);
        let signature = sign(&derived, &message).to_bytes().into();
        assert!(verify_v2_login(key, launcher, target, 5000, signature, 5000).is_ok());
        assert!(verify_v2_login(key, launcher, target, 5000, signature, 5061).is_err());
        assert!(verify_v2_login(key, launcher, [10; 32].into(), 5000, signature, 5000).is_err());
        assert!(
            verify_v2_login(
                key,
                launcher,
                target,
                5000,
                sign(&secret, &message).to_bytes().into(),
                5000
            )
            .is_err()
        );
    }
}
