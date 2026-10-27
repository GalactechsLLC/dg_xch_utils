#![cfg(feature = "labrador")]
use dg_xch_core::{
    blockchain::sized_bytes::Bytes32,
    consensus::chain_definition::ChainDefinition,
    signatures::{
        AuthorizationMode, AuthorizationRules, AuthorizationWitness, SignatureOccurrence,
        SignatureRequirement, SignatureSigner,
        extension::{AuthorizationExtension, FalconAuthorization},
        falcon::FalconSigner,
        labrador::{self, LabradorProfile},
        verify_authorization,
    },
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

fn inputs(count: usize) -> (Vec<SignatureRequirement>, Vec<[u8; 666]>) {
    let signers = [
        FalconSigner::generate().unwrap(),
        FalconSigner::generate().unwrap(),
    ];
    let mut requirements = Vec::new();
    let mut signatures = Vec::new();
    for i in 0..count {
        let signer = &signers[i % 2];
        let message = format!("actual Falcon signature {i}").into_bytes();
        signatures.push(signer.sign_message(&message).unwrap());
        requirements.push(SignatureRequirement::Falcon {
            occurrence: SignatureOccurrence {
                coin_id: Bytes32::const_new([3; 32]),
                condition_index: i as u32,
            },
            public_key: Box::new(signer.public_key()),
            message,
        });
    }
    (requirements, signatures)
}

#[test]
fn activation_is_bound_to_genesis_and_old_wire_is_unchanged() {
    let direct = ChainDefinition::falcon_development("labrador profile isolation".into());
    let mut aggregate = direct.clone();
    aggregate.falcon_aggregation = Some(LabradorProfile::Packed11V2);
    assert_ne!(
        direct.constants().unwrap().genesis_challenge,
        aggregate.constants().unwrap().genesis_challenge
    );
    let raw_genesis = aggregate.constants().unwrap().genesis_challenge;
    aggregate.falcon_aggregation = Some(LabradorProfile::Packed11ZkV2);
    assert_ne!(
        raw_genesis,
        aggregate.constants().unwrap().genesis_challenge
    );
    assert!(
        !serde_json::to_string(&direct)
            .unwrap()
            .contains("falcon_aggregation")
    );
    aggregate.signature_profile = Default::default();
    assert!(aggregate.constants().is_err());
    assert!(AuthorizationRules::for_mode(AuthorizationMode::FalconLabrador).is_err());
    let version = ChiaProtocolVersion::default();
    assert!(
        AuthorizationExtension::default()
            .to_bytes(version)
            .unwrap()
            .is_empty()
    );
    let mut auth = FalconAuthorization {
        genesis: Bytes32::default(),
        header: vec![],
        transactions: vec![],
        labrador: None,
    };
    assert!(
        AuthorizationExtension::from(auth.clone())
            .to_bytes(version)
            .unwrap()
            .starts_with(b"DGXAUT21")
    );
    auth.labrador = Some(vec![0; 54]);
    let extension = AuthorizationExtension::from(auth.clone());
    let bytes = extension.to_bytes(version).unwrap();
    assert!(bytes.starts_with(b"DGXAUT23"));
    assert_eq!(
        AuthorizationExtension::from_bytes(&mut Cursor::new(bytes.as_slice()), version).unwrap(),
        extension
    );
    for n in [8, 12, 44, bytes.len() - 1] {
        assert!(
            AuthorizationExtension::from_bytes(&mut Cursor::new(&bytes[..n]), version).is_err()
        );
    }
    auth.transactions.push([0; 666]);
    assert!(auth.transaction_witness().is_err());
    assert!(
        AuthorizationExtension::from(auth)
            .to_bytes(version)
            .is_err()
    );
}

#[test]
#[ignore = "requires dg_xch_labrador_worker; exercises real native proofs and rejection paths"]
fn real_proof_binds_the_entire_executed_statement() {
    let genesis = Bytes32::const_new([7; 32]);
    let (requirements, signatures) = inputs(2);
    let start = std::time::Instant::now();
    let proof = labrador::prove(genesis, &requirements, &signatures).unwrap();
    eprintln!(
        "two-signature proof: {} bytes, {:?}",
        proof.len(),
        start.elapsed()
    );
    let rules = AuthorizationRules::for_labrador(genesis).unwrap();
    verify_authorization(
        &rules,
        &requirements,
        &AuthorizationWitness::FalconLabrador(&proof),
    )
    .unwrap();
    assert!(
        verify_authorization(
            &AuthorizationRules::for_mode(AuthorizationMode::FalconDirect).unwrap(),
            &requirements,
            &AuthorizationWitness::FalconLabrador(&proof)
        )
        .is_err()
    );
    assert!(labrador::verify(Bytes32::default(), &requirements, &proof).is_err());
    let mut changed = requirements.clone();
    changed.reverse();
    assert!(labrador::verify(genesis, &changed, &proof).is_err());
    let mut changed = requirements.clone();
    let SignatureRequirement::Falcon { occurrence, .. } = &mut changed[0] else {
        unreachable!()
    };
    occurrence.condition_index += 17;
    assert!(labrador::verify(genesis, &changed, &proof).is_err());
    let mut changed = requirements.clone();
    let SignatureRequirement::Falcon { occurrence, .. } = &mut changed[0] else {
        unreachable!()
    };
    occurrence.coin_id = Bytes32::const_new([41; 32]);
    assert!(labrador::verify(genesis, &changed, &proof).is_err());
    let mut changed = requirements.clone();
    let SignatureRequirement::Falcon { public_key, .. } = &mut changed[0] else {
        unreachable!()
    };
    public_key[896] ^= 1;
    assert!(labrador::verify(genesis, &changed, &proof).is_err());
    let mut changed = requirements.clone();
    let SignatureRequirement::Falcon { message, .. } = &mut changed[0] else {
        unreachable!()
    };
    message.push(1);
    assert!(labrador::verify(genesis, &changed, &proof).is_err());
    assert!(labrador::verify(genesis, &requirements[..1], &proof).is_err());
    let mut duplicate = requirements.clone();
    duplicate[1] = duplicate[0].clone();
    assert!(labrador::verify(genesis, &duplicate, &proof).is_err());
    let mut damaged = proof.clone();
    damaged[42] ^= 1;
    assert!(labrador::verify(genesis, &requirements, &damaged).is_err());
    let last = damaged.len() - 1;
    damaged = proof.clone();
    damaged[last] ^= 1;
    assert!(labrador::verify(genesis, &requirements, &damaged).is_err());
    let mut padded = proof.clone();
    padded.push(0);
    assert!(labrador::verify(genesis, &requirements, &padded).is_err());
    let mut bad_signatures = signatures;
    bad_signatures[0][100] ^= 1;
    assert!(labrador::prove(genesis, &requirements, &bad_signatures).is_err());
}

#[test]
#[ignore = "resource-bounded 256-signature aggregate benchmark with real Falcon signatures"]
fn real_batch_is_smaller_than_direct_signatures() {
    let (requirements, signatures) = inputs(256);
    let genesis = Bytes32::const_new([9; 32]);
    let start = std::time::Instant::now();
    let proof = labrador::prove(genesis, &requirements, &signatures).unwrap();
    eprintln!(
        "256 signatures: direct={} aggregate={} elapsed={:?}",
        256 * 666,
        proof.len(),
        start.elapsed()
    );
    assert!(proof.len() < 256 * 666);
    labrador::verify(genesis, &requirements, &proof).unwrap();
}

#[test]
#[ignore = "requires native worker; proves hidden Falcon signature witnesses"]
fn real_wallet_zk_proof() {
    let (requirements, signatures) = inputs(2);
    let genesis = Bytes32::const_new([19; 32]);
    let start = std::time::Instant::now();
    let profile = LabradorProfile::Packed11ZkV2;
    let proof = labrador::prove_profile(profile, genesis, &requirements, &signatures).unwrap();
    eprintln!(
        "wallet ZK proof: {} bytes, {:?}",
        proof.len(),
        start.elapsed()
    );
    assert_eq!(proof[42 + 40 * requirements.len() + 5], 1);
    assert!(labrador::verify(genesis, &requirements, &proof).is_err());
    labrador::verify_evidence(profile, genesis, &requirements, &proof).unwrap();
    assert!(labrador::verify_profile(profile, Bytes32::default(), &requirements, &proof).is_err());
    let mut bad = proof.clone();
    bad[42] ^= 1;
    assert!(labrador::verify_profile(profile, genesis, &requirements, &bad).is_err());
    let mut downgraded = proof.clone();
    downgraded[8..40].copy_from_slice(dg_xch_labrador::PROFILE);
    downgraded[42 + 40 * requirements.len() + 5] = 0;
    assert!(labrador::verify(genesis, &requirements, &downgraded).is_err());
    let mut changed = requirements.clone();
    let SignatureRequirement::Falcon { message, .. } = &mut changed[0] else {
        unreachable!()
    };
    message.push(1);
    assert!(labrador::verify_profile(profile, genesis, &changed, &proof).is_err());
    let mut evidence = labrador::Evidence::default();
    evidence
        .add(&requirements, AuthorizationWitness::FalconLabrador(&proof))
        .unwrap();
    let mut reordered = requirements.clone();
    reordered.reverse();
    let auth = evidence.finish(genesis, &reordered).unwrap();
    labrador::verify_evidence(
        profile,
        genesis,
        &reordered,
        auth.labrador.as_ref().unwrap(),
    )
    .unwrap();
    let mut damaged_set = auth.labrador.unwrap();
    // Both slots now name the same occurrence: reject before native verification.
    damaged_set[15..17].copy_from_slice(&1u16.to_be_bytes());
    assert!(labrador::verify_evidence(profile, genesis, &reordered, &damaged_set).is_err());
}

#[test]
#[ignore = "real 256-signature wallet ZK resource and size measurement"]
fn real_wallet_zk_batch() {
    let (requirements, signatures) = inputs(256);
    let genesis = Bytes32::const_new([29; 32]);
    let start = std::time::Instant::now();
    let profile = LabradorProfile::Packed11ZkV2;
    let proof = labrador::prove_profile(profile, genesis, &requirements, &signatures).unwrap();
    eprintln!(
        "wallet ZK 256: direct={} proof={} elapsed={:?}",
        256 * 666,
        proof.len(),
        start.elapsed()
    );
    assert!(proof.len() < 256 * 666);
    assert_eq!(proof[42 + 40 * requirements.len() + 5], 1);
    labrador::verify_evidence(profile, genesis, &requirements, &proof).unwrap();
}

#[test]
fn wallet_prover_rejects_excessive_proof_count_before_native_work() {
    let count = labrador::MAX_BATCH * labrador::MAX_ZK_PROOFS + 1;
    let requirements: Vec<_> = (0..count)
        .map(|index| SignatureRequirement::Falcon {
            occurrence: SignatureOccurrence {
                coin_id: [1; 32].into(),
                condition_index: index as u32,
            },
            public_key: Box::new([0; 897]),
            message: vec![],
        })
        .collect();
    let signatures = vec![[0; 666]; count];
    let mut evidence = labrador::Evidence::default();
    evidence
        .add(
            &requirements,
            AuthorizationWitness::FalconDirect(&signatures),
        )
        .unwrap();
    let error = match evidence.prove_direct([2; 32].into()) {
        Ok(_) => panic!("excessive proof count was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "wallet ZK proof count limit exceeded");
}
