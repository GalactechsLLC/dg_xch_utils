use dg_xch_core::signatures::{
    AuthorizationError, AuthorizationMode, AuthorizationRules, AuthorizationWitness,
    verify_authorization,
};

#[test]
fn unsupported_or_context_free_modes_cannot_be_selected_by_witnesses() {
    use dg_xch_core::signatures::{AuthorizationVerifier, falcon::FalconLabrador};
    assert_eq!(
        FalconLabrador.verify(
            &AuthorizationRules::LEGACY_BLS,
            &[],
            &AuthorizationWitness::FalconLabrador(&[]),
        ),
        Err(AuthorizationError::UnsupportedMode(
            AuthorizationMode::FalconLabrador
        )),
    );
    #[cfg(not(feature = "falcon"))]
    assert_eq!(
        AuthorizationRules::for_mode(AuthorizationMode::FalconDirect),
        Err(AuthorizationError::UnsupportedMode(
            AuthorizationMode::FalconDirect
        )),
    );
    assert_eq!(
        AuthorizationRules::for_mode(AuthorizationMode::FalconLabrador),
        Err(AuthorizationError::UnsupportedMode(
            AuthorizationMode::FalconLabrador
        )),
    );
    for witness in [
        AuthorizationWitness::FalconDirect(&[]),
        AuthorizationWitness::FalconLabrador(&[1, 2, 3]),
    ] {
        // A panic here would expose an unfinished implementation to untrusted evidence.
        assert!(verify_authorization(&AuthorizationRules::LEGACY_BLS, &[], &witness).is_err());
    }
}

