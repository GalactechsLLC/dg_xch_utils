use crate::types::blockchain::sized_bytes::{Bytes48, Bytes96, SizedBytes};
use blst::min_pk::{PublicKey, SecretKey, Signature};
use blst::BLST_ERROR;

//const BASIC_SCHEME_DST: &[u8; 43] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";
pub const AUG_SCHEME_DST: &[u8; 43] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_AUG_";
// const POP_SCHEME_DST: &[u8; 43] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
// const AUG_SCHEME_POP_DST: &[u8; 43] = b"BLS_POP_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

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
    public_keys: &[&Bytes48],
    msgs: &Vec<&[u8]>,
    signature: &Bytes96,
) -> bool {
    let sig: Signature = if let Ok(s) = signature.try_into() {
        s
    } else {
        return false;
    };
    let mut new_msgs: Vec<Vec<u8>> = Vec::new();
    let mut keys: Vec<PublicKey> = Vec::new();
    for (key, msg) in public_keys.iter().zip(msgs) {
        let mut combined = Vec::new();
        combined.extend(key.to_bytes().as_slice());
        combined.extend(*msg);
        new_msgs.push(combined);
        keys.push((*key).into());
    }
    matches!(
        sig.aggregate_verify(
            true,
            &new_msgs
                .iter()
                .map(|m| m.as_slice())
                .collect::<Vec<&[u8]>>(),
            AUG_SCHEME_DST,
            &keys.iter().collect::<Vec<&PublicKey>>(),
            true,
        ),
        BLST_ERROR::BLST_SUCCESS
    )
}

pub fn sign(local_sk: &SecretKey, msg: &[u8]) -> Signature {
    local_sk.sign(msg, AUG_SCHEME_DST, &local_sk.sk_to_pk().to_bytes())
}

pub fn sign_prepend(local_sk: &SecretKey, msg: &[u8], prepend_pk: &PublicKey) -> Signature {
    local_sk.sign(msg, AUG_SCHEME_DST, &prepend_pk.to_bytes())
}
