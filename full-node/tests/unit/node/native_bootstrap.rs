use super::super::*;
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::proof_of_space::{ProofOfSpace, calculate_pos_challenge};
use dg_xch_core::blockchain::sized_bytes::{Bytes48, Bytes100};
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::consensus::block_rewards::RewardSchedule;
use dg_xch_core::consensus::constants::SIMULATOR;
use dg_xch_core::consensus::pot_iterations::{calculate_ip_iters, calculate_sp_iters};
use dg_xch_core::consensus::vdf_info_computation::get_signage_point_vdf_info;
use dg_xch_pos::pos2::ProofParams;
use dg_xch_pos::pos2::chainer::SearchLimits;
use dg_xch_pos::pos2::plotting::{NativePlot, PlotLimits};

fn vdf(challenge: Bytes32, input: ClassgroupElement, iterations: u64) -> (VdfInfo, VdfProof) {
    let result =
        dg_xch_vdf::proof::prove_result(challenge.as_ref(), input.data.as_ref(), 16, iterations)
            .expect("real CPU VDF");
    let (output, witness) = result.split_at(100);
    (
        VdfInfo {
            challenge,
            number_of_iterations: iterations,
            output: ClassgroupElement {
                data: Bytes100::parse(output).unwrap(),
            },
        },
        VdfProof {
            witness_type: 0,
            witness: UnsizedBytes::new(witness.to_vec()),
            normalized_to_identity: false,
        },
    )
}

fn farm(
    plot: &NativePlot,
    template: &ProofOfSpace,
    challenge: Bytes32,
    signage: Bytes32,
) -> Option<ProofOfSpace> {
    let challenge = calculate_pos_challenge(template.get_plot_id().unwrap(), challenge, signage);
    let cancelled = AtomicBool::new(false);
    let chains = plot
        .qualities(
            challenge,
            SearchLimits {
                max_hashes: 10_000_000,
                max_results: 1024,
            },
            &cancelled,
        )
        .expect("bounded native quality search");
    chains.first().map(|chain| {
        let mut proof = template.clone();
        proof.challenge = challenge;
        proof.proof = plot.prove(chain, challenge).expect("native proof").into();
        proof
    })
}

async fn declare(
    node: &Arc<FullNode>,
    plot_key: &SecretKey,
    pool_key: &SecretKey,
    proof: ProofOfSpace,
    index: u8,
    cc_sp: Bytes32,
    rc_sp: Bytes32,
) -> UnfinishedBlock {
    let target = PoolTarget {
        puzzle_hash: Bytes32::new([0x33; 32]),
        max_height: 0,
    };
    let declaration = DeclareProofOfSpace {
        challenge_hash: node.constants.genesis_challenge,
        challenge_chain_sp: cc_sp,
        signage_point_index: index,
        reward_chain_sp: rc_sp,
        proof_of_space: proof,
        challenge_chain_sp_signature: sign(plot_key, cc_sp.as_ref()).into(),
        reward_chain_sp_signature: sign(plot_key, rc_sp.as_ref()).into(),
        farmer_puzzle_hash: Bytes32::new([0x44; 32]),
        pool_target: Some(target),
        pool_signature: Some(
            sign(
                pool_key,
                &target.to_bytes(ChiaProtocolVersion::default()).unwrap(),
            )
            .into(),
        ),
        include_signature_source_data: true,
    };
    let api = node.peer_api();
    let request = FullNodeApi::on_declare_proof_of_space(&api, Bytes32::default(), declaration)
        .await
        .expect("native declaration assembled into candidate");
    FullNodeApi::on_signed_values(
        &api,
        Bytes32::default(),
        SignedValues {
            quality_string: request.quality_string,
            foliage_block_data_signature: sign(plot_key, request.foliage_block_data_hash.as_ref())
                .into(),
            foliage_transaction_block_signature: sign(
                plot_key,
                request.foliage_transaction_block_hash.as_ref(),
            )
            .into(),
        },
    )
    .await;
    let unfinished = node
        .ub_inbox
        .lock()
        .await
        .last()
        .cloned()
        .expect("signed candidate entered UB inbox");
    process_ub_inbox(node).await;
    let partial = unfinished.reward_chain_block.hash().unwrap();
    assert!(
        node.unfinished.lock().await.get_block(&partial).is_some(),
        "real header and body validation admitted UB"
    );
    assert!(
        node.ub_timelord_announce
            .lock()
            .await
            .iter()
            .any(|work| work.reward_chain_block == unfinished.reward_chain_block)
    );
    unfinished
}