#[cfg(feature = "bls")]
mod bls {
    use super::*;
    use blst::min_pk::{AggregatePublicKey, AggregateSignature, SecretKey, Signature};
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::blockchain::coin_spend::CoinSpend;
    use dg_xch_core::blockchain::condition_with_args::{ConditionWithArgs, Message};
    use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
    use dg_xch_core::blockchain::spend_bundle::SpendBundle;
    use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::clvm::bls_bindings;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::consensus::block_generator::{
        bls_requirements_for_conditions, conditions_from_spend_bundle,
        validate_block_aggregate_signature, validate_block_authorization,
    };
    use dg_xch_core::consensus::constants::MAINNET;
    use dg_xch_core::constants::AUG_SCHEME_DST;
    use dg_xch_core::errors::ChiaError;
    use dg_xch_core::signatures::bls::{Bls12381Aug, BlsSigner};
    use dg_xch_core::signatures::{
        SignatureOccurrence, SignatureProfile, SignatureRequirement, SignatureScheme,
        SignatureSigner,
    };
    use dg_xch_core::traits::SizedBytes;
    use dg_xch_core::utils::hash_256;
    use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};

    fn key(seed: u8) -> SecretKey {
        SecretKey::key_gen_v3(&[seed; 32], &[]).unwrap()
    }

    fn reference_sign(key: &SecretKey, message: &[u8]) -> Signature {
        // Independent of the refactored provider and the compatibility exports.
        key.sign(message, AUG_SCHEME_DST, &key.sk_to_pk().to_bytes())
    }

    fn reference_aggregate(signatures: &[Signature]) -> Bytes96 {
        AggregateSignature::aggregate(&signatures.iter().collect::<Vec<_>>(), true)
            .unwrap()
            .to_signature()
            .to_bytes()
            .into()
    }

    fn verify(
        requirements: &[SignatureRequirement],
        signature: &Bytes96,
    ) -> Result<(), AuthorizationError> {
        verify_authorization(
            &AuthorizationRules::LEGACY_BLS,
            requirements,
            &AuthorizationWitness::BlsAggregate(signature),
        )
    }

    #[test]
    fn common_signer_and_legacy_api_match_native_augmented_signatures() {
        let secret = key(17);
        let other_key = key(18).sk_to_pk();
        let mut signer = BlsSigner::new(&secret);
        for message in [b"".as_slice(), b"wallet authorization", &[0x5a; 1024]] {
            let expected = reference_sign(&secret, message);
            let signature = signer.sign(message).unwrap();
            assert_eq!(signature.to_bytes(), expected.to_bytes());
            assert_eq!(
                bls_bindings::sign(&secret, message).to_bytes(),
                expected.to_bytes()
            );
            assert_eq!(signer.public_key().to_bytes(), secret.sk_to_pk().to_bytes());
            assert!(Bls12381Aug.verify(&signer.public_key(), message, &signature));
            assert!(bls_bindings::verify_signature(
                &signer.public_key(),
                message,
                &signature
            ));
            assert!(!Bls12381Aug.verify(&other_key, message, &signature));
            assert!(!Bls12381Aug.verify(&signer.public_key(), b"changed", &signature));
        }
    }

    #[test]
    fn plot_signature_shares_keep_the_combined_key_augmentation() {
        let local = key(21);
        let farmer = key(22);
        let local_pk = local.sk_to_pk();
        let farmer_pk = farmer.sk_to_pk();
        let plot_pk = AggregatePublicKey::aggregate(&[&local_pk, &farmer_pk], true)
            .unwrap()
            .to_public_key();
        let message = b"signage point";
        let local_sig = bls_bindings::sign_prepend(&local, message, &plot_pk);
        let farmer_sig = bls_bindings::sign_prepend(&farmer, message, &plot_pk);
        assert_eq!(
            local_sig.to_bytes(),
            local
                .sign(message, AUG_SCHEME_DST, &plot_pk.to_bytes())
                .to_bytes()
        );
        let combined = reference_aggregate(&[local_sig, farmer_sig]);
        let signature = Signature::from_bytes(combined.as_ref()).unwrap();
        assert!(Bls12381Aug.verify(&plot_pk, message, &signature));
        assert!(!Bls12381Aug.verify(&local_pk, message, &signature));
    }

    #[test]
    fn aggregate_matches_native_across_both_key_validation_paths() {
        // Both sides of the repeated-key threshold and its 64-distinct-key cap.
        for (distinct, count) in [(1u8, 1usize), (1, 4), (3, 4), (4, 8), (64, 128), (65, 130)] {
            let keys = (1..=distinct).map(key).collect::<Vec<_>>();
            let mut requirements = Vec::new();
            let mut signatures = Vec::new();
            for i in 0..count {
                let secret = &keys[i % keys.len()];
                let message = i.to_be_bytes().to_vec();
                signatures.push(reference_sign(secret, &message));
                requirements.push(SignatureRequirement::Bls {
                    public_key: secret.sk_to_pk().to_bytes().into(),
                    message,
                });
            }
            let expected = reference_aggregate(&signatures);
            let encoded = signatures
                .iter()
                .map(|s| Bytes96::from(s.to_bytes()))
                .collect::<Vec<_>>();
            assert_eq!(
                bls_bindings::aggregate_signatures(&encoded).unwrap(),
                expected
            );
            verify(&requirements, &expected).unwrap();
            // BLS aggregate ordering is immaterial; occurrence multiplicity is not.
            requirements.reverse();
            verify(&requirements, &expected).unwrap();
            requirements.push(requirements[0].clone());
            assert!(verify(&requirements, &expected).is_err());
            requirements.pop();
            if let SignatureRequirement::Bls { message, .. } = &mut requirements[0] {
                message.push(0xff);
            }
            assert!(verify(&requirements, &expected).is_err());
        }
    }

    #[test]
    fn equal_requirements_need_the_full_signature_multiplicity() {
        let secret = key(7);
        let message = b"repeated obligation";
        let requirement = SignatureRequirement::Bls {
            public_key: secret.sk_to_pk().to_bytes().into(),
            message: message.to_vec(),
        };
        let single = reference_sign(&secret, message);
        let twice = reference_aggregate(&[single, single]);
        verify(&[requirement.clone(), requirement.clone()], &twice).unwrap();
        assert!(verify(std::slice::from_ref(&requirement), &twice).is_err());
        assert!(
            verify(
                &[requirement.clone(), requirement],
                &single.to_bytes().into()
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_identity_and_wrong_profile_inputs_fail_closed() {
        let identity = SpendBundle::empty().aggregated_signature;
        verify(&[], &identity).unwrap();
        assert!(verify(&[], &Bytes96::default()).is_err());
        let real_sig = reference_sign(&key(2), b"message").to_bytes().into();
        assert!(verify(&[], &real_sig).is_err());
        let mut infinity_key = [0; 48];
        infinity_key[0] = 0xc0;
        for bad_key in [[0; 48], [0xff; 48], infinity_key] {
            for count in [1, 4] {
                let requirements = vec![
                    SignatureRequirement::Bls {
                        public_key: bad_key.into(),
                        message: b"message".to_vec(),
                    };
                    count
                ];
                assert!(verify(&requirements, &real_sig).is_err());
                assert!(verify(&requirements, &identity).is_err());
            }
        }
        let valid = SignatureRequirement::Bls {
            public_key: key(2).sk_to_pk().to_bytes().into(),
            message: b"message".to_vec(),
        };
        for signature in [Bytes96::default(), [0xff; 96].into(), identity] {
            assert!(verify(std::slice::from_ref(&valid), &signature).is_err());
        }
        let falcon = SignatureRequirement::Falcon {
            occurrence: SignatureOccurrence {
                coin_id: Bytes32::default(),
                condition_index: 0,
            },
            public_key: Box::new([0; 897]),
            message: vec![],
        };
        assert_eq!(
            verify(&[falcon], &identity),
            Err(AuthorizationError::WrongProfile {
                expected: SignatureProfile::Bls12381Aug,
                actual: SignatureProfile::FnDsa512V2,
            })
        );
    }

    #[test]
    fn legacy_bundle_bytes_and_identity_hash_do_not_gain_profile_tags() {
        let bundle = SpendBundle::empty();
        let mut expected = vec![0, 0, 0, 0]; // Existing u32 coin-spend count.
        expected.extend_from_slice(bundle.aggregated_signature.as_ref());
        assert_eq!(
            bundle.to_bytes(ChiaProtocolVersion::default()).unwrap(),
            expected
        );
        assert_eq!(bundle.name().unwrap(), Bytes32::from(hash_256(&expected)));
        assert_eq!(
            bls_bindings::aggregate_signatures(std::iter::empty::<&Bytes96>()).unwrap(),
            bundle.aggregated_signature
        );
    }

    #[test]
    fn execution_adapter_preserves_all_legacy_scopes_and_network_domains() {
        let secret = key(29);
        let public_key = Bytes48::from(secret.sk_to_pk().to_bytes());
        let raw_message = b"scope";
        let message = Message::new(raw_message.to_vec()).unwrap();
        let conditions = [
            ConditionWithArgs::AggSigParent(public_key, message),
            ConditionWithArgs::AggSigPuzzle(public_key, message),
            ConditionWithArgs::AggSigAmount(public_key, message),
            ConditionWithArgs::AggSigPuzzleAmount(public_key, message),
            ConditionWithArgs::AggSigParentAmount(public_key, message),
            ConditionWithArgs::AggSigParentPuzzle(public_key, message),
            ConditionWithArgs::AggSigUnsafe(public_key, message),
            ConditionWithArgs::AggSigMe(public_key, message),
        ];
        let puzzle = Program::to(1_u8);
        let solution = Program::to(conditions.iter().map(SExp::from).collect::<Vec<_>>());
        // Pin the existing signed-message amount encoding, including its high-bit case.
        for (amount, amount_bytes) in [
            (0, vec![]),
            (127, vec![127]),
            (128, vec![128]),
            (u64::MAX, vec![0, 255, 255, 255, 255, 255, 255, 255, 255]),
        ] {
            let coin = Coin {
                parent_coin_info: [0x41; 32].into(),
                puzzle_hash: puzzle.tree_hash(),
                amount,
            };
            let bundle = SpendBundle {
                authorization: Default::default(),
                coin_spends: vec![CoinSpend {
                    coin,
                    puzzle_reveal: puzzle.serialized().unwrap(),
                    solution: solution.serialized().unwrap(),
                }],
                ..SpendBundle::empty()
            };
            let conds =
                conditions_from_spend_bundle(&bundle, MAINNET.hard_fork_height, &MAINNET).unwrap();
            let requirements = bls_requirements_for_conditions(&conds, &MAINNET).unwrap();
            assert_eq!(requirements.len(), 8);
            let mut signatures = Vec::new();
            for opcode in 43u8..=50 {
                let mut expected = raw_message.to_vec();
                if [43, 47, 48].contains(&opcode) {
                    expected.extend_from_slice(coin.parent_coin_info.as_ref());
                }
                if [44, 46, 48].contains(&opcode) {
                    expected.extend_from_slice(coin.puzzle_hash.as_ref());
                }
                if [45, 46, 47].contains(&opcode) {
                    expected.extend_from_slice(&amount_bytes);
                }
                if opcode == 50 {
                    expected.extend_from_slice(coin.name().as_ref());
                }
                if opcode == 50 {
                    expected.extend_from_slice(MAINNET.agg_sig_me_additional_data.as_ref());
                } else if opcode != 49 {
                    let mut domain = MAINNET.agg_sig_me_additional_data.bytes().to_vec();
                    domain.push(opcode);
                    expected.extend_from_slice(&hash_256(domain));
                }
                assert!(requirements.contains(&SignatureRequirement::Bls {
                    public_key,
                    message: expected.clone()
                }));
                signatures.push(reference_sign(&secret, &expected));
            }
            let signature = reference_aggregate(&signatures);
            validate_block_aggregate_signature(&conds, &signature, &MAINNET).unwrap();
            validate_block_authorization(
                &conds,
                &AuthorizationWitness::BlsAggregate(&signature),
                &MAINNET,
            )
            .unwrap();
            let mut other_network = MAINNET;
            other_network.agg_sig_me_additional_data = [0x99; 32].into();
            assert_eq!(
                validate_block_authorization(
                    &conds,
                    &AuthorizationWitness::BlsAggregate(&signature),
                    &other_network
                ),
                Err(ChiaError::BadAggregateSignature)
            );
        }
    }

    #[test]
    fn malformed_legacy_conditions_retain_their_error_class() {
        let mut conditions = SpendBundleConditions::default();
        conditions
            .agg_sig_unsafe
            .push((UnsizedBytes::new(vec![1; 47]), UnsizedBytes::new(vec![])));
        let signature = SpendBundle::empty().aggregated_signature;
        assert_eq!(
            validate_block_aggregate_signature(&conditions, &signature, &MAINNET),
            Err(ChiaError::BadAggregateSignature)
        );
        conditions.agg_sig_unsafe[0].0 = UnsizedBytes::new(vec![1; 49]);
        assert_eq!(
            validate_block_aggregate_signature(&conditions, &signature, &MAINNET),
            Err(ChiaError::InvalidCondition)
        );
        conditions.agg_sig_unsafe[0] = (
            UnsizedBytes::new(key(3).sk_to_pk().to_bytes().to_vec()),
            UnsizedBytes::new(vec![1; 1025]),
        );
        assert_eq!(
            validate_block_aggregate_signature(&conditions, &signature, &MAINNET),
            Err(ChiaError::InvalidCondition)
        );
    }
}
