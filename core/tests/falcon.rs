#![cfg(feature = "falcon")]
use dg_xch_core::signatures::falcon::{FalconSigner, FnDsa512V2};
use dg_xch_core::signatures::{SignatureScheme, SignatureSigner};

#[test]
fn randomized_header_signatures_cannot_change_consensus_references() {
    use dg_xch_core::{
        blockchain::sized_bytes::Bytes32,
        signatures::falcon_block::{FalconHeaderAuthorization, public_key_id},
    };
    let signer = FalconSigner::generate().unwrap();
    let genesis = Bytes32::const_new([11; 32]);
    let first = FalconHeaderAuthorization::sign(&signer, genesis, b"header").unwrap();
    let second = FalconHeaderAuthorization::sign(&signer, genesis, b"header").unwrap();
    assert_ne!(first.signature, second.signature);
    assert_eq!(first.reference(genesis), second.reference(genesis));
    let key = public_key_id(&signer.public_key());
    for proof in [first, second] {
        assert!(proof.verify(genesis, &key, b"header", &proof.reference(genesis)));
        assert!(!proof.verify(genesis, &key, b"other", &proof.reference(genesis)));
        assert!(!proof.verify(
            Bytes32::default(),
            &key,
            b"header",
            &proof.reference(genesis)
        ));
    }
}

#[test]
fn falcon_profile_is_bound_to_a_separate_development_chain() {
    use dg_xch_core::{consensus::chain_definition::ChainDefinition, signatures::SignatureProfile};
    let falcon = ChainDefinition::falcon_development("profile isolation".into());
    let mut bls = falcon.clone();
    bls.signature_profile = SignatureProfile::Bls12381Aug;
    assert_ne!(
        falcon.handshake_network_id().unwrap(),
        bls.handshake_network_id().unwrap()
    );
    assert_ne!(
        falcon.constants().unwrap().agg_sig_me_additional_data,
        bls.constants().unwrap().agg_sig_me_additional_data
    );
    let mut production = ChainDefinition::dgx();
    production.signature_profile = SignatureProfile::FnDsa512V2;
    assert!(production.constants().is_err());
    let mut chia = ChainDefinition::dg_pos2_test();
    chia.signature_profile = SignatureProfile::FnDsa512V2;
    assert!(chia.constants().is_err());
    let mut invalid = falcon;
    invalid.consensus.as_mut().unwrap().development = false;
    assert!(invalid.constants().is_err());
}

#[test]
fn padded_falcon_signatures_reject_tampering_and_key_mismatch() {
    let mut signer = FalconSigner::generate().unwrap();
    let key = signer.public_key();
    let signature = signer.sign(b"native Falcon test").unwrap();
    assert_eq!(key[0], 0x09);
    assert_eq!(signature[0], 0x39);
    assert!(FnDsa512V2.verify(&key, b"native Falcon test", &signature));
    assert!(!FnDsa512V2.verify(&key, b"changed message", &signature));
    let other = FalconSigner::generate().unwrap();
    assert!(!FnDsa512V2.verify(&other.public_key(), b"native Falcon test", &signature));
    let mut bad = signature;
    bad[40] ^= 1;
    assert!(!FnDsa512V2.verify(&key, b"native Falcon test", &bad));
    bad = signature;
    bad[0] = 0x29;
    assert!(!FnDsa512V2.verify(&key, b"native Falcon test", &bad));
    bad = signature;
    bad[665] = 1;
    assert!(!FnDsa512V2.verify(&key, b"native Falcon test", &bad));
    assert!(FalconSigner::from_secret_bytes(other.public_key(), signer.secret_bytes()).is_err());
    let restored = FalconSigner::from_secret_bytes(key, signer.secret_bytes()).unwrap();
    assert!(FnDsa512V2.verify(
        &key,
        b"restored",
        &restored.sign_message(b"restored").unwrap()
    ));
}

