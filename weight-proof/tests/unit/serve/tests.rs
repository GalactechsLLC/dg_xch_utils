use super::*;
use async_trait::async_trait;
use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::foliage::Foliage;
use dg_xch_core::blockchain::foliage_block_data::FoliageBlockData;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::infused_challenge_chain_subslot::InfusedChallengeChainSubSlot;
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::reward_chain_block::RewardChainBlock;
use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
use dg_xch_core::blockchain::sized_bytes::{Bytes48, Bytes96};
use dg_xch_core::blockchain::subslot_bundle::SubSlotBundle;
use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_stores::types::{BatchHandle, BlockStatus, Savepoint};
use dg_xch_stores::{BlockStore, StoreError};
use std::collections::HashMap;

// An in-memory BlockStore double: exactly the query surface the builder touches
// (get_block_record / get_block_record_by_height / get_block, plus the persisted-segment
// seam); everything else errors. `block_gets`/`segment_persists` count store traffic so the
// tests can assert a rebuild served from persisted segments instead of walking blocks.
struct MemStore {
    by_height: HashMap<u32, BlockRecord>,
    by_hash: HashMap<Bytes32, BlockRecord>,
    blocks: HashMap<Bytes32, FullBlock>,
    segments: std::sync::Mutex<HashMap<Bytes32, Vec<u8>>>,
    block_gets: std::sync::atomic::AtomicUsize,
    segment_persists: std::sync::atomic::AtomicUsize,
}

fn unsupported() -> StoreError {
    StoreError::Corrupt("unsupported in MemStore".into())
}

#[async_trait]
impl BlockStore for MemStore {
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        Ok(self.by_hash.get(hh).cloned())
    }
    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        Ok(self.by_height.get(&h).cloned())
    }
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        Ok(self
            .by_height
            .keys()
            .max()
            .and_then(|h| self.by_height.get(h).map(|r| (r.header_hash, r.height))))
    }
    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        Ok(self.by_height.keys().min().copied())
    }
    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        self.block_gets
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.blocks.get(hh).cloned())
    }
    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .segments
            .lock()
            .expect("segments")
            .get(ses_hash)
            .cloned())
    }
    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.segment_persists
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.segments
            .lock()
            .expect("segments")
            .insert(*ses_hash, bytes.to_vec());
        Ok(())
    }
    async fn add_block_records(&self, _records: &[BlockRecord]) -> Result<(), StoreError> {
        Err(unsupported())
    }
    async fn add_block_records_in(
        &self,
        _batch: &mut BatchHandle,
        _records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        Err(unsupported())
    }
    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        Err(unsupported())
    }
    async fn append_many(
        &self,
        _batch: &mut BatchHandle,
        _blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        Err(unsupported())
    }
    async fn commit(&self, _batch: BatchHandle) -> Result<(), StoreError> {
        Err(unsupported())
    }
    async fn get_unassociated(&self, _limit: usize) -> Result<Vec<u32>, StoreError> {
        Ok(Vec::new())
    }
    async fn set_peak(&self, _new_peak: &Bytes32) -> Result<u64, StoreError> {
        Err(unsupported())
    }
    async fn set_peak_in(
        &self,
        _batch: &mut BatchHandle,
        _new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        Err(unsupported())
    }
    async fn get_status(&self, _hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        Err(unsupported())
    }
    async fn set_status(&self, _hh: &Bytes32, _s: BlockStatus) -> Result<(), StoreError> {
        Err(unsupported())
    }
    async fn set_status_in(
        &self,
        _batch: &mut BatchHandle,
        _hh: &Bytes32,
        _s: BlockStatus,
    ) -> Result<(), StoreError> {
        Err(unsupported())
    }
    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        Err(unsupported())
    }
    async fn rollback(&self, _sp: Savepoint) -> Result<u64, StoreError> {
        Err(unsupported())
    }
    async fn get_generator_at_height(
        &self,
        _h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        Ok(None)
    }
    async fn build_indexes(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

fn h32(n: u32) -> Bytes32 {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_be_bytes());
    b[4] = 0xab; // distinguish from Bytes32::default()
    Bytes32::from(b)
}

fn zero_vdf_info() -> VdfInfo {
    VdfInfo {
        challenge: Bytes32::default(),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    }
}

