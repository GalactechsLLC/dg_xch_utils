use crate::WeightProofError;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::weight_proof::{
    SubEpochChallengeSegment, SubEpochData, SubEpochSegments, SubSlotData, WeightProof,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::vdf_info_computation::get_signage_point_vdf_info;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::{BlockStore, StoreError};
use log::{debug, info};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;
use tokio::sync::Mutex;

// The on-disk encoding version for persisted SubEpochSegments (the store holds the bytes
// opaquely). Pinned to the
// same version the store backends pin for record blobs; the segment types serialize identically
// across current protocol versions, but a pin keeps persisted bytes stable by construction.
const SEGMENT_STORE_VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;

#[derive(Debug)]
pub enum ServeError {
    /// The requested tip is not a block we hold (no reply).
    UnknownTip(Bytes32),
    /// Tip height below `WEIGHT_PROOF_RECENT_BLOCKS` (no reply).
    ChainTooShort { height: u32, required: u32 },
    /// Fewer than two sub-epoch summaries at-or-below the tip (no reply).
    NotEnoughSubEpochs,
    /// The store errored.
    Store(StoreError),
    /// A main-chain height had no record/body — the store cannot back a proof to this tip.
    MissingBlock(u32),
    /// A record/header referenced by hash during segment construction was outside the loaded span.
    MissingRecord(Bytes32),
    /// A structural invariant the reference asserts did not hold while building.
    Build(String),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::UnknownTip(tip) => write!(f, "unknown tip {tip}"),
            ServeError::ChainTooShort { height, required } => {
                write!(
                    f,
                    "chain too short for weight proof: tip {height} < {required}"
                )
            }
            ServeError::NotEnoughSubEpochs => write!(f, "not enough sub epochs"),
            ServeError::Store(e) => write!(f, "store error: {e}"),
            ServeError::MissingBlock(h) => write!(f, "missing block at height {h}"),
            ServeError::MissingRecord(hh) => write!(f, "missing record {hh}"),
            ServeError::Build(msg) => write!(f, "weight proof build failed: {msg}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<StoreError> for ServeError {
    fn from(e: StoreError) -> Self {
        ServeError::Store(e)
    }
}

impl From<std::io::Error> for ServeError {
    fn from(e: std::io::Error) -> Self {
        ServeError::Build(e.to_string())
    }
}

impl From<WeightProofError> for ServeError {
    fn from(e: WeightProofError) -> Self {
        ServeError::Build(format!("{e:?}"))
    }
}

impl ServeError {
    /// True for the refusal paths where the peer simply gets no reply.
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            ServeError::UnknownTip(_)
                | ServeError::ChainTooShort { .. }
                | ServeError::NotEnoughSubEpochs
        )
    }
}

// An in-RAM view of one contiguous main-chain span — the counterpart of the reference's
// `get_block_records_in_range` + `get_header_blocks_in_range(tx_filter=False)` dict pair plus
// `height_to_hash`. Heights that don't resolve in the store are simply
// absent, since the range fetch collects only existing hashes; a later lookup miss errors.
struct ChainCache {
    height_to_hash: HashMap<u32, Bytes32>,
    records: HashMap<Bytes32, BlockRecord>,
    headers: HashMap<Bytes32, HeaderBlock>,
}

impl ChainCache {
    fn hash_at(&self, height: u32) -> Result<Bytes32, ServeError> {
        self.height_to_hash
            .get(&height)
            .copied()
            .ok_or(ServeError::MissingBlock(height))
    }

    fn record(&self, hh: &Bytes32) -> Result<&BlockRecord, ServeError> {
        self.records.get(hh).ok_or(ServeError::MissingRecord(*hh))
    }

    fn header(&self, hh: &Bytes32) -> Result<&HeaderBlock, ServeError> {
        self.headers.get(hh).ok_or(ServeError::MissingRecord(*hh))
    }

    fn header_at(&self, height: u32) -> Result<&HeaderBlock, ServeError> {
        self.header(&self.hash_at(height)?)
    }
}

// The builder's mutable state, all under one async lock (see `WeightProofServer::state`).
struct ServeState {
    // Whole-proof cache keyed by tip, checked and refreshed under the lock.
    proof: Option<(Bytes32, Arc<WeightProof>)>,
    // The sub-epoch-summary index: every main-chain record carrying `sub_epoch_summary_included`,
    // ascending by height. Our store deliberately has no ses schema, so it is DERIVED by walking
    // records (the same approach `sub_epoch_summaries_of` takes from a proof) — incrementally, so
    // the full walk is paid once per server, then only the delta above `walked_to`.
    ses_blocks: Vec<BlockRecord>,
    walked_to: Option<u32>,
    // Built segments keyed by the ses block's header hash, LRU-bounded to `MAX_SAMPLES` sub-epochs
    // — the hot layer over the store's durable `sub_epoch_segments_v3` rows. A miss here
    // falls through to `BlockStore::get_sub_epoch_segments` before any block walking; a build
    // persists through `persist_sub_epoch_segments`.
    segments: VecDeque<(Bytes32, Arc<Vec<SubEpochChallengeSegment>>)>,
}