#[test]
fn falcon_execution_preserves_occurrences_and_rejects_wrong_flags_and_bls_witnesses() {
    use dg_xch_core::{
        blockchain::{coin::Coin, coin_spend::CoinSpend, spend_bundle::SpendBundle},
        clvm::{program::Program, sexp::SExp},
        consensus::{
            block_generator::{conditions_from_spend_bundle, validate_block_authorization},
            chain_definition::ChainDefinition,
        },
        signatures::{AuthorizationWitness, falcon_condition::transaction_reference},
    };
    let constants = ChainDefinition::falcon_development("Falcon execution test".into())
        .constants()
        .unwrap();
    let signer = FalconSigner::generate().unwrap();
    let public_key = signer.public_key();
    let sig_condition = |flag: Vec<u8>| -> SExp<'static> {
        vec![
            SExp::from(50u8),
            SExp::from(public_key.to_vec()),
            SExp::from(b"occurrence".to_vec()),
            SExp::from(flag),
        ]
        .into()
    };
    let puzzle = Program::to(1u8);
    let coin = Coin {
        parent_coin_info: [5; 32].into(),
        puzzle_hash: puzzle.tree_hash(),
        amount: 100,
    };
    let bundle_for = |conditions: Vec<SExp<'static>>| SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![CoinSpend {
            coin,
            puzzle_reveal: puzzle.serialized().unwrap(),
            solution: Program::to(conditions).serialized().unwrap(),
        }],
        ..SpendBundle::empty()
    };
    // The ignored negative assertion still occupies position zero in execution output.
    let skipped: SExp<'static> = vec![SExp::from(80u8), SExp::from(-1i64)].into();
    let bundle = bundle_for(vec![skipped, sig_condition(vec![]), sig_condition(vec![])]);
    let conditions = conditions_from_spend_bundle(&bundle, 10, &constants).unwrap();
    assert_eq!(
        conditions
            .falcon_signatures
            .iter()
            .map(|c| c.condition_index)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    let signatures: Vec<_> = conditions
        .falcon_signatures
        .iter()
        .map(|r| signer.sign_message(r.message.as_slice()).unwrap())
        .collect();
    validate_block_authorization(
        &conditions,
        &AuthorizationWitness::FalconDirect(&signatures),
        &constants,
    )
    .unwrap();
    assert!(
        validate_block_authorization(
            &conditions,
            &AuthorizationWitness::FalconDirect(&signatures[..1]),
            &constants
        )
        .is_err()
    );
    assert!(
        validate_block_authorization(
            &conditions,
            &AuthorizationWitness::BlsAggregate(&bundle.aggregated_signature),
            &constants
        )
        .is_err()
    );
    let reference =
        transaction_reference(constants.genesis_challenge, &conditions.falcon_signatures);
    assert_ne!(
        reference,
        transaction_reference(
            constants.genesis_challenge,
            &conditions.falcon_signatures[..1]
        )
    );
    for flag in [vec![0], vec![1], vec![2]] {
        assert!(
            conditions_from_spend_bundle(&bundle_for(vec![sig_condition(flag)]), 10, &constants)
                .is_err()
        );
    }
}