fn zero_proof() -> VdfProof {
    VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::new(Vec::new()),
        normalized_to_identity: false,
    }
}

fn slot_bundle() -> SubSlotBundle {
    SubSlotBundle {
        challenge_chain: ChallengeChainSubSlot {
            challenge_chain_end_of_slot_vdf: zero_vdf_info(),
            infused_challenge_chain_sub_slot_hash: None,
            subepoch_summary_hash: None,
            new_sub_slot_iters: None,
            new_difficulty: None,
        },
        infused_challenge_chain: Some(InfusedChallengeChainSubSlot {
            infused_challenge_chain_end_of_slot_vdf: zero_vdf_info(),
        }),
        reward_chain: RewardChainSubSlot {
            end_of_slot_vdf: zero_vdf_info(),
            challenge_chain_sub_slot_hash: Bytes32::default(),
            infused_challenge_chain_sub_slot_hash: None,
            deficit: MAINNET.min_blocks_per_challenge_block,
        },
        proofs: SubSlotProofs {
            challenge_chain_slot_proof: zero_proof(),
            infused_challenge_chain_slot_proof: Some(zero_proof()),
            reward_chain_slot_proof: zero_proof(),
        },
    }
}

fn ses(n: u8) -> SubEpochSummary {
    SubEpochSummary {
        prev_subepoch_summary_hash: h32(u32::from(n)),
        reward_chain_hash: h32(1000 + u32::from(n)),
        num_blocks_overflow: n,
        new_difficulty: None,
        new_sub_slot_iters: None,
    }
}

// One synthetic main-chain block: unique foliage (hence header hash) per height, weight = height+1,
// total_iters = (height+1) * 10_000_000 (far above any ip/sp iters the constants derive), slot
// starts and ses carriers as flagged. Structurally consistent for CONSTRUCTION (the builder never
// verifies VDFs/PoSpace — validation-grade chains need real proofs and are gated behind a corpus).
struct ChainSpec {
    len: u32,
    slot_every: u32,
    ses_heights: Vec<(u32, SubEpochSummary)>,
    challenge_heights: Vec<u32>,
}