async fn infuse(
    node: &Arc<FullNode>,
    unfinished: &UnfinishedBlock,
    previous: Option<&BlockRecord>,
    registry: &Arc<dyn OutboundPeers>,
) -> FullBlock {
    let proof = &unfinished.reward_chain_block.proof_of_space;
    let signage = unfinished
        .reward_chain_block
        .challenge_chain_sp_vdf
        .as_ref()
        .map_or(node.constants.genesis_challenge, |value| {
            value.output.hash().unwrap()
        });
    let quality = dg_xch_pos::verify_and_get_quality_string_with_context(
        proof,
        &node.constants,
        node.constants.genesis_challenge,
        signage,
        previous.map_or(0, |record| record.height + 1),
        0,
    )
    .expect("native PoS2 verifier");
    let required = dg_xch_core::consensus::pot_iterations::calculate_iterations_quality_for_proof(
        &node.constants,
        proof,
        quality,
        node.constants.difficulty_starting,
        signage,
    );
    let ip_iters = calculate_ip_iters(
        &node.constants,
        node.constants.sub_slot_iters_starting,
        unfinished.reward_chain_block.signage_point_index,
        required,
    )
    .unwrap();
    let delta =
        u64::try_from(u128::from(ip_iters) - previous.map_or(0, |record| record.total_iters))
            .unwrap();
    let identity = ClassgroupElement::get_default_element();
    let (mut cc_info, cc_proof) = vdf(
        node.constants.genesis_challenge,
        previous.map_or(identity, |record| record.challenge_vdf_output),
        delta,
    );
    cc_info.number_of_iterations = ip_iters;
    let (rc_info, rc_proof) = vdf(
        previous.map_or(node.constants.genesis_challenge, |record| {
            record.reward_infusion_new_challenge
        }),
        identity,
        delta,
    );
    let (icc_info, icc_proof) = match previous {
        None => (None, None),
        Some(record) => {
            let (info, proof) = vdf(record.challenge_block_info_hash, identity, delta);
            (Some(info), Some(proof))
        }
    };
    let message = NewInfusionPointVDF {
        unfinished_reward_hash: unfinished.reward_chain_block.hash().unwrap(),
        challenge_chain_ip_vdf: cc_info,
        challenge_chain_ip_proof: cc_proof,
        reward_chain_ip_vdf: rc_info,
        reward_chain_ip_proof: rc_proof,
        infused_challenge_chain_ip_vdf: icc_info,
        infused_challenge_chain_ip_proof: icc_proof,
    };
    let expected = assemble_infusion_block(node, &message)
        .await
        .expect("validated unfinished block assembled");
    let old_peak = node.store.get_peak().await.unwrap();
    if previous.is_none() {
        let mut orphan = expected.clone();
        orphan.reward_chain_block.height = 1;
        orphan.foliage.prev_block_hash = Bytes32::new([0xFE; 32]);
        assert!(node.follow_step_blocks(&[orphan]).await.is_err());
        assert_eq!(node.store.get_peak().await.unwrap(), old_peak);
    }
    let mut invalid = message.clone();
    invalid.challenge_chain_ip_proof.witness = UnsizedBytes::new(vec![0]);
    let api = node.peer_api();
    FullNodeApi::on_new_infusion_point_vdf(&api, Bytes32::default(), invalid).await;
    process_ip_inbox(node, registry, &node.inbound_peers).await;
    assert_eq!(
        node.store.get_peak().await.unwrap(),
        old_peak,
        "invalid VDF never advances the chain"
    );
    FullNodeApi::on_new_infusion_point_vdf(&api, Bytes32::default(), message).await;
    process_ip_inbox(node, registry, &node.inbound_peers).await;
    assert_eq!(
        node.store.get_peak().await.unwrap(),
        Some((expected.header_hash().unwrap(), expected.height()))
    );
    expected
}

