use super::super::*;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::weight_proof::RecentChainData;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::block_header_validation::validate_pospace_and_get_required_iters;
use dg_xch_core::consensus::deficit::calculate_deficit;
use dg_xch_core::consensus::full_block_to_block_record::header_block_to_sub_block_record;
use dg_xch_core::consensus::get_block_challenge::pre_sp_tx_block_height;
use dg_xch_core::consensus::pot_iterations::is_overflow_block;
use dg_xch_node::PrimitiveVerifier;
use std::time::{SystemTime, UNIX_EPOCH};

// Golden ssi/difficulty for the slice's epoch (no epoch turn inside 9,054,336..=9,054,620 —
// the next is at 9,054,720) — the same reference pair the header-validation test validates against.
const SSI: u64 = 574_619_648;
const DIFF: u64 = 2608;

fn load_chain() -> Vec<HeaderBlock> {
    let bytes =
        include_bytes!("../../../../node/tests/fixtures/recent_chain_mainnet_9054336_9054620.bin");
    RecentChainData::from_bytes(
        &mut std::io::Cursor::new(&bytes[..]),
        ChiaProtocolVersion::default(),
    )
    .expect("recent chain slice deserializes")
    .recent_chain_data
}

// Light-path (proof-of-space only, no VDF) required_iters, to seed ancestor records with a
// correct pb.ip_iters — the header-validation seeding recipe
// (node/tests/header_validation.rs).
fn light_required_iters(
    ancestors: &HashMap<Bytes32, BlockRecord>,
    block: &HeaderBlock,
    challenge: Bytes32,
    prev_challenge: Bytes32,
    overflow: bool,
) -> u64 {
    let rcb = &block.reward_chain_block;
    let cc_sp_hash = match &rcb.challenge_chain_sp_vdf {
        None => challenge,
        Some(v) => v.output.hash().expect("cc sp hash"),
    };
    let pre = pre_sp_tx_block_height(
        &MAINNET,
        ancestors,
        block.prev_header_hash(),
        rcb.signage_point_index,
        block.finished_sub_slots.len(),
    )
    .expect("pre_sp_tx_block_height");
    validate_pospace_and_get_required_iters(
        &PrimitiveVerifier(&NativePrimitives),
        &MAINNET,
        &rcb.proof_of_space,
        if overflow { prev_challenge } else { challenge },
        cc_sp_hash,
        block.height(),
        DIFF,
        pre,
    )
    .expect("pospace")
    .expect("valid pospace")
}

fn build_records(chain: &[HeaderBlock]) -> Vec<BlockRecord> {
    let c = &MAINNET;
    let mut ancestors: HashMap<Bytes32, BlockRecord> = HashMap::new();
    let mut out: Vec<BlockRecord> = Vec::with_capacity(chain.len());
    let mut challenge = Some(chain[0].reward_chain_block.pos_ss_cc_challenge_hash);
    let mut prev_challenge: Option<Bytes32> = None;
    let mut prev_rec: Option<BlockRecord> = None;
    let mut deficit: u8 = 0;
    let mut tx_blocks: u32 = 0;
    let mut prev_tx_height: u32 = 0;

    for block in chain {
        let rcb = &block.reward_chain_block;
        let h = block.height();
        let mut overflow = false;
        let mut ses: Option<SubEpochSummary> = None;
        for ss in &block.finished_sub_slots {
            prev_challenge = Some(ss.challenge_chain.challenge_chain_end_of_slot_vdf.challenge);
            challenge = Some(ss.challenge_chain.hash().expect("cc hash"));
            deficit = ss.reward_chain.deficit;
            if let Some(ses_hash) = ss.challenge_chain.subepoch_summary_hash {
                ses = Some(SubEpochSummary {
                    prev_subepoch_summary_hash: ses_hash,
                    reward_chain_hash: ses_hash,
                    num_blocks_overflow: 0,
                    new_difficulty: None,
                    new_sub_slot_iters: None,
                });
            }
        }
        let mut required_iters = 0u64;
        if let (Some(ch), Some(pc)) = (challenge, prev_challenge)
            && tx_blocks > 2
        {
            overflow = is_overflow_block(c, rcb.signage_point_index).expect("overflow");
            deficit = calculate_deficit(
                c,
                h,
                prev_rec.as_ref(),
                overflow,
                block.finished_sub_slots.len(),
            );
            required_iters = light_required_iters(&ancestors, block, ch, pc, overflow);
        }
        let rec = header_block_to_sub_block_record(
            c,
            required_iters,
            block,
            SSI,
            overflow,
            deficit,
            prev_tx_height,
            ses,
        )
        .expect("record");
        ancestors.insert(rec.header_hash, rec.clone());
        out.push(rec.clone());
        if rcb.is_transaction_block {
            tx_blocks += 1;
            prev_tx_height = h;
        }
        prev_rec = Some(rec);
    }
    out
}