fn build_chain(spec: &ChainSpec) -> MemStore {
    let mut store = MemStore {
        by_height: HashMap::new(),
        by_hash: HashMap::new(),
        blocks: HashMap::new(),
        segments: std::sync::Mutex::new(HashMap::new()),
        block_gets: std::sync::atomic::AtomicUsize::new(0),
        segment_persists: std::sync::atomic::AtomicUsize::new(0),
    };
    let mut prev_hash = Bytes32::default();
    for h in 0..spec.len {
        let first_in_sub_slot = h > 0 && h.is_multiple_of(spec.slot_every);
        let ses_included = spec
            .ses_heights
            .iter()
            .find(|(sh, _)| *sh == h)
            .map(|(_, s)| *s);
        let is_challenge = spec.challenge_heights.contains(&h);
        let finished_sub_slots = if first_in_sub_slot {
            let mut bundle = slot_bundle();
            if let Some(s) = &ses_included {
                bundle.challenge_chain.subepoch_summary_hash =
                    Some(s.hash().expect("hash test ses"));
            }
            vec![bundle]
        } else {
            assert!(
                ses_included.is_none(),
                "test chain: ses heights must be slot starts"
            );
            Vec::new()
        };
        let block = FullBlock {
            finished_sub_slots,
            reward_chain_block: RewardChainBlock {
                weight: u128::from(h) + 1,
                height: h,
                total_iters: (u128::from(h) + 1) * 10_000_000,
                signage_point_index: 0,
                pos_ss_cc_challenge_hash: Bytes32::default(),
                proof_of_space: ProofOfSpace {
                    version: 0,
                    plot_index: 0,
                    meta_group: 0,
                    strength: 0,
                    challenge: Bytes32::default(),
                    pool_public_key: None,
                    pool_contract_puzzle_hash: Some(Bytes32::default()),
                    plot_public_key: Bytes48::default(),
                    size: 32,
                    proof: Vec::new().into(),
                },
                challenge_chain_sp_vdf: None,
                challenge_chain_sp_signature: Bytes96::default(),
                challenge_chain_ip_vdf: zero_vdf_info(),
                reward_chain_sp_vdf: None,
                reward_chain_sp_signature: Bytes96::default(),
                reward_chain_ip_vdf: zero_vdf_info(),
                infused_challenge_chain_ip_vdf: None,
                is_transaction_block: false,
            },
            challenge_chain_sp_proof: None,
            challenge_chain_ip_proof: zero_proof(),
            reward_chain_sp_proof: None,
            reward_chain_ip_proof: zero_proof(),
            infused_challenge_chain_ip_proof: None,
            foliage: Foliage {
                prev_block_hash: prev_hash,
                reward_block_hash: h32(h), // unique foliage → unique header hash
                foliage_block_data: FoliageBlockData {
                    unfinished_reward_block_hash: Bytes32::default(),
                    pool_target: PoolTarget {
                        puzzle_hash: Bytes32::default(),
                        max_height: 0,
                    },
                    pool_signature: None,
                    farmer_reward_puzzle_hash: Bytes32::default(),
                    extension_data: Bytes32::default(),
                },
                foliage_block_data_signature: Bytes96::default(),
                foliage_transaction_block_hash: None,
                foliage_transaction_block_signature: None,
            },
            foliage_transaction_block: None,
            transactions_info: None,
            transactions_generator: None,
            transactions_generator_ref_list: Vec::new(),
        };
        let header_hash = block.header_hash().expect("hash fake block");
        let record = BlockRecord {
            header_hash,
            prev_hash,
            height: h,
            weight: u128::from(h) + 1,
            total_iters: (u128::from(h) + 1) * 10_000_000,
            signage_point_index: 0,
            challenge_vdf_output: ClassgroupElement::get_default_element(),
            infused_challenge_vdf_output: None,
            reward_infusion_new_challenge: Bytes32::default(),
            challenge_block_info_hash: Bytes32::default(),
            sub_slot_iters: MAINNET.sub_slot_iters_starting,
            pool_puzzle_hash: Bytes32::default(),
            farmer_puzzle_hash: Bytes32::default(),
            required_iters: 1,
            deficit: if is_challenge {
                MAINNET.min_blocks_per_challenge_block - 1
            } else {
                MAINNET.min_blocks_per_challenge_block
            },
            overflow: false,
            prev_transaction_block_height: 0,
            timestamp: None,
            prev_transaction_block_hash: None,
            fees: None,
            reward_claims_incorporated: None,
            finished_challenge_slot_hashes: if first_in_sub_slot {
                Some(vec![h32(2000 + h)])
            } else {
                None
            },
            finished_infused_challenge_slot_hashes: None,
            finished_reward_slot_hashes: if first_in_sub_slot {
                Some(vec![h32(3000 + h)])
            } else {
                None
            },
            sub_epoch_summary_included: ses_included,
        };
        store.by_height.insert(h, record.clone());
        store.by_hash.insert(header_hash, record);
        store.blocks.insert(header_hash, block);
        prev_hash = header_hash;
    }
    store
}

fn server_over(spec: &ChainSpec) -> WeightProofServer<MemStore> {
    WeightProofServer::new(Arc::new(build_chain(spec)), MAINNET)
}

// A tip we do not hold is refused
// (no reply) — never built, never a panic.
#[tokio::test]
async fn refuses_unknown_tip() {
    let server = server_over(&ChainSpec {
        len: 10,
        slot_every: 4,
        ses_heights: vec![],
        challenge_heights: vec![],
    });
    let err = server
        .get_proof_of_weight(h32(999_999))
        .await
        .expect_err("unknown tip must refuse");
    assert!(matches!(err, ServeError::UnknownTip(_)));
    assert!(err.is_refusal());
}

// A known tip below
// WEIGHT_PROOF_RECENT_BLOCKS (mainnet 1000) is refused.
#[tokio::test]
async fn refuses_chain_shorter_than_weight_proof_recent_blocks() {
    let spec = ChainSpec {
        len: 500,
        slot_every: 10,
        ses_heights: vec![(400, ses(0))],
        challenge_heights: vec![],
    };
    let store = build_chain(&spec);
    let tip = store.by_height[&499].header_hash;
    let server = WeightProofServer::new(Arc::new(store), MAINNET);
    let err = server
        .get_proof_of_weight(tip)
        .await
        .expect_err("short chain must refuse");
    assert!(
        matches!(
            err,
            ServeError::ChainTooShort {
                height: 499,
                required: 1000
            }
        ),
        "got {err:?}"
    );
    assert!(err.is_refusal());
}

