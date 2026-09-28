use super::validate_block_aggregate_signature;
use crate::blockchain::sized_bytes::Bytes96;
use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::clvm::bls_bindings::sign;
use crate::consensus::constants::MAINNET;
use crate::traits::SizedBytes;
use blst::min_pk::SecretKey;

// AGG_SIG_UNSAFE pairs live on the bundle conditions, not on any spend. A bundle
// whose only signature is a real AGG_SIG_UNSAFE must verify.
#[test]
fn bundle_level_agg_sig_unsafe_pairs_join_the_aggregate() {
    let sk = SecretKey::key_gen_v3(&[7u8; 32], &[]).expect("sk");
    let msg = b"agg sig unsafe message".to_vec();
    let sig = sign(&sk, &msg);
    let mut conds = SpendBundleConditions::default();
    conds.agg_sig_unsafe.push((
        UnsizedBytes::new(sk.sk_to_pk().to_bytes().to_vec()),
        UnsizedBytes::new(msg),
    ));
    let aggregate = Bytes96::parse(&sig.to_bytes()).expect("sig bytes");
    validate_block_aggregate_signature(&conds, &aggregate, &MAINNET)
        .expect("an AGG_SIG_UNSAFE-only bundle verifies");
}