// Project a finished mainnet header block back to the UnfinishedBlock a peer would have
// relayed for it: strip the infusion-point VDFs, keep everything the farmer signed.
fn unfinished_from_header(hb: &HeaderBlock) -> UnfinishedBlock {
    UnfinishedBlock {
        finished_sub_slots: hb.finished_sub_slots.clone(),
        reward_chain_block: hb.reward_chain_block.get_unfinished(),
        challenge_chain_sp_proof: hb.challenge_chain_sp_proof.clone(),
        reward_chain_sp_proof: hb.reward_chain_sp_proof.clone(),
        foliage: hb.foliage,
        foliage_transaction_block: hb.foliage_transaction_block,
        transactions_info: hb.transactions_info.clone(),
        transactions_generator: None,
        transactions_generator_ref_list: Vec::new(),
    }
}

// The deepest slice block matching `want_tx` at a plain (index > 0, non-overflow,
// mid-sub-slot) signage point — deep ancestry below it, none of the first-in-slot special
// cases in the way.
fn pick_target(chain: &[HeaderBlock], want_tx: bool) -> usize {
    for i in (16..chain.len()).rev() {
        let b = &chain[i];
        let sp = b.reward_chain_block.signage_point_index;
        if b.foliage_transaction_block.is_some() == want_tx
            && b.finished_sub_slots.is_empty()
            && sp > 0
            && !is_overflow_block(&MAINNET, sp).expect("overflow")
        {
            return i;
        }
    }
    panic!("no suitable target block in the slice");
}

async fn node_with_slice_records() -> (Arc<FullNode<SqliteStore>>, Vec<HeaderBlock>) {
    let chain = load_chain();
    assert!(chain.len() > 280, "full slice present");
    let records = build_records(&chain);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let db = std::env::temp_dir().join(format!("fn_ubgate_{}_{nanos}.sqlite", std::process::id()));
    let store = open_backend(&Backend::Sqlite(db)).await.expect("store");
    store
        .add_block_records(&records)
        .await
        .expect("seed records");
    let node = Arc::new(
        FullNode::boot_with_store(
            Config {
                p2p: P2pSettings::default(),
                listen: "127.0.0.1:0".parse().unwrap(),
                rpc: "127.0.0.1:0".parse().unwrap(),
                introducer: None,
                manual_peers: Vec::new(),
                advertise: None,
                backend: Backend::Sqlite(std::path::PathBuf::from("unused")),
                network_id: "mainnet".to_string(),
                capture_dir: None,
                genesis_sync: false,
                sync_from: 0,
                uncompact: false,
                prefetch_memory_mb: None,
                prefetch_max_inflight: None,
                trusted_peers: Vec::new(),
                trusted_cidrs: Vec::new(),
                rpc_tls: crate::config::RpcTlsMode::Local,
                debug_endpoints: false,
            },
            store,
        )
        .expect("boot node"),
    );
    (node, chain)
}

async fn drive_one(node: &Arc<FullNode<SqliteStore>>, ub: UnfinishedBlock) {
    node.ub_inbox.lock().await.push(ub);
    process_ub_inbox(node).await;
}

async fn cached(node: &Arc<FullNode<SqliteStore>>, ub: &UnfinishedBlock) -> bool {
    let partial = ub.reward_chain_block.hash().expect("partial hash");
    let foliage = ub.foliage.foliage_transaction_block_hash;
    node.unfinished
        .lock()
        .await
        .get_block2(&partial, foliage.as_ref())
        .0
        .is_some()
}