impl ServeState {
    fn cached_segments(&mut self, hh: &Bytes32) -> Option<Arc<Vec<SubEpochChallengeSegment>>> {
        let idx = self.segments.iter().position(|(k, _)| k == hh)?;
        // Move the hit to the back so eviction pops the least-recently-used ses first.
        let entry = self.segments.remove(idx)?;
        let segs = entry.1.clone();
        self.segments.push_back(entry);
        Some(segs)
    }

    fn cache_segments(&mut self, hh: Bytes32, segs: Arc<Vec<SubEpochChallengeSegment>>) {
        self.segments.push_back((hh, segs));
        while self.segments.len() > crate::MAX_SAMPLES {
            self.segments.pop_front();
        }
    }
}

/// The construction-side handler: builds — and caches — the weight proof for a requested tip out
/// of a `BlockStore`. One instance per node; the internal lock doubles as the async
/// single-flight: concurrent requests for the same tip serialize on it, the first builds, the
/// rest return the cached proof.
pub struct WeightProofServer<S: ?Sized> {
    store: Arc<S>,
    constants: ConsensusConstants,
    state: Mutex<ServeState>,
}

impl<S> WeightProofServer<S>
where
    S: BlockStore + Send + Sync + ?Sized,
{
    #[must_use]
    pub fn new(store: Arc<S>, constants: ConsensusConstants) -> Self {
        WeightProofServer {
            store,
            constants,
            state: Mutex::new(ServeState {
                proof: None,
                ses_blocks: Vec::new(),
                walked_to: None,
                segments: VecDeque::new(),
            }),
        }
    }

    pub async fn get_proof_of_weight(&self, tip: Bytes32) -> Result<Arc<WeightProof>, ServeError> {
        // `try_block_record(tip)` unknown → refuse.
        let tip_rec = self
            .store
            .get_block_record(&tip)
            .await?
            .ok_or(ServeError::UnknownTip(tip))?;
        // Tip below WEIGHT_PROOF_RECENT_BLOCKS → refuse.
        if tip_rec.height < self.constants.weight_proof_recent_blocks {
            return Err(ServeError::ChainTooShort {
                height: tip_rec.height,
                required: self.constants.weight_proof_recent_blocks,
            });
        }
        let mut st = self.state.lock().await;
        if let Some((cached_tip, wp)) = &st.proof
            && *cached_tip == tip
        {
            return Ok(wp.clone());
        }
        let wp = Arc::new(self.create_proof_of_weight(&mut st, &tip_rec).await?);
        st.proof = Some((tip, wp.clone()));
        Ok(wp)
    }

    /// Recent chain,
    /// sub-epoch data, seed-derived sampling, and per-sampled-sub-epoch segment construction.
    async fn create_proof_of_weight(
        &self,
        st: &mut ServeState,
        tip_rec: &BlockRecord,
    ) -> Result<WeightProof, ServeError> {
        info!(
            "create weight proof tip={} height={}",
            tip_rec.header_hash, tip_rec.height
        );
        // The ses index must reach the tip before anything else.
        self.extend_ses_index(st, tip_rec.height).await?;
        let ses_blocks = st.ses_blocks.clone();

        let recent_chain = self.get_recent_chain(&ses_blocks, tip_rec.height).await?;

        // Needs at least two summaries.
        if ses_blocks.len() <= 1 {
            return Err(ServeError::NotEnoughSubEpochs);
        }

        // The genesis record opens the first sub-epoch's weight band.
        let mut prev_ses_block = self
            .store
            .get_block_record_by_height(0)
            .await?
            .ok_or(ServeError::MissingBlock(0))?;

        let mut sub_epochs: Vec<SubEpochData> = Vec::new();
        for ses_block in &ses_blocks {
            if ses_block.height > tip_rec.height {
                break;
            }
            let ses = ses_block
                .sub_epoch_summary_included
                .as_ref()
                .ok_or_else(|| ServeError::Build("ses block without summary".into()))?;
            sub_epochs.push(create_sub_epoch_data(ses));
        }

        // Seed from the SECOND-TO-LAST summary at-or-below the tip,
        // then the sampling draws in the reference's exact rng call order.
        let seed = get_seed_for_proof(&ses_blocks, tip_rec.height)?;
        let mut rng = crate::py_random::PyRandom::new(seed.as_ref());
        let weight_to_check =
            crate::get_weights_for_sampling(&mut rng, tip_rec.weight, &recent_chain)?;

        // Sample each sub-epoch's weight band; build (or reuse) the
        // challenge segments for every sampled one, capped at MAX_SAMPLES.
        let mut sample_n = 0usize;
        let mut sub_epoch_segments: Vec<SubEpochChallengeSegment> = Vec::new();
        for (sub_epoch_n, ses_block) in ses_blocks.iter().enumerate() {
            if ses_block.height > tip_rec.height {
                break;
            }
            if sample_n >= crate::MAX_SAMPLES {
                debug!("reached sampled sub epoch cap");
                break;
            }
            if ses_block.sub_epoch_summary_included.is_none() {
                return Err(ServeError::Build("ses block without summary".into()));
            }
            if crate::sample_sub_epoch(
                prev_ses_block.weight,
                ses_block.weight,
                weight_to_check.as_deref(),
            ) {
                sample_n += 1;
                let segs = match st.cached_segments(&ses_block.header_hash) {
                    Some(segs) => segs,
                    None => {
                        // The persisted
                        // store is checked BEFORE building; only a miss pays the sub-epoch block
                        // walk, and what it builds is persisted for every later build (and every
                        // later restart — segments below a served tip never change). The LRU
                        // above is the hot layer; the store is the durable one.
                        let got = match self
                            .store
                            .get_sub_epoch_segments(&ses_block.header_hash)
                            .await?
                        {
                            Some(bytes) => Arc::new(
                                SubEpochSegments::from_bytes(
                                    &mut Cursor::new(&bytes[..]),
                                    SEGMENT_STORE_VERSION,
                                )?
                                .challenge_segments,
                            ),
                            None => {
                                let sub_epoch_n = u32::try_from(sub_epoch_n).map_err(|_| {
                                    ServeError::Build("sub_epoch_n overflow".into())
                                })?;
                                let built = SubEpochSegments {
                                    challenge_segments: self
                                        .create_sub_epoch_segments(
                                            ses_block,
                                            &prev_ses_block,
                                            sub_epoch_n,
                                        )
                                        .await?,
                                };
                                // Persist the SubEpochSegments wrapper's bytes under the ses
                                // block hash.
                                self.store
                                    .persist_sub_epoch_segments(
                                        &ses_block.header_hash,
                                        &built.to_bytes(SEGMENT_STORE_VERSION)?,
                                    )
                                    .await?;
                                Arc::new(built.challenge_segments)
                            }
                        };
                        st.cache_segments(ses_block.header_hash, got.clone());
                        got
                    }
                };
                sub_epoch_segments.extend(segs.iter().cloned());
            }
            prev_ses_block = ses_block.clone();
        }

        debug!("sub_epochs sub_epochs={}", sub_epochs.len());
        Ok(WeightProof {
            sub_epochs,
            sub_epoch_segments,
            recent_chain_data: recent_chain,
        })
    }

    // Extend the derived ses index up to `upto` (inclusive). The main chain below an already-walked
    // height is immutable for our purposes (ses blocks sit at least a sub-epoch below any served tip,
    // far deeper than tip-local reorgs), so the walk never re-visits.
    async fn extend_ses_index(&self, st: &mut ServeState, upto: u32) -> Result<(), ServeError> {
        let start = match st.walked_to {
            Some(w) if w >= upto => return Ok(()),
            Some(w) => w.saturating_add(1),
            None => 0,
        };
        for h in start..=upto {
            let rec = self
                .store
                .get_block_record_by_height(h)
                .await?
                .ok_or(ServeError::MissingBlock(h))?;
            if rec.sub_epoch_summary_included.is_some() {
                st.ses_blocks.push(rec);
            }
        }
        st.walked_to = Some(upto);
        Ok(())
    }

    /// Headers from the
    /// block BEFORE the second-to-last sub-epoch summary at-or-below the tip, up to the tip.
    async fn get_recent_chain(
        &self,
        ses_blocks: &[BlockRecord],
        tip_height: u32,
    ) -> Result<Vec<HeaderBlock>, ServeError> {
        // Min_height = (second ses at-or-below tip) - 1.
        let mut min_height = 0u32;
        let mut count_ses = 0usize;
        for b in ses_blocks.iter().rev() {
            if b.height <= tip_height {
                count_ses += 1;
            }
            if count_ses == 2 {
                min_height = b.height.saturating_sub(1);
                break;
            }
        }
        debug!("recent chain span start={} end={}", min_height, tip_height);

        // Load the span's headers (tx_filter=False) and records. Every
        // height in the span must resolve (the reference asserts each height_to_hash).
        let span = usize::try_from(tip_height - min_height + 1)
            .map_err(|_| ServeError::Build("recent chain span overflow".into()))?;
        let mut headers: Vec<HeaderBlock> = Vec::with_capacity(span);
        let mut records: Vec<BlockRecord> = Vec::with_capacity(span);
        for h in min_height..=tip_height {
            let rec = self
                .store
                .get_block_record_by_height(h)
                .await?
                .ok_or(ServeError::MissingBlock(h))?;
            let block = self
                .store
                .get_block(&rec.header_hash)
                .await?
                .ok_or(ServeError::MissingBlock(h))?;
            headers.push(block.get_block_header());
            records.push(rec);
        }
        let at = |h: u32| usize::try_from(h - min_height).expect("span bounded above");

        // Walk down from the tip until two summaries are collected,
        // then prepend one more block (the block before the second summary).
        let mut recent_chain: VecDeque<HeaderBlock> = VecDeque::new();
        let mut ses_count = 0usize;
        let mut curr_height = tip_height;
        while ses_count < 2 {
            if curr_height == 0 {
                break;
            }
            recent_chain.push_front(headers[at(curr_height)].clone());
            if records[at(curr_height)]
                .sub_epoch_summary_included
                .is_some()
            {
                ses_count += 1;
            }
            curr_height -= 1;
        }
        recent_chain.push_front(headers[at(curr_height)].clone());

        info!(
            "recent chain start={} end={}",
            recent_chain
                .front()
                .map(HeaderBlock::height)
                .unwrap_or_default(),
            recent_chain
                .back()
                .map(HeaderBlock::height)
                .unwrap_or_default()
        );
        Ok(recent_chain.into())
    }

    /// The height
    /// two sub-slot starts below the sub-epoch start (the reference's 50-record batches are only its
    /// cache refill; point-gets are semantically identical).
    async fn get_prev_two_slots_height(&self, se_start: &BlockRecord) -> Result<u32, ServeError> {
        let mut slot = 0usize;
        let mut curr_rec = se_start.clone();
        while slot < 2 && curr_rec.height > 0 {
            if curr_rec.first_in_sub_slot() {
                slot += 1;
            }
            let h = curr_rec.height - 1;
            curr_rec = self
                .store
                .get_block_record_by_height(h)
                .await?
                .ok_or(ServeError::MissingBlock(h))?;
        }
        Ok(curr_rec.height)
    }

    // Load `[start, end]` as a ChainCache in one pass (records + tx_filter=False headers).
    // Heights past the peak simply don't resolve.
    async fn load_chain(&self, start: u32, end: u32) -> Result<ChainCache, ServeError> {
        let mut cache = ChainCache {
            height_to_hash: HashMap::new(),
            records: HashMap::new(),
            headers: HashMap::new(),
        };
        for h in start..=end {
            let Some(rec) = self.store.get_block_record_by_height(h).await? else {
                continue;
            };
            let Some(block) = self.store.get_block(&rec.header_hash).await? else {
                continue;
            };
            cache.height_to_hash.insert(h, rec.header_hash);
            cache
                .headers
                .insert(rec.header_hash, block.get_block_header());
            cache.records.insert(rec.header_hash, rec);
        }
        Ok(cache)
    }

    /// Scan the
    /// sub-epoch's span for challenge blocks; each yields one challenge segment.
    async fn create_sub_epoch_segments(
        &self,
        ses_block: &BlockRecord,
        se_start: &BlockRecord,
        sub_epoch_n: u32,
    ) -> Result<Vec<SubEpochChallengeSegment>, ServeError> {
        let start_height = self.get_prev_two_slots_height(se_start).await?;
        let end_height = ses_block
            .height
            .saturating_add(self.constants.max_sub_slot_blocks);
        let cache = self.load_chain(start_height, end_height).await?;

        let mut segments: Vec<SubEpochChallengeSegment> = Vec::new();
        let mut curr_hash = se_start.header_hash;
        let mut height = se_start.height;
        let mut first = true;
        loop {
            let curr_height = cache.header(&curr_hash)?.height();
            if curr_height >= ses_block.height {
                break;
            }
            if cache
                .record(&curr_hash)?
                .is_challenge_block(self.constants.min_blocks_per_challenge_block)
            {
                debug!(
                    "challenge segment segment={} height={}",
                    segments.len(),
                    curr_height
                );
                let (seg, end) =
                    self.create_challenge_segment(&cache, &curr_hash, sub_epoch_n, first)?;
                segments.push(seg);
                height = end;
                first = false;
            } else {
                height = height.saturating_add(1);
            }
            curr_hash = cache.hash_at(height)?;
        }
        debug!("sub epoch segments done next_sub_epoch_start={}", height);
        Ok(segments)
    }

    fn create_challenge_segment(
        &self,
        cache: &ChainCache,
        hh: &Bytes32,
        sub_epoch_n: u32,
        first_segment_in_sub_epoch: bool,
    ) -> Result<(SubEpochChallengeSegment, u32), ServeError> {
        let header_block = cache.header(hh)?;
        // VDFs from sub slots before the challenge block.
        let (mut sub_slots, first_rc_end_of_slot_vdf) =
            self.first_sub_slot_vdfs(cache, hh, first_segment_in_sub_epoch)?;
        // The challenge block's own VDFs.
        sub_slots.push(challenge_block_vdfs(&self.constants, cache, hh)?);
        // VDFs from the slot after the challenge block to end of slot.
        let (end_slots, end_height) =
            self.slot_end_vdf(cache, header_block.height().saturating_add(1))?;
        sub_slots.extend(end_slots);
        // Only a sub-epoch's first segment (past sub-epoch 0) carries
        // the first reward-chain end-of-slot VDF.
        let rc_slot_end_info = if first_segment_in_sub_epoch && sub_epoch_n != 0 {
            first_rc_end_of_slot_vdf
        } else {
            None
        };
        Ok((
            SubEpochChallengeSegment {
                sub_epoch_n,
                sub_slots,
                rc_slot_end_info,
            },
            end_height,
        ))
    }

    /// The challenge
    /// chain VDFs from the segment's slot start up to (not including) the challenge block.
    fn first_sub_slot_vdfs(
        &self,
        cache: &ChainCache,
        hh: &Bytes32,
        first_in_sub_epoch: bool,
    ) -> Result<(Vec<SubSlotData>, Option<VdfInfo>), ServeError> {
        let header_block = cache.header(hh)?;
        let header_block_sub_rec = cache.record(hh)?;

        // Find the slot start.
        let mut curr_sub_rec = header_block_sub_rec;
        let mut first_rc_end_of_slot_vdf = None;
        if first_in_sub_epoch && curr_sub_rec.height > 0 {
            while curr_sub_rec.sub_epoch_summary_included.is_none() {
                curr_sub_rec = cache.record(&curr_sub_rec.prev_hash)?;
            }
            first_rc_end_of_slot_vdf = Some(self.first_rc_end_of_slot_vdf(cache, hh)?);
        } else if header_block_sub_rec.overflow && header_block_sub_rec.first_in_sub_slot() {
            let mut sub_slots_num = 2i64;
            while sub_slots_num > 0 && curr_sub_rec.height > 0 {
                if curr_sub_rec.first_in_sub_slot() {
                    let finished = curr_sub_rec
                        .finished_challenge_slot_hashes
                        .as_ref()
                        .ok_or_else(|| {
                            ServeError::Build(
                                "first_in_sub_slot without challenge slot hashes".into(),
                            )
                        })?;
                    sub_slots_num -= i64::try_from(finished.len())
                        .map_err(|_| ServeError::Build("slot hash count overflow".into()))?;
                }
                curr_sub_rec = cache.record(&curr_sub_rec.prev_hash)?;
            }
        } else {
            while !curr_sub_rec.first_in_sub_slot() && curr_sub_rec.height > 0 {
                curr_sub_rec = cache.record(&curr_sub_rec.prev_hash)?;
            }
        }

        // Collect per-block ip VDFs + finished-slot VDFs up to the challenge block.
        let mut curr = cache.header(&curr_sub_rec.header_hash)?;
        let mut sub_slots_data: Vec<SubSlotData> = Vec::new();
        let mut tmp_sub_slots_data: Vec<SubSlotData> = Vec::new();
        while curr.height() < header_block.height() {
            if curr.first_in_sub_slot() {
                // If not blue boxed, keep the collected block VDFs.
                let first_slot = curr
                    .finished_sub_slots
                    .first()
                    .ok_or_else(|| ServeError::Build("first_in_sub_slot without slots".into()))?;
                if !blue_boxed_end_of_slot(first_slot) {
                    sub_slots_data.append(&mut tmp_sub_slots_data);
                }
                for sub_slot in &curr.finished_sub_slots {
                    let curr_icc_info = sub_slot
                        .infused_challenge_chain
                        .map(|icc| icc.infused_challenge_chain_end_of_slot_vdf);
                    sub_slots_data.push(handle_finished_slots(sub_slot, curr_icc_info));
                }
                tmp_sub_slots_data.clear();
            }
            // A bare ip-VDF entry per block.
            tmp_sub_slots_data.push(SubSlotData {
                proof_of_space: None,
                cc_signage_point: None,
                cc_infusion_point: None,
                icc_infusion_point: None,
                cc_sp_vdf_info: None,
                signage_point_index: Some(curr.reward_chain_block.signage_point_index),
                cc_slot_end: None,
                icc_slot_end: None,
                cc_slot_end_info: None,
                icc_slot_end_info: None,
                cc_ip_vdf_info: Some(curr.reward_chain_block.challenge_chain_ip_vdf),
                icc_ip_vdf_info: curr.reward_chain_block.infused_challenge_chain_ip_vdf,
                total_iters: Some(curr.total_iters()),
            });
            curr = cache.header_at(curr.height().saturating_add(1))?;
        }

        if !tmp_sub_slots_data.is_empty() {
            sub_slots_data.append(&mut tmp_sub_slots_data);
        }

        // The challenge block's own finished slots.
        for sub_slot in &header_block.finished_sub_slots {
            let curr_icc_info = sub_slot
                .infused_challenge_chain
                .map(|icc| icc.infused_challenge_chain_end_of_slot_vdf);
            sub_slots_data.push(handle_finished_slots(sub_slot, curr_icc_info));
        }
        Ok((sub_slots_data, first_rc_end_of_slot_vdf))
    }

    /// The
    /// reward-chain end-of-slot VDF of the sub-epoch's opening slot (found by walking back to the
    /// ses-carrying block).
    fn first_rc_end_of_slot_vdf(
        &self,
        cache: &ChainCache,
        hh: &Bytes32,
    ) -> Result<VdfInfo, ServeError> {
        let mut curr = cache.record(hh)?;
        while curr.height > 0 && curr.sub_epoch_summary_included.is_none() {
            curr = cache.record(&curr.prev_hash)?;
        }
        let header = cache.header(&curr.header_hash)?;
        Ok(header
            .finished_sub_slots
            .last()
            .ok_or_else(|| ServeError::Build("ses block without finished sub slots".into()))?
            .reward_chain
            .end_of_slot_vdf)
    }

    /// All VDFs from the
    /// first sub slot after the challenge block through the last sub slot before the next challenge
    /// block. Returns the collected entries and the next challenge block's height.
    fn slot_end_vdf(
        &self,
        cache: &ChainCache,
        start_height: u32,
    ) -> Result<(Vec<SubSlotData>, u32), ServeError> {
        debug!("slot end vdf start_height={}", start_height);
        let mut curr = cache.header_at(start_height)?;
        let mut curr_header_hash = cache.hash_at(start_height)?;
        let mut sub_slots_data: Vec<SubSlotData> = Vec::new();
        let mut tmp_sub_slots_data: Vec<SubSlotData> = Vec::new();
        while !cache
            .record(&curr_header_hash)?
            .is_challenge_block(self.constants.min_blocks_per_challenge_block)
        {
            if curr.first_in_sub_slot() {
                sub_slots_data.append(&mut tmp_sub_slots_data);
                // Collected end-of-slot VDFs.
                let curr_prev_header_hash = curr.prev_header_hash();
                for (idx, sub_slot) in curr.finished_sub_slots.iter().enumerate() {
                    let prev_rec = cache.record(&curr_prev_header_hash)?;
                    let eos_vdf_iters = if idx == 0 {
                        prev_rec
                            .sub_slot_iters
                            .checked_sub(prev_rec.ip_iters(&self.constants)?)
                            .ok_or_else(|| ServeError::Build("eos_vdf_iters underflow".into()))?
                    } else {
                        prev_rec.sub_slot_iters
                    };
                    sub_slots_data.push(handle_end_of_slot(sub_slot, eos_vdf_iters)?);
                }
                tmp_sub_slots_data.clear();
            }
            tmp_sub_slots_data.push(handle_block_vdfs(&self.constants, cache, curr)?);
            let next_height = curr.height().saturating_add(1);
            curr = cache.header_at(next_height)?;
            curr_header_hash = cache.hash_at(next_height)?;
        }

        if !tmp_sub_slots_data.is_empty() {
            sub_slots_data.append(&mut tmp_sub_slots_data);
        }
        debug!(
            "slot end vdf done end_height={} slots={}",
            curr.height(),
            sub_slots_data.len()
        );
        Ok((sub_slots_data, curr.height()))
    }
}

