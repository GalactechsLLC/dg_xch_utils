use super::*;
use dg_xch_core::consensus::producer::verify_plot_signature;

#[test]
fn a_plot_signature_verifies_under_the_plot_public_key() {
    let keys = PlotKeys::derive(42, 0).expect("derive");
    let msg = Bytes32::from([0x33; 32]);
    let sig = keys.sign(msg).expect("sign");
    assert!(
        verify_plot_signature(&keys.plot_public_key, msg, &sig),
        "the aggregate plot signature did not verify under the plot public key"
    );
    // A different message does not.
    assert!(!verify_plot_signature(
        &keys.plot_public_key,
        Bytes32::from([0x44; 32]),
        &sig
    ));
}
