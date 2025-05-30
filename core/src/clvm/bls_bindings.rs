use crate::blockchain::sized_bytes::Bytes48;
use crate::constants::AUG_SCHEME_DST;
use blst::min_pk::{PublicKey, SecretKey, Signature};
use blst::BLST_ERROR;

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
    let mut new_msgs: Vec<Vec<u8>> = Vec::new();
    let mut keys: Vec<PublicKey> = Vec::new();
    for (key, msg) in public_keys.iter().zip(msgs) {
        let mut combined = Vec::new();
        combined.extend(*key);
        combined.extend(*msg);
        new_msgs.push(combined);
        keys.push((*key).into());
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

#[must_use]
pub fn sign(local_sk: &SecretKey, msg: &[u8]) -> Signature {
    local_sk.sign(msg, AUG_SCHEME_DST, &local_sk.sk_to_pk().to_bytes())
}

#[must_use]
pub fn sign_prepend(local_sk: &SecretKey, msg: &[u8], prepend_pk: &PublicKey) -> Signature {
    local_sk.sign(msg, AUG_SCHEME_DST, &prepend_pk.to_bytes())
}