/// One non-challenge
/// block's signage/infusion-point VDFs, with the cc-sp iteration count recomputed from
/// `get_signage_point_vdf_info` for non-normalized proofs.
fn handle_block_vdfs(
    constants: &ConsensusConstants,
    cache: &ChainCache,
    curr: &HeaderBlock,
) -> Result<SubSlotData, ServeError> {
    let block_record = cache.record(&curr.header_hash()?)?;

    let mut icc_ip_proof = None;
    let mut icc_ip_info = None;
    if curr.infused_challenge_chain_ip_proof.is_some() {
        let info = curr
            .reward_chain_block
            .infused_challenge_chain_ip_vdf
            .ok_or_else(|| ServeError::Build("icc ip proof without icc ip vdf".into()))?;
        icc_ip_proof = curr.infused_challenge_chain_ip_proof.clone();
        icc_ip_info = Some(info);
    }

    let mut cc_sp_proof = None;
    let mut cc_sp_info = None;
    if let Some(sp_proof) = &curr.challenge_chain_sp_proof {
        let sp_vdf = curr
            .reward_chain_block
            .challenge_chain_sp_vdf
            .ok_or_else(|| ServeError::Build("cc sp proof without cc sp vdf".into()))?;
        let mut cc_sp_vdf_info = sp_vdf;
        if !sp_proof.normalized_to_identity {
            let prev_b = if curr.height() == 0 {
                None
            } else {
                Some(cache.record(&curr.prev_header_hash())?)
            };
            let (_, _, _, _, cc_vdf_iters, _) = get_signage_point_vdf_info(
                constants,
                &curr.finished_sub_slots,
                block_record.overflow,
                prev_b,
                &cache.records,
                block_record.sp_total_iters(constants)?,
                block_record.sp_iters(constants)?,
            )?;
            cc_sp_vdf_info = VdfInfo {
                challenge: sp_vdf.challenge,
                number_of_iterations: cc_vdf_iters,
                output: sp_vdf.output,
            };
        }
        cc_sp_proof = Some(sp_proof.clone());
        cc_sp_info = Some(cc_sp_vdf_info);
    }

    Ok(SubSlotData {
        proof_of_space: None,
        cc_signage_point: cc_sp_proof,
        cc_infusion_point: Some(curr.challenge_chain_ip_proof.clone()),
        icc_infusion_point: icc_ip_proof,
        cc_sp_vdf_info: cc_sp_info,
        signage_point_index: Some(curr.reward_chain_block.signage_point_index),
        cc_slot_end: None,
        icc_slot_end: None,
        cc_slot_end_info: None,
        icc_slot_end_info: None,
        cc_ip_vdf_info: Some(curr.reward_chain_block.challenge_chain_ip_vdf),
        icc_ip_vdf_info: icc_ip_info,
        total_iters: Some(curr.total_iters()),
    })
}

