use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::block_header_validation::{
    HeaderValidationVerifier, validate_pospace_and_get_required_iters,
};
use dg_xch_core::consensus::constants::{ConsensusConstants, MAINNET};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingVerifier(AtomicUsize);

impl HeaderValidationVerifier for CountingVerifier {
    fn validate_vdf(
        &self,
        _: &ConsensusConstants,
        _: &ClassgroupElement,
        _: &VdfInfo,
        _: &VdfProof,
        _: Option<&VdfInfo>,
    ) -> bool {
        panic!("activation checks must not verify VDFs")
    }

    fn pospace_quality_string(
        &self,
        _: &ConsensusConstants,
        _: &ProofOfSpace,
        _: Bytes32,
        _: Bytes32,
        _: u32,
    ) -> Option<Bytes32> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Some(Bytes32::default())
    }
}

#[test]
fn proof_activation_rejects_before_signage_transaction_height() {
    let constants = ConsensusConstants {
        hard_fork2_height: 100,
        ..MAINNET
    };
    let proof = ProofOfSpace::v2(
        Bytes32::default(),
        Some(Bytes48::default()),
        None,
        Bytes48::default(),
        0,
        0,
        2,
        Vec::new().into(),
    );
    let verifier = CountingVerifier(AtomicUsize::new(0));
    for previous_transaction_height in [0, 99] {
        assert!(
            validate_pospace_and_get_required_iters(
                &verifier,
                &constants,
                &proof,
                Bytes32::default(),
                Bytes32::default(),
                150,
                1,
                previous_transaction_height,
            )
            .unwrap()
            .is_none()
        );
    }
    assert_eq!(verifier.0.load(Ordering::Relaxed), 0);
    assert!(
        validate_pospace_and_get_required_iters(
            &verifier,
            &constants,
            &proof,
            Bytes32::default(),
            Bytes32::default(),
            150,
            1,
            100,
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(verifier.0.load(Ordering::Relaxed), 1);
    let genesis = ConsensusConstants {
        hard_fork2_height: 0,
        ..constants
    };
    assert!(
        validate_pospace_and_get_required_iters(
            &verifier,
            &genesis,
            &proof,
            Bytes32::default(),
            Bytes32::default(),
            0,
            1,
            0,
        )
        .unwrap()
        .is_some()
    );
}