#[test]
fn falcon_wallet_signature_binds_destination_amount_coin_and_chain() {
    use dg_xch_core::{
        blockchain::{coin::Coin, sized_bytes::Bytes32},
        clvm::{program::Program, sexp::SExp},
        consensus::{
            block_generator::{conditions_from_spend_bundle, validate_block_authorization},
            chain_definition::ChainDefinition,
        },
        signatures::{AuthorizationWitness, falcon_wallet},
    };
    let constants = ChainDefinition::falcon_development("Falcon wallet test".into())
        .constants()
        .unwrap();
    let signer = FalconSigner::generate().unwrap();
    let puzzle = falcon_wallet::puzzle(&signer.public_key());
    let coin = Coin {
        parent_coin_info: [9; 32].into(),
        puzzle_hash: puzzle.tree_hash(),
        amount: 100,
    };
    let spend = falcon_wallet::spend(
        &signer,
        coin,
        Bytes32::const_new([7; 32]),
        99,
        &constants,
        10,
    )
    .unwrap();
    let conditions = conditions_from_spend_bundle(&spend.bundle, 10, &constants).unwrap();
    assert_eq!(conditions.falcon_signatures.len(), 1);
    assert_eq!(conditions.addition_amount, 99);
    validate_block_authorization(
        &conditions,
        &AuthorizationWitness::FalconDirect(&spend.signatures),
        &constants,
    )
    .unwrap();
    for (destination, amount) in [([8u8; 32], 99u64), ([7u8; 32], 98)] {
        let mut bad = spend.bundle.clone();
        bad.coin_spends[0].solution =
            Program::to(vec![SExp::from(destination.to_vec()), SExp::from(amount)])
                .serialized()
                .unwrap();
        let conditions = conditions_from_spend_bundle(&bad, 10, &constants).unwrap();
        assert!(
            validate_block_authorization(
                &conditions,
                &AuthorizationWitness::FalconDirect(&spend.signatures),
                &constants
            )
            .is_err()
        );
    }
    let other = ChainDefinition::falcon_development("different chain".into())
        .constants()
        .unwrap();
    let conditions = conditions_from_spend_bundle(&spend.bundle, 10, &other).unwrap();
    assert!(
        validate_block_authorization(
            &conditions,
            &AuthorizationWitness::FalconDirect(&spend.signatures),
            &other
        )
        .is_err()
    );
}