/// The challenge block's entry — proof of
/// space plus its signage/infusion-point VDFs.
fn challenge_block_vdfs(
    constants: &ConsensusConstants,
    cache: &ChainCache,
    hh: &Bytes32,
) -> Result<SubSlotData, ServeError> {
    let header_block = cache.header(hh)?;
    let block_rec = cache.record(hh)?;
    let prev_b = if header_block.height() == 0 {
        None
    } else {
        Some(cache.record(&header_block.prev_header_hash())?)
    };
    // Always recomputed, used only for the non-normalized cc-sp info.
    let (_, _, _, _, cc_vdf_iters, _) = get_signage_point_vdf_info(
        constants,
        &header_block.finished_sub_slots,
        block_rec.overflow,
        prev_b,
        &cache.records,
        block_rec.sp_total_iters(constants)?,
        block_rec.sp_iters(constants)?,
    )?;

    let mut cc_sp_info = None;
    if let Some(sp_vdf) = &header_block.reward_chain_block.challenge_chain_sp_vdf {
        cc_sp_info = Some(*sp_vdf);
        let sp_proof = header_block
            .challenge_chain_sp_proof
            .as_ref()
            .ok_or_else(|| ServeError::Build("cc sp vdf without cc sp proof".into()))?;
        if !sp_proof.normalized_to_identity {
            cc_sp_info = Some(VdfInfo {
                challenge: sp_vdf.challenge,
                number_of_iterations: cc_vdf_iters,
                output: sp_vdf.output,
            });
        }
    }
    Ok(SubSlotData {
        proof_of_space: Some(header_block.reward_chain_block.proof_of_space.clone()),
        cc_signage_point: header_block.challenge_chain_sp_proof.clone(),
        cc_infusion_point: Some(header_block.challenge_chain_ip_proof.clone()),
        icc_infusion_point: None,
        cc_sp_vdf_info: cc_sp_info,
        signage_point_index: Some(header_block.reward_chain_block.signage_point_index),
        cc_slot_end: None,
        icc_slot_end: None,
        cc_slot_end_info: None,
        icc_slot_end_info: None,
        cc_ip_vdf_info: Some(header_block.reward_chain_block.challenge_chain_ip_vdf),
        icc_ip_vdf_info: header_block
            .reward_chain_block
            .infused_challenge_chain_ip_vdf,
        total_iters: Some(block_rec.total_iters),
    })
}