// For ses blocks at 400 and 800 and
// tip 1050, min_height = 800-1 … no — the walk collects TWO summaries going down (800 then 400),
// so the chain must span from the block BEFORE the second summary (height 399) to the tip. Every
// header must carry the tx_filter=False empty BIP158 filter byte.
#[tokio::test]
async fn recent_chain_spans_block_before_second_last_ses_to_tip() {
    let spec = ChainSpec {
        len: 1051,
        slot_every: 10,
        ses_heights: vec![(400, ses(0)), (800, ses(1))],
        challenge_heights: vec![],
    };
    let store = build_chain(&spec);
    let ses_blocks = vec![store.by_height[&400].clone(), store.by_height[&800].clone()];
    let server = WeightProofServer::new(Arc::new(store), MAINNET);
    let chain = server
        .get_recent_chain(&ses_blocks, 1050)
        .await
        .expect("recent chain builds");
    assert_eq!(chain.first().map(HeaderBlock::height), Some(399));
    assert_eq!(chain.last().map(HeaderBlock::height), Some(1050));
    assert_eq!(chain.len(), 652);
    for header in &chain {
        assert_eq!(
            header.transactions_filter.as_slice(),
            &[0u8],
            "tx_filter=False headers carry the one-byte empty BIP158 filter"
        );
    }
}

// From a start at height 10 with slot
// starts at 8 and 4, the walk counts the two slot starts and STILL steps below the second — the
// reference assigns `curr_rec = blocks[height-1]` after the count, so the answer is 3, not 4.
#[tokio::test]
async fn prev_two_slots_height_steps_below_the_second_slot_start() {
    let spec = ChainSpec {
        len: 12,
        slot_every: 4,
        ses_heights: vec![],
        challenge_heights: vec![],
    };
    let store = build_chain(&spec);
    let se_start = store.by_height[&10].clone();
    let server = WeightProofServer::new(Arc::new(store), MAINNET);
    assert_eq!(
        server
            .get_prev_two_slots_height(&se_start)
            .await
            .expect("walk"),
        3
    );
}

// The sampling seed is the hash of
// the SECOND-TO-LAST summary at-or-below the tip — for summaries at 400/800/1200 and tip 1050,
// that is the summary at 400 (1200 is above the tip and must not count).
#[test]
fn seed_is_hash_of_second_to_last_summary_at_or_below_tip() {
    let spec = ChainSpec {
        len: 1251,
        slot_every: 10,
        ses_heights: vec![(400, ses(0)), (800, ses(1)), (1200, ses(2))],
        challenge_heights: vec![],
    };
    let store = build_chain(&spec);
    let ses_blocks = vec![
        store.by_height[&400].clone(),
        store.by_height[&800].clone(),
        store.by_height[&1200].clone(),
    ];
    let seed = get_seed_for_proof(&ses_blocks, 1050).expect("seed");
    assert_eq!(seed, ses(0).hash().expect("hash"));
}