#[tokio::test]
async fn header_valid_ub_with_bogus_generator_is_dropped_not_cached_not_relayed() {
    let (node, chain) = node_with_slice_records().await;
    let target = &chain[pick_target(&chain, false)];
    let mut ub = unfinished_from_header(target);
    // A generator that is not even deserializable CLVM — the block is rejected without
    // ever reaching a peer.
    ub.transactions_generator = Some(SerializedProgram::from_hex("fffefd").expect("hex"));
    let poisoned = ub.clone();
    drive_one(&node, ub).await;

    assert!(
        !cached(&node, &poisoned).await,
        "a generator-invalid unfinished block must NOT enter the served cache"
    );
    assert_eq!(
        node.ub_announce.lock().await.len(),
        0,
        "a generator-invalid unfinished block must NOT be queued for relay"
    );
    assert_eq!(
        node.producer.dropped_count("ub_body_fail"),
        1,
        "the drop must be counted by the transactions gate (proves the header stage passed)"
    );
    assert_eq!(
        node.producer.dropped_count("ub_validation_fail"),
        0,
        "the header stage must not be the rejector (vacuity guard)"
    );
}

#[tokio::test]
async fn tx_ub_with_generator_root_mismatch_is_dropped_not_relayed() {
    let (node, chain) = node_with_slice_records().await;
    let target = &chain[pick_target(&chain, true)];
    assert!(
        target.transactions_info.is_some(),
        "fixture shape: the recent-chain slice carries transactions_info for tx blocks"
    );
    let mut ub = unfinished_from_header(target);
    ub.transactions_generator = Some(SerializedProgram::from_hex("ff0880").expect("hex"));
    let poisoned = ub.clone();
    drive_one(&node, ub).await;

    assert!(
        !cached(&node, &poisoned).await,
        "must not enter the served cache"
    );
    assert_eq!(
        node.ub_announce.lock().await.len(),
        0,
        "must not be queued for relay"
    );
    assert_eq!(node.producer.dropped_count("ub_body_fail"), 1);
    assert_eq!(node.producer.dropped_count("ub_validation_fail"), 0);
}

#[tokio::test]
async fn honest_ub_without_generator_still_validates_caches_and_announces() {
    let (node, chain) = node_with_slice_records().await;
    let target = &chain[pick_target(&chain, false)];
    let ub = unfinished_from_header(target);
    let honest = ub.clone();
    drive_one(&node, ub).await;

    assert!(
        cached(&node, &honest).await,
        "an honest unfinished block must still be cached (no false positive)"
    );
    let announces = node.ub_announce.lock().await;
    assert_eq!(announces.len(), 1, "exactly one relay announce queued");
    assert_eq!(
        announces[0].unfinished_reward_hash,
        honest.reward_chain_block.hash().expect("partial hash"),
    );
    drop(announces);
    for reason in ["ub_body_fail", "ub_generator_fail", "ub_cost_mismatch"] {
        assert_eq!(
            node.producer.dropped_count(reason),
            0,
            "no transactions-gate drop for an honest block ({reason})"
        );
    }
}

// The dedup ladder (seen set + get_unfinished_block2) sits in front of the
// validators: an exact duplicate in the same drain is dropped by the seen set, and a NEW
// serialization at an already-cached (reward, foliage) — e.g. the same block with a
// generator spliced in — is dropped by the cache check BEFORE any validation runs. One
// announce total: a burst of duplicates costs one header validation and one generator run
// (the DoS bound).
#[tokio::test]
async fn duplicate_ubs_are_deduped_and_validated_once() {
    let (node, chain) = node_with_slice_records().await;
    let target = &chain[pick_target(&chain, false)];
    let ub = unfinished_from_header(target);
    let honest = ub.clone();
    // Two exact copies in one drain: the second dies on the seen set.
    node.ub_inbox.lock().await.push(ub.clone());
    node.ub_inbox.lock().await.push(ub);
    process_ub_inbox(&node).await;
    assert!(cached(&node, &honest).await);
    assert_eq!(
        node.ub_announce.lock().await.len(),
        1,
        "one announce, not two"
    );
    assert_eq!(node.producer.dropped_count("ub_duplicate"), 1);

    // A DIFFERENT serialization of the same (reward, foliage) — the cached block with a
    // bogus generator spliced in — dies on the already-cached check, before the header or
    // generator validators run (and without evicting the honest cached entry).
    let mut poisoned = honest.clone();
    poisoned.transactions_generator = Some(SerializedProgram::from_hex("fffefd").expect("hex"));
    drive_one(&node, poisoned).await;
    assert_eq!(node.producer.dropped_count("ub_already_cached"), 1);
    assert_eq!(node.producer.dropped_count("ub_body_fail"), 0);
    assert!(cached(&node, &honest).await, "the honest entry survives");
    assert_eq!(node.ub_announce.lock().await.len(), 1, "still one announce");
}