/// A finished sub slot as a slot-end entry.
fn handle_finished_slots(
    end_of_slot: &EndOfSubSlotBundle,
    icc_end_of_slot_info: Option<VdfInfo>,
) -> SubSlotData {
    SubSlotData {
        proof_of_space: None,
        cc_signage_point: None,
        cc_infusion_point: None,
        icc_infusion_point: None,
        cc_sp_vdf_info: None,
        signage_point_index: None,
        cc_slot_end: Some(end_of_slot.proofs.challenge_chain_slot_proof.clone()),
        icc_slot_end: end_of_slot
            .proofs
            .infused_challenge_chain_slot_proof
            .clone(),
        cc_slot_end_info: Some(end_of_slot.challenge_chain.challenge_chain_end_of_slot_vdf),
        icc_slot_end_info: icc_end_of_slot_info,
        cc_ip_vdf_info: None,
        icc_ip_vdf_info: None,
        total_iters: None,
    }
}

/// A collected end-of-slot entry with the
/// cc/icc infos rewritten to the true eos iteration count unless the proofs are normalized.
fn handle_end_of_slot(
    sub_slot: &EndOfSubSlotBundle,
    eos_vdf_iters: u64,
) -> Result<SubSlotData, ServeError> {
    // The reference asserts both the icc chain and its proof exist here.
    let icc = sub_slot
        .infused_challenge_chain
        .as_ref()
        .ok_or_else(|| ServeError::Build("end of slot without infused challenge chain".into()))?;
    let icc_proof = sub_slot
        .proofs
        .infused_challenge_chain_slot_proof
        .as_ref()
        .ok_or_else(|| ServeError::Build("end of slot without icc slot proof".into()))?;
    let icc_info = if icc_proof.normalized_to_identity {
        icc.infused_challenge_chain_end_of_slot_vdf
    } else {
        VdfInfo {
            challenge: icc.infused_challenge_chain_end_of_slot_vdf.challenge,
            number_of_iterations: eos_vdf_iters,
            output: icc.infused_challenge_chain_end_of_slot_vdf.output,
        }
    };
    let cc_info = if sub_slot
        .proofs
        .challenge_chain_slot_proof
        .normalized_to_identity
    {
        sub_slot.challenge_chain.challenge_chain_end_of_slot_vdf
    } else {
        VdfInfo {
            challenge: sub_slot
                .challenge_chain
                .challenge_chain_end_of_slot_vdf
                .challenge,
            number_of_iterations: eos_vdf_iters,
            output: sub_slot
                .challenge_chain
                .challenge_chain_end_of_slot_vdf
                .output,
        }
    };
    Ok(SubSlotData {
        proof_of_space: None,
        cc_signage_point: None,
        cc_infusion_point: None,
        icc_infusion_point: None,
        cc_sp_vdf_info: None,
        signage_point_index: None,
        cc_slot_end: Some(sub_slot.proofs.challenge_chain_slot_proof.clone()),
        icc_slot_end: Some(icc_proof.clone()),
        cc_slot_end_info: Some(cc_info),
        icc_slot_end_info: Some(icc_info),
        cc_ip_vdf_info: None,
        icc_ip_vdf_info: None,
        total_iters: None,
    })
}