// The construction smoke test on a synthetic minimal chain, hand-derived from the reference:
//
// Chain: 1101 blocks, slot starts every 10, summaries at 400 (ses0) and 800 (ses1), challenge
// blocks (deficit 15) at 405 and 801, tip 1100. Weights grow by 1 per block, so the recent chain
// (399..=1100) spans 702/1101 of the total weight: delta≈0.64 ⇒ prob_of_adv_succeeding =
// 1 - ln(0.5)/ln(delta) < 0 ⇒ `_get_weights_for_sampling` returns None ⇒ EVERY sub-epoch is
// sampled (the `weight_to_check is None` short-circuit).
//
// Expected segments, walked by hand:
// - sub-epoch 0 (heights 0..400): no challenge blocks ⇒ zero segments.
// - sub-epoch 1 (heights 400..800): one challenge block at 405 ⇒ ONE segment, sub_epoch_n=1,
//   and — being the sub-epoch's first segment past sub-epoch 0 — it carries rc_slot_end_info,
//   which is ses0's slot-opening reward-chain end-of-slot VDF.
//   Its sub_slots, in order:
//   · 1 finished-slot entry for slot start 400
//   · 5 bare ip entries for blocks 400..=404
//   · 1 challenge-block entry (proof of space present)
//   · then __slot_end_vdf over 406..=800 until the next challenge block at 801: a block
//     entry per height (395) plus an end-of-slot entry per slot start 410,420,…,800 (40) = 435
//   ⇒ 442 sub_slots total, exactly one carrying a proof of space, 41 carrying cc_slot_end.
#[tokio::test]
async fn builds_one_segment_per_challenge_block_with_hand_derived_shape() {
    let spec = ChainSpec {
        len: 1101,
        slot_every: 10,
        ses_heights: vec![(400, ses(0)), (800, ses(1))],
        challenge_heights: vec![405, 801],
    };
    let store = build_chain(&spec);
    let tip = store.by_height[&1100].header_hash;
    let server = WeightProofServer::new(Arc::new(store), MAINNET);
    let wp = server.get_proof_of_weight(tip).await.expect("proof builds");

    assert_eq!(wp.sub_epochs.len(), 2);
    assert_eq!(wp.sub_epochs[0], create_sub_epoch_data(&ses(0)));
    assert_eq!(wp.sub_epochs[1], create_sub_epoch_data(&ses(1)));

    // Recent chain: block before ses0 (399) to tip (1100).
    assert_eq!(
        wp.recent_chain_data.first().map(HeaderBlock::height),
        Some(399)
    );
    assert_eq!(
        wp.recent_chain_data.last().map(HeaderBlock::height),
        Some(1100)
    );

    // One segment total: sub-epoch 0 has no challenge blocks, sub-epoch 1 has exactly one.
    assert_eq!(wp.sub_epoch_segments.len(), 1);
    let seg = &wp.sub_epoch_segments[0];
    assert_eq!(seg.sub_epoch_n, 1);
    // First segment of a non-zero sub-epoch carries the rc end-of-slot VDF of ses0's slot.
    assert_eq!(seg.rc_slot_end_info, Some(zero_vdf_info()));

    assert_eq!(seg.sub_slots.len(), 442);
    assert_eq!(
        seg.sub_slots
            .iter()
            .filter(|s| s.proof_of_space.is_some())
            .count(),
        1,
        "exactly the challenge block carries a proof of space"
    );
    assert_eq!(
        seg.sub_slots
            .iter()
            .filter(|s| s.cc_slot_end.is_some())
            .count(),
        41,
        "one end-of-slot entry per slot start in the segment span"
    );
    // The challenge-block entry sits right after the pre-challenge entries (index 6).
    assert!(seg.sub_slots[6].proof_of_space.is_some());
    assert_eq!(
        seg.sub_slots[6].total_iters,
        Some(406u128 * 10_000_000),
        "challenge entry carries the record's total_iters"
    );

    let again = server.get_proof_of_weight(tip).await.expect("cached");
    assert!(
        Arc::ptr_eq(&wp, &again),
        "same tip must hit the tip-keyed cache, not rebuild"
    );
}

#[tokio::test]
async fn built_proof_passes_validator_phase2_summary_anchor() {
    let ses0 = SubEpochSummary {
        prev_subepoch_summary_hash: MAINNET.genesis_challenge,
        reward_chain_hash: h32(1000),
        num_blocks_overflow: 3,
        new_difficulty: None,
        new_sub_slot_iters: None,
    };
    let ses1 = SubEpochSummary {
        prev_subepoch_summary_hash: ses0.hash().expect("hash ses0"),
        reward_chain_hash: h32(1001),
        num_blocks_overflow: 5,
        new_difficulty: None,
        new_sub_slot_iters: None,
    };
    let spec = ChainSpec {
        len: 1101,
        slot_every: 10,
        ses_heights: vec![(400, ses0), (800, ses1)],
        challenge_heights: vec![],
    };
    let store = build_chain(&spec);
    let tip = store.by_height[&1100].header_hash;
    let server = WeightProofServer::new(Arc::new(store), MAINNET);
    let wp = server.get_proof_of_weight(tip).await.expect("proof builds");

    let summaries = crate::sub_epoch_summaries_of(&wp, &MAINNET).expect(
        "the validator's _validate_sub_epoch_summaries mirror must accept a served proof \
         (genesis-anchored chain terminating in the recent chain's on-chain commitment)",
    );
    assert_eq!(summaries.len(), 2);
    assert_eq!(
        summaries[0], ses0,
        "reconstructed ses0 mirrors the stored summary"
    );
    assert_eq!(
        summaries[1], ses1,
        "reconstructed ses1 mirrors the stored summary"
    );
}

