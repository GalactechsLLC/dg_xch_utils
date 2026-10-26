#![cfg(feature = "falcon")]
use dg_xch_core::{
    blockchain::{coin_record::CoinRecord, sized_bytes::Bytes32, spend_bundle::SpendBundle},
    consensus::{chain_definition::ChainDefinition, constants::ConsensusConstants},
    signatures::falcon::FalconSigner,
};
use dg_xch_simulator_lib::coinset::CoinsetSimulator;
use dg_xch_wallet::{
    assets::{
        AssetAction, AssetCoin, AssetKind, NftLaunch, build_asset_transaction, discover_asset,
    },
    falcon::{FalconWallet, Payment, combine},
};
use std::{collections::HashSet, sync::Arc};

fn setup() -> (CoinsetSimulator, FalconWallet, FalconWallet) {
    let constants = ChainDefinition::falcon_development("Falcon wallet tests".into())
        .constants()
        .unwrap();
    let alice = FalconWallet::new(FalconSigner::generate().unwrap(), Arc::new(constants)).unwrap();
    let bob = FalconWallet::new(FalconSigner::generate().unwrap(), Arc::new(constants)).unwrap();
    let mut simulator = CoinsetSimulator::new();
    simulator.constants = constants;
    (simulator, alice, bob)
}

#[cfg(feature = "labrador")]
#[tokio::test]
#[ignore = "requires native worker; exercises wallet proofs through mempool and consensus"]
async fn wallet_zk_proofs_compose_with_direct_spends_and_survive_block_assembly() {
    use dg_xch_core::{
        consensus::block_generator::{
            BlockGeneratorFlags, BlockGeneratorInput, conditions_from_spend_bundle,
            execute_block_generator_result, validate_block_authorization,
        },
        signatures::labrador::{self, LabradorProfile},
    };
    use dg_xch_stores::{CoinStore, SqliteStore};
    use dg_xch_wallet::falcon::prove_zk;
    let mut chain = ChainDefinition::falcon_development("wallet ZK composition".into());
    chain.falcon_aggregation = Some(LabradorProfile::Packed11ZkV2);
    let constants = chain.constants().unwrap();
    let mut sim = CoinsetSimulator::new();
    sim.constants = constants;
    let alice = FalconWallet::new(FalconSigner::generate().unwrap(), Arc::new(constants)).unwrap();
    let bob = FalconWallet::new(FalconSigner::generate().unwrap(), Arc::new(constants)).unwrap();
    let coins = [
        fund(&mut sim, &alice, 600),
        fund(&mut sim, &alice, 700),
        fund(&mut sim, &bob, 800),
        fund(&mut sim, &bob, 900),
    ];
    let a = alice
        .payment(
            &coins[..2],
            &[Payment {
                puzzle_hash: bob.puzzle_hash(),
                amount: 1000,
            }],
            3,
            101,
        )
        .await
        .unwrap();
    let b = bob
        .payment(
            &coins[2..3],
            &[Payment {
                puzzle_hash: alice.puzzle_hash(),
                amount: 600,
            }],
            2,
            101,
        )
        .await
        .unwrap();
    let direct = bob
        .payment(
            &coins[3..],
            &[Payment {
                puzzle_hash: alice.puzzle_hash(),
                amount: 500,
            }],
            1,
            101,
        )
        .await
        .unwrap();
    let a = prove_zk(a, &constants, 101).await.unwrap();
    let b = prove_zk(b, &constants, 101).await.unwrap();
    assert!(a.authorization.as_ref().unwrap().transactions.is_empty());
    assert!(b.authorization.as_ref().unwrap().transactions.is_empty());
    let parts = [a.clone(), b, direct];
    let combined = combine(&parts, &constants, 101).unwrap();
    let proof = combined
        .authorization
        .as_ref()
        .unwrap()
        .labrador
        .as_ref()
        .unwrap();
    assert_eq!(labrador::evidence_stats(4, proof).unwrap(), (2, 3, 1));
    assert!(combine(&[a.clone(), a.clone()], &constants, 101).is_err());
    let mut missing = combined.clone();
    missing.coin_spends.remove(0);
    let conditions = conditions_from_spend_bundle(&missing, 101, &constants).unwrap();
    missing.aggregated_signature = dg_xch_core::signatures::falcon_condition::transaction_reference(
        constants.genesis_challenge,
        &conditions.falcon_signatures,
    );
    assert!(sim.new_transaction(missing).is_err());
    let mut mixed = a.clone();
    mixed
        .authorization
        .as_mut()
        .unwrap()
        .transactions
        .push([0x39; 666]);
    assert!(sim.new_transaction(mixed).is_err());
    let mut wrong_chain = constants;
    wrong_chain.genesis_challenge = [0x88; 32].into();
    assert!(combine(&parts, &wrong_chain, 101).is_err());
    let mut downgrade = constants;
    downgrade.falcon_aggregation = Some(LabradorProfile::Packed11V2);
    assert!(combine(&parts, &downgrade, 101).is_err());
    // Independent mempool entries are reordered by fee; block construction must
    // remap their statement coverage while retaining their original proofs.
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("zk.sqlite"))
        .await
        .unwrap();
    store.apply_block(0, 0, &coins, &[]).await.unwrap();
    let mut mp = dg_xch_node::mempool::Mempool::new(&constants);
    mp.set_peak(100, 1000);
    for bundle in &parts {
        let conds = conditions_from_spend_bundle(bundle, 101, &constants).unwrap();
        mp.admit(&store, bundle.clone(), conds).await.unwrap();
    }
    let block = mp
        .create_block_generator(&constants, 101, std::time::Duration::from_secs(10))
        .unwrap();
    let conditions = execute_block_generator_result(&BlockGeneratorInput {
        transactions_generator: block.program,
        generator_refs: vec![],
        constants,
        height: 101,
        flags: BlockGeneratorFlags::for_height(&constants, 101),
    })
    .unwrap();
    assert_eq!(conditions.falcon_signatures.len(), 4);
    let auth = block.authorization.as_ref().unwrap();
    assert_eq!(
        labrador::evidence_stats(4, auth.labrador.as_ref().unwrap()).unwrap(),
        (2, 3, 1)
    );
    validate_block_authorization(
        &conditions,
        &auth.transaction_witness().unwrap(),
        &constants,
    )
    .unwrap();
    sim.new_transaction(combined).unwrap();
    assert!(
        coins
            .iter()
            .all(|r| sim.coin_record(r.coin.name()).unwrap().spent)
    );
}