/// Both slot proofs normalized.
fn blue_boxed_end_of_slot(sub_slot: &EndOfSubSlotBundle) -> bool {
    sub_slot
        .proofs
        .challenge_chain_slot_proof
        .normalized_to_identity
        && sub_slot
            .proofs
            .infused_challenge_chain_slot_proof
            .as_ref()
            .is_none_or(|p| p.normalized_to_identity)
}

fn create_sub_epoch_data(ses: &SubEpochSummary) -> SubEpochData {
    SubEpochData {
        reward_chain_hash: ses.reward_chain_hash,
        num_blocks_overflow: ses.num_blocks_overflow,
        new_sub_slot_iters: ses.new_sub_slot_iters,
        new_difficulty: ses.new_difficulty,
    }
}

/// The hash of the
/// SECOND-TO-LAST sub-epoch summary at-or-below the tip.
fn get_seed_for_proof(ses_blocks: &[BlockRecord], tip_height: u32) -> Result<Bytes32, ServeError> {
    let mut count = 0usize;
    for b in ses_blocks.iter().rev() {
        if b.height <= tip_height {
            count += 1;
        }
        if count == 2 {
            let ses = b
                .sub_epoch_summary_included
                .as_ref()
                .ok_or_else(|| ServeError::Build("ses block without summary".into()))?;
            return Ok(ses.hash()?);
        }
    }
    Err(ServeError::NotEnoughSubEpochs)
}

#[cfg(test)]
#[path = "../tests/unit/serve/tests.rs"]
mod tests;