// Every sampled sub-epoch's built segments are persisted through the store, keyed by the ses
// block's header hash, as the ChiaSerialize bytes of a SubEpochSegments wrapper. On the smoke
// chain both sub-epochs are sampled (weight_to_check is None): sub-epoch 0 persists an EMPTY
// list (the empty build result is persisted too — only a None build errors), sub-epoch 1
// persists its one segment.
#[tokio::test]
async fn built_segments_are_persisted_keyed_by_ses_hash() {
    use dg_xch_core::blockchain::weight_proof::SubEpochSegments;
    use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};

    let spec = ChainSpec {
        len: 1101,
        slot_every: 10,
        ses_heights: vec![(400, ses(0)), (800, ses(1))],
        challenge_heights: vec![405, 801],
    };
    let store = Arc::new(build_chain(&spec));
    let tip = store.by_height[&1100].header_hash;
    let ses0_hash = store.by_height[&400].header_hash;
    let ses1_hash = store.by_height[&800].header_hash;
    let server = WeightProofServer::new(store.clone(), MAINNET);
    let wp = server.get_proof_of_weight(tip).await.expect("proof builds");

    assert_eq!(
        store
            .segment_persists
            .load(std::sync::atomic::Ordering::Relaxed),
        2,
        "one persist per sampled sub-epoch"
    );
    let decode = |hh: &Bytes32| -> Vec<SubEpochChallengeSegment> {
        let bytes = store
            .segments
            .lock()
            .expect("segments")
            .get(hh)
            .cloned()
            .expect("persisted under the ses block hash");
        SubEpochSegments::from_bytes(
            &mut std::io::Cursor::new(&bytes[..]),
            ChiaProtocolVersion::Chia0_0_37,
        )
        .expect("persisted bytes decode as SubEpochSegments")
        .challenge_segments
    };
    assert_eq!(
        decode(&ses0_hash),
        Vec::new(),
        "sub-epoch 0 built no segments"
    );
    assert_eq!(
        decode(&ses1_hash),
        wp.sub_epoch_segments,
        "sub-epoch 1's persisted segments are exactly the served ones"
    );
}

// Get_sub_epoch_challenge_segments
// is checked BEFORE building, so a fresh handler over a store that already holds the segments
// must not walk the sub-epoch spans again. Restart-shaped: server B is a brand-new instance
// (empty in-memory LRU) over the same store server A persisted into. The block-read budget
// proves it: A's build reads the recent chain (heights 399..=1100 → 702 get_block calls) PLUS
// both segment spans — 0..=528 (529) and 389..=928 (540; se_start 400 is itself a slot start,
// so the two-slots walk stops one below 390) — while B's build must spend exactly the
// recent-chain 702 and nothing else, persist nothing new, and serve the identical proof.
#[tokio::test]
async fn fresh_server_over_same_store_rebuilds_from_persisted_segments() {
    let spec = ChainSpec {
        len: 1101,
        slot_every: 10,
        ses_heights: vec![(400, ses(0)), (800, ses(1))],
        challenge_heights: vec![405, 801],
    };
    let store = Arc::new(build_chain(&spec));
    let tip = store.by_height[&1100].header_hash;

    let server_a = WeightProofServer::new(store.clone(), MAINNET);
    let wp_a = server_a
        .get_proof_of_weight(tip)
        .await
        .expect("first build");
    let gets_a = store.block_gets.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(gets_a, 702 + 529 + 540, "first build walks all three spans");

    let server_b = WeightProofServer::new(store.clone(), MAINNET);
    let wp_b = server_b
        .get_proof_of_weight(tip)
        .await
        .expect("rebuild over the persisted store");
    let gets_b = store.block_gets.load(std::sync::atomic::Ordering::Relaxed) - gets_a;
    assert_eq!(
        gets_b, 702,
        "rebuild reads only the recent chain — segments come from the store, not a block walk"
    );
    assert_eq!(
        store
            .segment_persists
            .load(std::sync::atomic::Ordering::Relaxed),
        2,
        "rebuild persists nothing new"
    );
    assert_eq!(
        *wp_a, *wp_b,
        "persisted segments reproduce the identical proof"
    );
}