#[tokio::test]
#[ignore = "builds a CPU k18 plot and validates real bootstrap/signage/infusion proofs"]
async fn native_pos2_genesis_and_successor_through_farmer_timelord_handlers() {
    let _ = dg_logger::DruidGardenLoggerBuilder::new()
        .current_level(log::Level::Warn)
        .build()
        .init();
    let plot_key = SecretKey::key_gen_v3(&[0x21; 32], &[]).unwrap();
    let pool_key = SecretKey::key_gen_v3(&[0x22; 32], &[]).unwrap();
    let template = ProofOfSpace::v2(
        Bytes32::default(),
        Some(Bytes48::new(pool_key.sk_to_pk().to_bytes())),
        None,
        Bytes48::new(plot_key.sk_to_pk().to_bytes()),
        0,
        0,
        2,
        Vec::new().into(),
    );
    let mut constants = ConsensusConstants {
        rewards: RewardSchedule::NO_PREFARM,
        hard_fork2_height: 0,
        plot_size_v2: 18,
        number_zero_bits_plot_filter_v2: 0,
        difficulty_constant_factor: 1,
        difficulty_starting: 1,
        sub_slot_iters_starting: 1024,
        discriminant_size_bits: 16,
        ..SIMULATOR
    };
    let plot = NativePlot::build(
        ProofParams::new(template.get_plot_id().unwrap(), 18, 2, constants.is_testnet).unwrap(),
        PlotLimits::default(),
        &AtomicBool::new(false),
    )
    .expect("native CPU plot");
    let (genesis, proof) = (1..=128u8)
        .find_map(|nonce| {
            let mut genesis = SIMULATOR.genesis_challenge.bytes();
            genesis[0] = nonce;
            let genesis = Bytes32::new(genesis);
            farm(&plot, &template, genesis, genesis).map(|proof| (genesis, proof))
        })
        .expect("bounded genesis challenge search");
    constants.genesis_challenge = genesis;
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("bootstrap.sqlite");
    let store = Arc::new(SqliteStore::open(&database).await.unwrap());
    let node = Arc::new(
        FullNode::boot_with_store_constants(
            Config {
                p2p: P2pSettings::default(),
                listen: "127.0.0.1:0".parse().unwrap(),
                rpc: "127.0.0.1:0".parse().unwrap(),
                introducer: None,
                manual_peers: Vec::new(),
                advertise: None,
                backend: Backend::Sqlite(database),
                network_id: "dgx".to_string(),
                capture_dir: None,
                genesis_sync: true,
                sync_from: 0,
                uncompact: false,
                prefetch_memory_mb: None,
                prefetch_max_inflight: None,
                chain_definition: None,
                performance: Default::default(),
                trusted_peers: Vec::new(),
                trusted_cidrs: Vec::new(),
                rpc_tls: crate::config::RpcTlsMode::Local,
                debug_endpoints: false,
            },
            store,
            constants,
        )
        .unwrap(),
    );
    let registry: Arc<dyn OutboundPeers> =
        Arc::new(dg_xch_p2p::PeerRegistry::new(P2pSettings::default()));
    let api = node.peer_api();
    assert!(FullNodeApi::timelord_genesis(&api).await.is_some());
    assert!(!node.synced.load(Ordering::Relaxed));
    let unfinished = declare(&node, &plot_key, &pool_key, proof, 0, genesis, genesis).await;
    let genesis_block = infuse(&node, &unfinished, None, &registry).await;
    assert!(FullNodeApi::timelord_genesis(&api).await.is_none());
    let previous = node
        .store
        .get_block_record(&genesis_block.header_hash().unwrap())
        .await
        .unwrap()
        .unwrap();
    let records = HashMap::from([(previous.header_hash, previous.clone())]);
    let mut successor = None;
    for index in 4..61u8 {
        let sp_iters =
            calculate_sp_iters(&constants, constants.sub_slot_iters_starting, index).unwrap();
        let (cc_challenge, rc_challenge, cc_input, rc_input, cc_iters, rc_iters) =
            get_signage_point_vdf_info(
                &constants,
                &[],
                false,
                Some(&previous),
                &records,
                u128::from(sp_iters),
                sp_iters,
            )
            .unwrap();
        let (mut cc_vdf, cc_proof) = vdf(cc_challenge, cc_input, cc_iters);
        cc_vdf.number_of_iterations = sp_iters;
        let (rc_vdf, rc_proof) = vdf(rc_challenge, rc_input, rc_iters);
        let cc_hash = cc_vdf.output.hash().unwrap();
        let rc_hash = rc_vdf.output.hash().unwrap();
        let Some(proof) = farm(&plot, &template, genesis, cc_hash) else {
            continue;
        };
        FullNodeApi::on_new_signage_point_vdf(
            &api,
            Bytes32::default(),
            NewSignagePointVDF {
                index_from_challenge: index,
                challenge_chain_sp_vdf: cc_vdf,
                challenge_chain_sp_proof: cc_proof,
                reward_chain_sp_vdf: rc_vdf,
                reward_chain_sp_proof: rc_proof,
            },
        )
        .await;
        process_sp_inbox(&node).await;
        assert!(
            node.slot_state
                .lock()
                .await
                .get_signage_point(&cc_hash)
                .is_some()
        );
        assert!(
            node.sp_farmer_announce
                .lock()
                .await
                .iter()
                .any(|point| point.challenge_chain_sp == cc_hash)
        );
        successor =
            Some(declare(&node, &plot_key, &pool_key, proof, index, cc_hash, rc_hash).await);
        break;
    }
    let successor = successor.expect("bounded real-signage proof search");
    let rewards = &successor
        .transactions_info
        .as_ref()
        .unwrap()
        .reward_claims_incorporated;
    assert_eq!(rewards.len(), 2);
    assert_ne!(rewards[0].name(), rewards[1].name());
    assert!(rewards.iter().all(|coin| coin.amount == 0));
    let block = infuse(&node, &successor, Some(&previous), &registry).await;
    assert_eq!(block.height(), 1);
    assert_eq!(block.prev_header_hash(), previous.header_hash);
    assert!(
        block
            .transactions_info
            .as_ref()
            .unwrap()
            .reward_claims_incorporated
            .iter()
            .all(|coin| coin.amount == 0)
    );
    assert_eq!(
        node.store
            .get_block(&block.header_hash().unwrap())
            .await
            .unwrap(),
        Some(block.clone())
    );
    let restarted_store = Arc::new(
        SqliteStore::open(&directory.path().join("restart.sqlite"))
            .await
            .unwrap(),
    );
    {
        let mut engine = Engine::new(restarted_store.clone(), NativePrimitives, constants);
        assert!(matches!(
            engine.add_block(&genesis_block).await.unwrap(),
            dg_xch_node::engine::AddBlockOutcome::NewPeak { height: 0 }
        ));
    }
    let mut restarted = Engine::new(restarted_store.clone(), NativePrimitives, constants);
    let mut invalid = block.clone();
    invalid.challenge_chain_ip_proof.witness = UnsizedBytes::new(vec![0]);
    assert!(restarted.add_block(&invalid).await.is_err());
    assert_eq!(
        restarted_store.get_peak().await.unwrap(),
        Some((previous.header_hash, 0))
    );
    assert!(matches!(
        restarted.add_block(&block).await.unwrap(),
        dg_xch_node::engine::AddBlockOutcome::Extended { height: 1 }
    ));
}
