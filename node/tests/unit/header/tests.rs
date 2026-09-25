use super::{QueuedSig, first_failing_sig, verify_sig_batch};
use dg_xch_core::blockchain::sized_bytes::{Bytes48, Bytes96};
use dg_xch_core::consensus::block_header_validation::HeaderSigTag;

// Lock the tag -> rejection-string mapping for all five gates. A rename here is a
// consensus-visible error-string change and must be a deliberate edit.
#[test]
fn tag_rejection_strings_are_exact() {
    assert_eq!(
        HeaderSigTag::RewardChainSp.rejection(),
        "INVALID_RC_SIGNATURE"
    );
    assert_eq!(
        HeaderSigTag::ChallengeChainSp.rejection(),
        "INVALID_CC_SIGNATURE"
    );
    assert_eq!(
        HeaderSigTag::FoliageBlockData.rejection(),
        "INVALID_PLOT_SIGNATURE (block data)"
    );
    assert_eq!(
        HeaderSigTag::FoliageTransactionBlock.rejection(),
        "INVALID_PLOT_SIGNATURE (ftb)"
    );
    assert_eq!(HeaderSigTag::Pool.rejection(), "INVALID_POOL_SIGNATURE");
}

fn garbage(tag: HeaderSigTag) -> QueuedSig {
    // A zero public key / zero signature is not a valid G1/G2 point, so `bls_verify` fails
    // closed (no panic) — exactly the malformed-input path, and enough to drive the batch's
    // failure/ordering logic without real crypto.
    QueuedSig {
        pk: Bytes48::from([0u8; 48]),
        msg: vec![1, 2, 3],
        sig: Bytes96::from([0u8; 96]),
        tag,
    }
}

#[test]
fn empty_batch_is_vacuously_ok() {
    assert!(verify_sig_batch(&[]));
    assert_eq!(first_failing_sig(&[]), None);
}

#[test]
fn first_failing_sig_returns_the_first_bad_in_push_order() {
    // Two failing sigs; `first_failing_sig` must return the first in slice order so the
    // reported rejection matches the inline first-failure.
    let q = [
        garbage(HeaderSigTag::RewardChainSp),
        garbage(HeaderSigTag::Pool),
    ];
    assert!(!verify_sig_batch(&q));
    assert_eq!(first_failing_sig(&q), Some(HeaderSigTag::RewardChainSp));
}