#[test]
fn native_authorization_wire_roundtrips_block_lists_and_rejects_bad_lengths() {
    use dg_xch_core::{
        blockchain::{full_block::FullBlock, sized_bytes::Bytes32, spend_bundle::SpendBundle},
        protocols::{farmer::SignedValues, full_node::RespondBlocks},
        signatures::{
            extension::{AuthorizationExtension, FalconAuthorization},
            falcon_block::FalconHeaderAuthorization,
        },
    };
    use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
    use std::io::Cursor;
    let version = ChiaProtocolVersion::default();
    let legacy: FullBlock = serde_json::from_str(include_str!(
        "../../full-node/tests/fixtures/full_block_5000000.json"
    ))
    .unwrap();
    let legacy_bytes = legacy.to_bytes(version).unwrap();
    assert!(legacy.authorization.is_none());
    assert!(
        !serde_json::to_value(&legacy)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("authorization")
    );
    assert_eq!(
        AuthorizationExtension::default().to_bytes(version).unwrap(),
        Vec::<u8>::new()
    );
    let signer = FalconSigner::generate().unwrap();
    let genesis = Bytes32::const_new([71; 32]);
    let proof = FalconHeaderAuthorization::sign(&signer, genesis, b"native wire").unwrap();
    let auth: AuthorizationExtension = FalconAuthorization {
        labrador: None,
        genesis,
        header: vec![proof.clone()],
        transactions: vec![],
    }
    .into();
    let mut extended = legacy.clone();
    extended.authorization = auth.clone();
    let extended_bytes = extended.to_bytes(version).unwrap();
    assert_eq!(&extended_bytes[..legacy_bytes.len()], &legacy_bytes);
    assert_eq!(
        legacy.header_hash().unwrap(),
        extended.header_hash().unwrap()
    );
    for blocks in [
        vec![legacy.clone(), extended.clone(), legacy.clone()],
        vec![extended.clone(), extended.clone()],
    ] {
        let response = RespondBlocks {
            start_height: 0,
            end_height: blocks.len() as u32 - 1,
            blocks,
        };
        let encoded = response.to_bytes(version).unwrap();
        let mut cursor = Cursor::new(encoded.as_slice());
        assert_eq!(
            RespondBlocks::from_bytes(&mut cursor, version).unwrap(),
            response
        );
        assert_eq!(cursor.position() as usize, encoded.len());
    }
    let signed = SignedValues {
        quality_string: genesis,
        foliage_block_data_signature: proof.reference(genesis),
        foliage_transaction_block_signature: proof.reference(genesis),
        authorization: auth.clone(),
    };
    let encoded = signed.to_bytes(version).unwrap();
    assert_eq!(
        SignedValues::from_bytes(&mut Cursor::new(encoded.as_slice()), version).unwrap(),
        signed
    );
    let mut bundle = SpendBundle::empty();
    bundle.authorization = auth.clone();
    assert!(SpendBundle::aggregate(vec![bundle]).is_err());
    let encoded = auth.to_bytes(version).unwrap();
    for length in 8..encoded.len() {
        assert!(
            AuthorizationExtension::from_bytes(&mut Cursor::new(&encoded[..length]), version)
                .is_err()
        );
    }
    let mut oversized = encoded.clone();
    oversized[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(
        AuthorizationExtension::from_bytes(&mut Cursor::new(oversized.as_slice()), version)
            .is_err()
    );
    let mut bad = auth.as_ref().unwrap().clone();
    bad.header[0].signature[80] ^= 1;
    assert!(bad.verify(genesis).is_err());
    assert!(auth.as_ref().unwrap().verify(Bytes32::default()).is_err());
}

#[test]
fn native_wire_size_limits_are_profile_scoped_and_still_bounded() {
    use dg_xch_core::{
        protocols::{ProtocolMessageTypes as P, rate_limits::RateLimiter},
        signatures::SignatureProfile,
    };
    for message in [P::SignedValues, P::RespondSignatures] {
        assert!(
            RateLimiter::new(true)
                .process_and_check(message, 4096, &Vec::new())
                .is_some()
        );
        let falcon = RateLimiter::new(true).with_signature_profile(SignatureProfile::FnDsa512V2);
        assert!(
            falcon
                .process_and_check(message, 4096, &Vec::new())
                .is_none()
        );
        assert!(
            falcon
                .process_and_check(message, 8193, &Vec::new())
                .is_some()
        );
    }
}

#[test]
fn imported_fn_dsa_keys_reject_legacy_length_and_tampered_public_key_hash() {
    let signer = FalconSigner::generate().unwrap();
    let public = signer.public_key();
    assert_eq!(signer.secret_bytes().len(), 1345);
    assert!(FalconSigner::from_secret_bytes(public, &signer.secret_bytes()[..1281]).is_err());
    let mut tampered = zeroize::Zeroizing::new(signer.secret_bytes().to_vec());
    tampered[1344] ^= 1;
    assert!(FalconSigner::from_secret_bytes(public, &tampered).is_err());
}

#[test]
fn fn_dsa_signatures_are_bound_to_the_versioned_application_context() {
    use fn_dsa::VerifyingKey;
    let signer = FalconSigner::generate().unwrap();
    let signature = signer.sign_message(b"context isolation").unwrap();
    let key = fn_dsa::VerifyingKey512::decode(&signer.public_key()).unwrap();
    for context in [
        fn_dsa::DOMAIN_NONE,
        fn_dsa::DomainContext(b"dg_xch/fn-dsa-512/v1"),
    ] {
        assert!(!key.verify(
            &signature,
            &context,
            &fn_dsa::HASH_ID_RAW,
            b"context isolation"
        ));
    }
    assert!(FnDsa512V2.verify(&signer.public_key(), b"context isolation", &signature));
}

#[test]
fn old_development_key_file_magic_is_rejected_without_modifying_the_file() {
    use dg_xch_core::signatures::falcon_keys::FalconKeyring;
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("dgx-legacy-key-{}", uuid::Uuid::new_v4()));
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(path.clone());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&path).unwrap().write_all(b"DGXKEY01").unwrap();
    let error = FalconKeyring::load(&path).unwrap_err();
    assert!(error.to_string().contains("regenerate development keys"));
    assert_eq!(std::fs::read(&path).unwrap(), b"DGXKEY01");
}