fn fund(sim: &mut CoinsetSimulator, wallet: &FalconWallet, amount: u64) -> CoinRecord {
    let coin = sim.new_coin(wallet.puzzle_hash(), amount);
    *sim.coin_record(coin.name()).unwrap()
}

fn assets(sim: &CoinsetSimulator, bundle: &SpendBundle, owner: Bytes32) -> Vec<AssetCoin> {
    let mut found = Vec::new();
    for spend in &bundle.coin_spends {
        for coin in spend.compute_additions_with_cost(500_000_000).unwrap().0 {
            let record = *sim.coin_record(coin.name()).unwrap();
            if !record.spent
                && let Some(asset) =
                    discover_asset(record, spend.clone(), &HashSet::from([owner])).unwrap()
            {
                found.push(asset);
            }
        }
    }
    found
}

#[tokio::test]
async fn payments_bind_outputs_fees_and_all_inputs_and_reject_replay() {
    let (mut sim, alice, bob) = setup();
    let inputs = [fund(&mut sim, &alice, 600), fund(&mut sim, &alice, 700)];
    let bundle = alice
        .payment(
            &inputs,
            &[Payment {
                puzzle_hash: bob.puzzle_hash(),
                amount: 1000,
            }],
            10,
            sim.height,
        )
        .await
        .unwrap();
    assert_eq!(bundle.authorization.as_ref().unwrap().transactions.len(), 2);
    for (amount, fee) in [(999, 10), (1000, 11)] {
        let mut tampered = alice
            .payment(
                &inputs,
                &[Payment {
                    puzzle_hash: bob.puzzle_hash(),
                    amount,
                }],
                fee,
                sim.height,
            )
            .await
            .unwrap();
        tampered.authorization.as_mut().unwrap().transactions =
            bundle.authorization.as_ref().unwrap().transactions.clone();
        assert!(
            sim.new_transaction(tampered)
                .unwrap_err()
                .to_string()
                .contains("BadAggregateSignature")
        );
    }
    let mut partial = bundle.clone();
    partial.coin_spends.remove(0);
    partial
        .authorization
        .as_mut()
        .unwrap()
        .transactions
        .remove(0);
    let partial_conditions = dg_xch_core::consensus::block_generator::conditions_from_spend_bundle(
        &partial,
        sim.height,
        &sim.constants,
    )
    .unwrap();
    partial.aggregated_signature = dg_xch_core::signatures::falcon_condition::transaction_reference(
        sim.constants.genesis_challenge,
        &partial_conditions.falcon_signatures,
    );
    // The remaining signature is valid and the spend pays its entire input as
    // fees; only the missing input's announcement must prevent this theft.
    dg_xch_core::consensus::block_generator::validate_block_authorization(
        &partial_conditions,
        &dg_xch_core::signatures::AuthorizationWitness::FalconDirect(
            &partial.authorization.as_ref().unwrap().transactions,
        ),
        &sim.constants,
    )
    .unwrap();
    assert!(sim.new_transaction(partial).is_err());
    let mut replay = sim.clone();
    replay.constants = ChainDefinition::falcon_development("other network".into())
        .constants()
        .unwrap();
    assert!(replay.new_transaction(bundle.clone()).is_err());
    let mut missing = bundle.clone();
    missing.authorization = Default::default();
    assert!(sim.new_transaction(missing).is_err());
    let outputs = sim.new_transaction(bundle.clone()).unwrap();
    assert_eq!(outputs.iter().map(|coin| coin.amount).sum::<u64>(), 1290);
    assert!(
        outputs
            .iter()
            .any(|coin| coin.puzzle_hash == alice.puzzle_hash() && coin.amount == 290)
    );
    assert!(sim.new_transaction(bundle).is_err());
    let coin = outputs
        .iter()
        .find(|coin| coin.puzzle_hash == bob.puzzle_hash())
        .unwrap();
    let returned = bob
        .payment(
            &[*sim.coin_record(coin.name()).unwrap()],
            &[Payment {
                puzzle_hash: alice.puzzle_hash(),
                amount: 300,
            }],
            5,
            sim.height,
        )
        .await
        .unwrap();
    let outputs = sim.new_transaction(returned).unwrap();
    assert!(
        outputs
            .iter()
            .any(|coin| coin.puzzle_hash == bob.puzzle_hash() && coin.amount == 695)
    );
    assert!(
        alice
            .payment(
                &[inputs[0], inputs[0]],
                &[Payment {
                    puzzle_hash: bob.puzzle_hash(),
                    amount: 10
                }],
                0,
                sim.height
            )
            .await
            .is_err()
    );
    assert!(
        alice
            .payment(
                &[inputs[0]],
                &[Payment {
                    puzzle_hash: bob.puzzle_hash(),
                    amount: u64::MAX
                }],
                1,
                sim.height
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn independent_wallet_bundles_combine_without_losing_occurrences() {
    let (mut sim, alice, bob) = setup();
    let a = fund(&mut sim, &alice, 100);
    let b = fund(&mut sim, &bob, 200);
    let a = alice
        .payment(
            &[a],
            &[Payment {
                puzzle_hash: bob.puzzle_hash(),
                amount: 99,
            }],
            1,
            sim.height,
        )
        .await
        .unwrap();
    let b = bob
        .payment(
            &[b],
            &[Payment {
                puzzle_hash: alice.puzzle_hash(),
                amount: 199,
            }],
            1,
            sim.height,
        )
        .await
        .unwrap();
    assert!(combine(&[a.clone(), a.clone()], &sim.constants, sim.height).is_err());
    let mut corrupt = b.clone();
    corrupt.authorization.as_mut().unwrap().transactions[0][100] ^= 1;
    assert!(combine(&[a.clone(), corrupt], &sim.constants, sim.height).is_err());
    let joined = combine(&[b, a], &sim.constants, sim.height).unwrap();
    assert_eq!(joined.authorization.as_ref().unwrap().transactions.len(), 2);
    sim.new_transaction(joined).unwrap();
}

#[tokio::test]
async fn cat_partial_transfers_and_nft_round_trip_use_falcon_and_preserve_assets() {
    let (mut sim, alice, bob) = setup();
    let funding = fund(&mut sim, &alice, 2000);
    let mint = build_asset_transaction(
        &alice,
        &[funding],
        &[],
        &HashSet::new(),
        &AssetAction::LaunchCat2 { amount: 1000 },
        10,
        alice.puzzle_hash(),
        sim.height,
    )
    .await
    .unwrap();
    sim.new_transaction(mint.clone()).unwrap();
    let cat = assets(&sim, &mint, alice.puzzle_hash()).pop().unwrap();
    assert_eq!(cat.kind, AssetKind::Cat2);
    let fee = fund(&mut sim, &alice, 100);
    let send = build_asset_transaction(
        &alice,
        &[fee],
        std::slice::from_ref(&cat),
        &HashSet::new(),
        &AssetAction::Transfer {
            coin_id: cat.record.coin.name(),
            destination: bob.puzzle_hash(),
            amount: 400,
        },
        3,
        alice.puzzle_hash(),
        sim.height,
    )
    .await
    .unwrap();
    assert_eq!(send.authorization.as_ref().unwrap().transactions.len(), 2);
    let mut bad_lineage = send.clone();
    bad_lineage.coin_spends[0].coin.parent_coin_info = [91; 32].into();
    assert!(sim.new_transaction(bad_lineage).is_err());
    sim.new_transaction(send.clone()).unwrap();
    let received = assets(&sim, &send, bob.puzzle_hash()).pop().unwrap();
    assert_eq!(received.asset_id, cat.asset_id);
    assert_eq!(
        assets(&sim, &send, alice.puzzle_hash())[0]
            .record
            .coin
            .amount,
        600
    );
    let back = build_asset_transaction(
        &bob,
        &[],
        std::slice::from_ref(&received),
        &HashSet::new(),
        &AssetAction::Transfer {
            coin_id: received.record.coin.name(),
            destination: alice.puzzle_hash(),
            amount: 150,
        },
        0,
        bob.puzzle_hash(),
        sim.height,
    )
    .await
    .unwrap();
    assert!(
        build_asset_transaction(
            &alice,
            &[],
            std::slice::from_ref(&received),
            &HashSet::new(),
            &AssetAction::Transfer {
                coin_id: received.record.coin.name(),
                destination: alice.puzzle_hash(),
                amount: 150
            },
            0,
            alice.puzzle_hash(),
            sim.height
        )
        .await
        .is_err()
    );
    sim.new_transaction(back.clone()).unwrap();
    assert_eq!(
        assets(&sim, &back, bob.puzzle_hash())[0].record.coin.amount,
        250
    );
    let funding = fund(&mut sim, &alice, 20);
    let mint = build_asset_transaction(
        &alice,
        &[funding],
        &[],
        &HashSet::new(),
        &AssetAction::LaunchNft(NftLaunch {
            data_uri: "https://example.com/falcon-nft".into(),
            data_hash: [3; 32].into(),
            royalty_basis_points: 250,
            royalty_puzzle_hash: alice.puzzle_hash(),
            edition_number: 1,
            edition_total: 1,
        }),
        1,
        alice.puzzle_hash(),
        sim.height,
    )
    .await
    .unwrap();
    sim.new_transaction(mint.clone()).unwrap();
    let nft = assets(&sim, &mint, alice.puzzle_hash()).pop().unwrap();
    let mut current = nft.clone();
    for (sender, recipient) in [(&alice, &bob), (&bob, &alice)] {
        let transfer = build_asset_transaction(
            sender,
            &[],
            &[current.clone()],
            &HashSet::new(),
            &AssetAction::Transfer {
                coin_id: current.record.coin.name(),
                destination: recipient.puzzle_hash(),
                amount: 1,
            },
            0,
            sender.puzzle_hash(),
            sim.height,
        )
        .await
        .unwrap();
        let mut bad = transfer.clone();
        bad.authorization.as_mut().unwrap().transactions[0][100] ^= 1;
        assert!(sim.new_transaction(bad).is_err());
        sim.new_transaction(transfer.clone()).unwrap();
        current = assets(&sim, &transfer, recipient.puzzle_hash())
            .pop()
            .unwrap();
        assert_eq!(current.asset_id, nft.asset_id);
        assert_eq!(current.metadata, nft.metadata);
        assert_eq!(current.royalty_basis_points, Some(250));
        assert_eq!(current.royalty_puzzle_hash, Some(alice.puzzle_hash()));
    }
}

#[test]
fn falcon_wallet_rejects_bls_chain_rules() {
    let constants: ConsensusConstants = ChainDefinition::development("BLS wallet isolation".into())
        .constants()
        .unwrap();
    assert!(FalconWallet::new(FalconSigner::generate().unwrap(), Arc::new(constants)).is_err());
}

#[tokio::test]
async fn mixed_256_signature_wallet_batch_fits_the_mempool_budget() {
    let (mut sim, alice, bob) = setup();
    let cat_funding = fund(&mut sim, &alice, 2000);
    let cat_mint = build_asset_transaction(
        &alice,
        &[cat_funding],
        &[],
        &HashSet::new(),
        &AssetAction::LaunchCat2 { amount: 1000 },
        1,
        alice.puzzle_hash(),
        sim.height,
    )
    .await
    .unwrap();
    sim.new_transaction(cat_mint.clone()).unwrap();
    let cat = assets(&sim, &cat_mint, alice.puzzle_hash()).pop().unwrap();
    let nft_funding = fund(&mut sim, &bob, 20);
    let nft_mint = build_asset_transaction(
        &bob,
        &[nft_funding],
        &[],
        &HashSet::new(),
        &AssetAction::LaunchNft(NftLaunch {
            data_uri: "https://example.com/falcon-batch".into(),
            data_hash: [42; 32].into(),
            royalty_basis_points: 250,
            royalty_puzzle_hash: bob.puzzle_hash(),
            edition_number: 1,
            edition_total: 1,
        }),
        1,
        bob.puzzle_hash(),
        sim.height,
    )
    .await
    .unwrap();
    sim.new_transaction(nft_mint.clone()).unwrap();
    let nft = assets(&sim, &nft_mint, bob.puzzle_hash()).pop().unwrap();
    let mut bundles = Vec::new();
    for i in 0..254 {
        let (sender, recipient) = if i % 2 == 0 {
            (&alice, &bob)
        } else {
            (&bob, &alice)
        };
        let coin = fund(&mut sim, sender, 1000 + i);
        bundles.push(
            sender
                .payment(
                    &[coin],
                    &[Payment {
                        puzzle_hash: recipient.puzzle_hash(),
                        amount: coin.coin.amount - 1,
                    }],
                    1,
                    sim.height,
                )
                .await
                .unwrap(),
        );
    }
    for (sender, recipient, asset) in [(&alice, &bob, cat), (&bob, &alice, nft)] {
        bundles.push(
            build_asset_transaction(
                sender,
                &[],
                std::slice::from_ref(&asset),
                &HashSet::new(),
                &AssetAction::Transfer {
                    coin_id: asset.record.coin.name(),
                    destination: recipient.puzzle_hash(),
                    amount: asset.record.coin.amount,
                },
                0,
                sender.puzzle_hash(),
                sim.height,
            )
            .await
            .unwrap(),
        );
    }
    let combined = combine(&bundles, &sim.constants, sim.height).unwrap();
    let conditions = dg_xch_core::consensus::block_generator::conditions_from_spend_bundle(
        &combined,
        sim.height,
        &sim.constants,
    )
    .unwrap();
    assert_eq!(conditions.falcon_signatures.len(), 256);
    let json_bytes = serde_json::to_vec(&serde_json::json!({"spend_bundle": &combined}))
        .unwrap()
        .len();
    println!(
        "mixed batch: cost={}, RPC JSON bytes={json_bytes}",
        conditions.cost
    );
    assert!(
        conditions.cost <= sim.constants.max_block_cost_clvm / 2,
        "batch cost {} exceeds the mempool's half-block policy",
        conditions.cost
    );
    sim.new_transaction(combined).unwrap();
}
