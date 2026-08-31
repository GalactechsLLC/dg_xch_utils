use crate::error::SimError;
use crate::factory::{FarmedIters, build_genesis_full, build_genesis_unfinished, farm_genesis};
use crate::pos2::PlotSet;
use crate::timelord::prove_vdf;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::infused_challenge_chain_subslot::InfusedChallengeChainSubSlot;
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::proof_of_space::{
    ProofBytes, ProofOfSpace, calculate_pos_challenge, calculate_prefix_bits_v2, passes_plot_filter,
};
use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::subslot_bundle::SubSlotBundle;
use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::consensus::block_generator::conditions_from_spend_bundle;
use dg_xch_core::consensus::block_rewards::{calculate_base_farmer_reward, calculate_pool_reward};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::deficit::calculate_deficit;
use dg_xch_core::consensus::difficulty_adjustment::{
    can_finish_sub_and_full_epoch, get_next_sub_slot_iters_and_difficulty,
};
use dg_xch_core::consensus::make_sub_epoch_summary::make_sub_epoch_summary;
use dg_xch_core::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality_for_proof, calculate_sp_interval_iters,
    calculate_sp_iters, is_overflow_block,
};
use dg_xch_core::consensus::producer::RewardBlockClaim;
use dg_xch_core::consensus::producer::{
    BlockTransactions, calculate_infusion_point_total_iters, create_unfinished_block,
    unfinished_block_to_full_block,
};
use dg_xch_core::consensus::vdf_info_computation::get_signage_point_vdf_info;
use dg_xch_core::traits::SizedBytes;
use dg_xch_node::engine::{AddBlockOutcome, BlockDelta, Engine};
use dg_xch_node::mempool::Mempool;
use dg_xch_node::primitives::NativePrimitives;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::traits::{BlockStore, CoinStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// The framing for the first block of a NEW sub-slot: the end-of-sub-slot bundle that closes the
/// previous slot, and the challenge-chain / reward-chain / infused-challenge-chain slot hashes its
/// infusion VDFs start against. A within-slot successor carries none of this (`farm_next_inner`'s
/// `cross` is `None`).
struct CrossSlot {
    finished: Vec<SubSlotBundle>,
    cc_challenge: Bytes32,
    rc_challenge: Bytes32,
    icc_challenge: Bytes32,
    base: u128,
}

/// A running simulated chain: an engine, the plots it farms with, and the block records it has
/// confirmed so far. `farm_next` produces and confirms one more block, so a test can grow a chain
/// and then hand its blocks to a fresh node to sync.
pub struct ChainBuilder<S> {
    engine: Engine<S, NativePrimitives>,
    constants: ConsensusConstants,
    plots: PlotSet,
    difficulty: u64,
    sub_slot_iters: u64,
    farmer_reward_puzzle_hash: Bytes32,
    records: HashMap<Bytes32, BlockRecord>,
    tip: Option<Bytes32>,
    timestamp: u64,
    blocks: Vec<FullBlock>,
    branch: u64,
    last_delta: Option<BlockDelta>,
}

impl<S: BlockStore + CoinStore + Sync> ChainBuilder<S> {
    pub fn new(
        store: S,
        constants: ConsensusConstants,
        plots: PlotSet,
        farmer_reward_puzzle_hash: Bytes32,
    ) -> Self {
        let difficulty = constants.difficulty_starting;
        let sub_slot_iters = constants.sub_slot_iters_starting;
        let engine = Engine::new(store, NativePrimitives, constants);
        Self {
            engine,
            constants,
            plots,
            difficulty,
            sub_slot_iters,
            farmer_reward_puzzle_hash,
            records: HashMap::new(),
            tip: None,
            timestamp: 1_700_000_000,
            blocks: Vec::new(),
            branch: 0,
            last_delta: None,
        }
    }

    /// Every block confirmed so far, in height order — the corpus a fresh node syncs.
    #[must_use]
    pub fn blocks(&self) -> &[FullBlock] {
        &self.blocks
    }

    #[must_use]
    pub fn engine(&self) -> &Engine<S, NativePrimitives> {
        &self.engine
    }

    #[must_use]
    pub fn constants(&self) -> &ConsensusConstants {
        &self.constants
    }

    /// Retarget the farmer reward: subsequent blocks pay their rewards to `ph`, which is how a
    /// coin is farmed to a chosen address.
    pub fn set_reward_ph(&mut self, ph: Bytes32) {
        self.farmer_reward_puzzle_hash = ph;
    }

    fn tip_record(&self) -> Option<&BlockRecord> {
        self.tip.as_ref().and_then(|h| self.records.get(h))
    }

    /// Confirm a full block into the engine and remember its record.
    async fn confirm(&mut self, block: FullBlock) -> Result<AddBlockOutcome, SimError> {
        let header_hash = block
            .header_hash()
            .map_err(|e| SimError::Invariant(format!("header hash: {e}")))?;
        let (outcome, delta) = self
            .engine
            .add_block_with_delta(&block)
            .await
            .map_err(SimError::Consensus)?;
        self.last_delta = delta;
        // Read the record by its own header hash, not by height: during a competing branch build a
        // height lookup would return the incumbent chain's block, not the one just added.
        let record = self
            .engine
            .store()
            .get_block_record(&header_hash)
            .await
            .map_err(|e| SimError::Invariant(format!("store: {e}")))?
            .ok_or_else(|| SimError::Invariant(format!("no record for {header_hash}")))?;
        self.tip = Some(record.header_hash);
        self.records.insert(record.header_hash, record);
        self.blocks.push(block);
        Ok(outcome)
    }

    /// The farmer reward target, mixed with the branch seed so a reorged branch farms genuinely
    /// different blocks (different foliage, different header hashes) from the branch it replaces.
    fn farmer_ph(&self) -> Bytes32 {
        if self.branch == 0 {
            return self.farmer_reward_puzzle_hash;
        }
        let mut bytes = self.farmer_reward_puzzle_hash.bytes();
        let seed = self.branch.to_le_bytes();
        for (b, s) in bytes.iter_mut().zip(seed.iter().cycle()) {
            *b ^= s;
        }
        Bytes32::from(bytes)
    }

    /// The challenge of the sub-slot `prev` sits in: the last finished challenge slot hash of the
    /// slot's first block (the genesis challenge within the first slot, a fresh challenge after each
    /// crossing).
    fn sub_slot_challenge(&self, prev: &BlockRecord) -> Result<Bytes32, SimError> {
        let mut curr = prev;
        while !curr.first_in_sub_slot() {
            curr = self.records.get(&curr.prev_hash).ok_or_else(|| {
                SimError::Invariant("challenge walk missing ancestor".to_string())
            })?;
        }
        curr.finished_challenge_slot_hashes
            .as_ref()
            .and_then(|v| v.last())
            .copied()
            .ok_or_else(|| SimError::Invariant("no finished challenge slot".to_string()))
    }

    /// The sub-slot iterations and difficulty the block after `prev` runs at. The validator derives
    /// the same pair for that position, so an epoch turn has to carry forward on both sides.
    fn retarget(&self, prev: &BlockRecord, new_slot: bool) -> Result<(u64, u64), SimError> {
        get_next_sub_slot_iters_and_difficulty(&self.constants, new_slot, Some(prev), &self.records)
            .map_err(SimError::Io)
    }

    /// The record at `height` on the current tip's chain.
    fn record_at_height(&self, height: u32) -> Option<&BlockRecord> {
        let mut curr = self.tip_record()?;
        loop {
            if curr.height == height {
                return Some(curr);
            }
            if curr.height < height {
                return None;
            }
            curr = self.records.get(&curr.prev_hash)?;
        }
    }

    /// Reorg: fork at `fork_height` and farm `new_blocks` on a competing branch seeded by `seed`.
    /// The branch differs from the incumbent and, being longer, outweighs it, so the engine reorgs
    /// onto it.
    pub async fn reorg(
        &mut self,
        fork_height: u32,
        new_blocks: u32,
        seed: u64,
    ) -> Result<AddBlockOutcome, SimError> {
        let fork = self
            .record_at_height(fork_height)
            .ok_or_else(|| SimError::Invariant(format!("no block at fork height {fork_height}")))?
            .header_hash;
        self.tip = Some(fork);
        self.branch = seed.max(1);
        // The branch overtakes the incumbent on the block that first exceeds its weight, which the
        // engine reports as a reorg; later blocks extend the now-winning branch. Surface the reorg.
        let mut result = AddBlockOutcome::AlreadyHave;
        for _ in 0..new_blocks {
            let outcome = self.farm_next_inner(None, None, None).await?;
            if matches!(result, AddBlockOutcome::Reorg { .. }) {
                continue;
            }
            result = outcome;
        }
        Ok(result)
    }

    /// Farm and confirm the genesis block.
    pub async fn farm_genesis(&mut self) -> Result<AddBlockOutcome, SimError> {
        let farmed = farm_genesis(
            &self.constants,
            &self.plots,
            self.difficulty,
            self.sub_slot_iters,
        )?;
        let ub = build_genesis_unfinished(
            &self.constants,
            &farmed,
            self.farmer_reward_puzzle_hash,
            self.timestamp,
        )?;
        let full = build_genesis_full(&self.constants, &ub, &farmed)?;
        self.confirm(full).await
    }

    /// Farm and confirm the next block within the current sub-slot, as a non-transaction block.
    ///
    /// The successor keeps the sub-slot challenge, advances the signage point, chains the
    /// challenge-chain VDF from the previous block's output and starts the reward-chain VDF from the
    /// previous block's reward-infusion challenge.
    pub async fn farm_next(&mut self) -> Result<AddBlockOutcome, SimError> {
        self.farm_next_inner(Some(false), None, None).await
    }

    /// Farm and confirm the next block as a transaction block. It incorporates the reward coins of
    /// the previous transaction block and any non-transaction blocks since, and any mempool spends.
    pub async fn farm_next_tx(&mut self) -> Result<AddBlockOutcome, SimError> {
        self.farm_next_inner(Some(true), None, None).await
    }

    /// Farm and confirm the next transaction block, carrying a real spend bundle. The bundle is
    /// admitted to a fresh mempool at the current peak and assembled into a block generator through
    /// the production `create_block_generator` path. The spent coins leave the coin store and the
    /// created coins enter it.
    pub async fn farm_next_tx_with_bundle(
        &mut self,
        bundle: SpendBundle,
    ) -> Result<AddBlockOutcome, SimError> {
        let (_, peak_height) = self
            .engine
            .store()
            .get_peak()
            .await
            .map_err(|e| SimError::Invariant(format!("peak: {e:?}")))?
            .ok_or_else(|| SimError::Invariant("farm genesis before spending".to_string()))?;
        let next_height = peak_height + 1;
        let mut mempool = Mempool::new(&self.constants);
        mempool.set_peak(peak_height, self.timestamp);
        let conds = conditions_from_spend_bundle(&bundle, next_height, &self.constants)
            .map_err(|e| SimError::Invariant(format!("bundle conditions: {e:?}")))?;
        mempool
            .admit(self.engine.store(), bundle, conds)
            .await
            .map_err(|e| SimError::Invariant(format!("admit: {e:?}")))?;
        let tx = mempool
            .create_block_generator(&self.constants, next_height, Duration::from_secs(10))
            .ok_or_else(|| SimError::Invariant("mempool yielded no block generator".to_string()))?;
        self.farm_next_inner(Some(true), Some(tx), None).await
    }

    /// The `BlockDelta` of the most recently confirmed block (the coin additions/removals + peak
    /// change), taken out for a server to feed to the wallet-notifier's peak hook. `None` if the last
    /// block was already known or nothing has been farmed.
    pub fn take_last_delta(&mut self) -> Option<BlockDelta> {
        self.last_delta.take()
    }

    /// Farm the next block, sealing whatever a shared mempool holds when the block is a transaction
    /// block. With `guarantee_tx` the signage point is advanced until a transaction block lands;
    /// without it the block type is whatever the position yields and a pending spend waits in the
    /// mempool for the next transaction block.
    pub async fn farm_next_from_shared_mempool(
        &mut self,
        mempool: &Arc<Mutex<Mempool>>,
        guarantee_tx: bool,
    ) -> Result<AddBlockOutcome, SimError> {
        let (_, peak_height) = self
            .engine
            .store()
            .get_peak()
            .await
            .map_err(|e| SimError::Invariant(format!("peak: {e:?}")))?
            .ok_or_else(|| SimError::Invariant("farm genesis before serving".to_string()))?;
        let next_height = peak_height + 1;
        let tx = {
            let mut mp = mempool.lock().await;
            mp.set_peak(peak_height, self.timestamp);
            mp.create_block_generator(&self.constants, next_height, Duration::from_secs(10))
        };
        // A transaction only seals into a transaction block; on a non-transaction block it stays in
        // the mempool (create_block_generator only reads it) for the next one. `guarantee_tx` filters
        // to a transaction position; otherwise the block type is left to the signage point.
        let prefer = guarantee_tx.then_some(true);
        let result = self.farm_next_inner(prefer, tx, None).await;
        match result {
            Err(SimError::SubSlotExhausted) => self.farm_next_slot().await,
            other => other,
        }
    }

    /// Close the tip's sub-slot and farm the first block of the next one that yields an eligible
    /// signage point. This is what lets a chain grow past the ~61 signage points of a single
    /// sub-slot: the end-of-sub-slot bundle finishes the challenge/reward/infused-challenge VDFs,
    /// and the new block infuses against the fresh challenge. When difficulty has outgrown the
    /// plot set enough that a whole sub-slot yields no eligible proof, further sub-slots are
    /// closed empty and carried in the same block's finished list — the chain's own shape for a
    /// proof drought — up to a bound.
    pub async fn farm_next_slot(&mut self) -> Result<AddBlockOutcome, SimError> {
        const MAX_EMPTY_SUB_SLOTS: usize = 32;
        let prev = self
            .tip_record()
            .cloned()
            .ok_or_else(|| SimError::Invariant("farm genesis before crossing".to_string()))?;
        let (bundle, mut cc_challenge, mut rc_challenge, mut icc_challenge) =
            self.build_end_of_sub_slot(&prev)?;
        let prev_ip = u128::from(prev.ip_iters(&self.constants).map_err(SimError::Io)?);
        let mut base = (prev.total_iters - prev_ip) + u128::from(prev.sub_slot_iters);
        let mut finished = vec![bundle];
        loop {
            let cross = CrossSlot {
                finished: finished.clone(),
                cc_challenge,
                rc_challenge,
                icc_challenge,
                base,
            };
            match self.farm_next_inner(None, None, Some(cross)).await {
                Err(SimError::SubSlotExhausted) if finished.len() < MAX_EMPTY_SUB_SLOTS => {
                    let (ssi, _) = self.retarget(&prev, true)?;
                    let last = finished.last().expect("the closing bundle");
                    let (bundle, cc, rc, icc) = self.build_empty_sub_slot(
                        last,
                        cc_challenge,
                        rc_challenge,
                        icc_challenge,
                        ssi,
                    )?;
                    base += u128::from(ssi);
                    finished.push(bundle);
                    cc_challenge = cc;
                    rc_challenge = rc;
                    icc_challenge = icc;
                }
                other => return other,
            }
        }
    }

    /// Close an empty sub-slot on top of `prev_bundle`: every chain runs from the identity over
    /// the whole slot, challenged by the previous bundle's hashes, and the deficit carries
    /// unchanged — no block, no reset, no summary, no epoch values.
    fn build_empty_sub_slot(
        &self,
        prev_bundle: &SubSlotBundle,
        cc_challenge: Bytes32,
        rc_challenge: Bytes32,
        icc_challenge: Bytes32,
        ssi: u64,
    ) -> Result<(SubSlotBundle, Bytes32, Bytes32, Bytes32), SimError> {
        let c = &self.constants;
        let disc = c.discriminant_size_bits;
        let identity = ClassgroupElement::get_default_element();
        let deficit = prev_bundle.reward_chain.deficit;
        let (cc_eos_vdf, cc_proof) = prove_vdf(cc_challenge, &identity, ssi, disc)?;
        // The infused chain keeps running through an empty slot only while a challenge period is
        // under way (deficit below the reset value).
        let (icc, icc_hash, icc_proof) = if deficit < c.min_blocks_per_challenge_block {
            let (icc_eos_vdf, icc_proof) = prove_vdf(icc_challenge, &identity, ssi, disc)?;
            let icc = InfusedChallengeChainSubSlot {
                infused_challenge_chain_end_of_slot_vdf: icc_eos_vdf,
            };
            let hash = icc.hash().map_err(SimError::Io)?;
            (Some(icc), Some(hash), Some(icc_proof))
        } else {
            (None, None, None)
        };
        let cc = ChallengeChainSubSlot {
            challenge_chain_end_of_slot_vdf: cc_eos_vdf,
            infused_challenge_chain_sub_slot_hash: None,
            subepoch_summary_hash: None,
            new_sub_slot_iters: None,
            new_difficulty: None,
        };
        let cc_hash = cc.hash().map_err(SimError::Io)?;
        let (rc_eos_vdf, rc_proof) = prove_vdf(rc_challenge, &identity, ssi, disc)?;
        let rc = RewardChainSubSlot {
            end_of_slot_vdf: rc_eos_vdf,
            challenge_chain_sub_slot_hash: cc_hash,
            infused_challenge_chain_sub_slot_hash: icc_hash,
            deficit,
        };
        let rc_hash = rc.hash().map_err(SimError::Io)?;
        let bundle = SubSlotBundle {
            challenge_chain: cc,
            infused_challenge_chain: icc,
            reward_chain: rc,
            proofs: SubSlotProofs {
                challenge_chain_slot_proof: cc_proof,
                infused_challenge_chain_slot_proof: icc_proof,
                reward_chain_slot_proof: rc_proof,
            },
        };
        Ok((bundle, cc_hash, rc_hash, icc_hash.unwrap_or(icc_challenge)))
    }

    /// Build the end-of-sub-slot bundle that closes `prev`'s sub-slot, with the new challenge-chain,
    /// reward-chain, and infused-challenge-chain slot hashes the next slot's first block infuses
    /// against. Mirrors the header validator's end-of-slot rules for a single closed slot.
    fn build_end_of_sub_slot(
        &self,
        prev: &BlockRecord,
    ) -> Result<(SubSlotBundle, Bytes32, Bytes32, Bytes32), SimError> {
        let c = &self.constants;
        let disc = c.discriminant_size_bits;
        let min = c.min_blocks_per_challenge_block;
        let ssi = prev.sub_slot_iters;
        let prev_ip = prev.ip_iters(c).map_err(SimError::Io)?;
        let eos_iters = ssi - prev_ip;
        let identity = ClassgroupElement::get_default_element();

        // The sub-slot challenge is the last finished challenge slot hash of the slot's first block.
        let mut first = prev;
        while !first.first_in_sub_slot() {
            first = self
                .records
                .get(&first.prev_hash)
                .ok_or_else(|| SimError::Invariant("cc slot walk missing ancestor".to_string()))?;
        }
        let cc_challenge = *first
            .finished_challenge_slot_hashes
            .as_ref()
            .and_then(|v| v.last())
            .ok_or_else(|| SimError::Invariant("no finished challenge slot".to_string()))?;

        // Challenge chain: one continuous VDF from the sub-slot challenge, continued from the tip's
        // output to the slot end; the stored iters are the whole sub-slot.
        let cc_input = ClassgroupElement::try_from(&prev.challenge_vdf_output)
            .map_err(|_| SimError::Invariant("bad prev challenge vdf output".to_string()))?;
        let (mut cc_eos_vdf, cc_proof) = prove_vdf(cc_challenge, &cc_input, eos_iters, disc)?;
        cc_eos_vdf.number_of_iterations = ssi;

        // Reward chain: from the identity over the remaining iters, challenged by the tip's infusion.
        let (rc_eos_vdf, rc_proof) = prove_vdf(
            prev.reward_infusion_new_challenge,
            &identity,
            eos_iters,
            disc,
        )?;

        // Infused challenge chain: challenged by the most recent challenge block (or finished icc
        // slot), continued from the tip's infused output (or the identity at a challenge block). The
        // stored iters run from that commitment to the slot end; the proof runs from the tip.
        let mut w = prev;
        while !w.is_challenge_block(min) && !w.first_in_sub_slot() {
            w = self
                .records
                .get(&w.prev_hash)
                .ok_or_else(|| SimError::Invariant("icc slot walk missing ancestor".to_string()))?;
        }
        let (icc_challenge, icc_committed) = if w.is_challenge_block(min) {
            (
                w.challenge_block_info_hash,
                ssi - w.ip_iters(c).map_err(SimError::Io)?,
            )
        } else {
            let h = *w
                .finished_infused_challenge_slot_hashes
                .as_ref()
                .and_then(|v| v.last())
                .ok_or_else(|| SimError::Invariant("no finished icc slot".to_string()))?;
            (h, ssi)
        };
        let icc_input = if prev.is_challenge_block(min) {
            ClassgroupElement::get_default_element()
        } else {
            prev.infused_challenge_vdf_output
                .as_ref()
                .map(ClassgroupElement::try_from)
                .transpose()
                .map_err(|_| SimError::Invariant("bad prev icc output".to_string()))?
                .ok_or_else(|| SimError::Invariant("no prev icc output".to_string()))?
        };
        let (mut icc_eos_vdf, icc_proof) = prove_vdf(icc_challenge, &icc_input, eos_iters, disc)?;
        icc_eos_vdf.number_of_iterations = icc_committed;
        let icc = InfusedChallengeChainSubSlot {
            infused_challenge_chain_end_of_slot_vdf: icc_eos_vdf,
        };
        let icc_hash = icc.hash().map_err(SimError::Io)?;

        // The deficit resets to the maximum after a challenge block (deficit 0), else carries over.
        let deficit = if prev.deficit == 0 { min } else { prev.deficit };

        // The sub-epoch summary the next block includes, when this crossing is the one that finishes
        // the sub-epoch. Only the first closed slot may carry it, and it carries the new epoch values
        // exactly when the epoch turns too.
        let (can_finish_se, can_finish_epoch) = can_finish_sub_and_full_epoch(
            c,
            &self.records,
            prev.height,
            prev.prev_hash,
            prev.deficit,
            prev.sub_epoch_summary_included.is_some(),
        )
        .map_err(SimError::Io)?;
        let (new_ssi, new_difficulty) = self.retarget(prev, true)?;
        let ses_hash = if can_finish_se {
            let prev_prev = self.records.get(&prev.prev_hash).ok_or_else(|| {
                SimError::Invariant("sub-epoch summary walk missing ancestor".to_string())
            })?;
            let ses = make_sub_epoch_summary(
                c,
                &self.records,
                prev.height + 1,
                prev_prev,
                can_finish_epoch.then_some(new_difficulty),
                can_finish_epoch.then_some(new_ssi),
            )
            .map_err(SimError::Io)?;
            Some(ses.hash().map_err(SimError::Io)?)
        } else {
            None
        };

        let cc = ChallengeChainSubSlot {
            challenge_chain_end_of_slot_vdf: cc_eos_vdf,
            infused_challenge_chain_sub_slot_hash: if deficit == min {
                Some(icc_hash)
            } else {
                None
            },
            subepoch_summary_hash: ses_hash,
            new_sub_slot_iters: can_finish_epoch.then_some(new_ssi),
            new_difficulty: can_finish_epoch.then_some(new_difficulty),
        };
        let cc_hash = cc.hash().map_err(SimError::Io)?;
        let rc = RewardChainSubSlot {
            end_of_slot_vdf: rc_eos_vdf,
            challenge_chain_sub_slot_hash: cc_hash,
            infused_challenge_chain_sub_slot_hash: Some(icc_hash),
            deficit,
        };
        let rc_hash = rc.hash().map_err(SimError::Io)?;
        let bundle = SubSlotBundle {
            challenge_chain: cc,
            infused_challenge_chain: Some(icc),
            reward_chain: rc,
            proofs: SubSlotProofs {
                challenge_chain_slot_proof: cc_proof,
                infused_challenge_chain_slot_proof: Some(icc_proof),
                reward_chain_slot_proof: rc_proof,
            },
        };
        Ok((bundle, cc_hash, rc_hash, icc_hash))
    }

    async fn farm_next_inner(
        &mut self,
        prefer: Option<bool>,
        tx: Option<BlockTransactions>,
        cross: Option<CrossSlot>,
    ) -> Result<AddBlockOutcome, SimError> {
        let prev = self
            .tip_record()
            .cloned()
            .ok_or_else(|| SimError::Invariant("farm genesis before farm_next".to_string()))?;
        // An epoch turn changes the difficulty and the sub-slot iterations; the block runs at the
        // retargeted values or the engine derives a different record than the one farmed.
        let (ssi, difficulty) = self.retarget(&prev, cross.is_some())?;
        self.sub_slot_iters = ssi;
        self.difficulty = difficulty;
        let c = &self.constants;
        // A within-slot successor keeps the sub-slot challenge and advances from the tip's own
        // signage point; the first block of a new sub-slot takes the new challenge, restarts its
        // base at the slot boundary, and scans from signage point zero.
        let within_base = prev.total_iters - u128::from(prev.ip_iters(c).map_err(SimError::Io)?);
        let base = cross.as_ref().map_or(within_base, |x| x.base);
        let challenge_hash = match cross.as_ref() {
            Some(x) => x.cc_challenge,
            None => self.sub_slot_challenge(&prev)?,
        };
        let finished: &[SubSlotBundle] = cross.as_ref().map_or(&[], |x| x.finished.as_slice());
        // A new sub-slot scans from signage point one: point zero is the slot boundary itself, which
        // carries no signage-point VDF and would need the special-cased infusion path.
        let sp_start = if cross.is_some() {
            1
        } else {
            prev.signage_point_index + 1
        };
        let prefix_bits = calculate_prefix_bits_v2(c, 0);
        let sp_interval_iters = calculate_sp_interval_iters(c, ssi).map_err(SimError::Io)?;
        self.timestamp += 20;

        // Advance the signage point until a plot wins in range with a later infusion than the tip.
        for sp_index in sp_start..(c.num_sps_sub_slot as u8) {
            if is_overflow_block(c, sp_index).unwrap_or(true) {
                break;
            }
            let sp_iters = calculate_sp_iters(c, ssi, sp_index).map_err(SimError::Io)?;
            let sp_total_iters = base + u128::from(sp_iters);
            let (cc_sp_chal, rc_sp_chal, cc_sp_in, rc_sp_in, cc_sp_iters, rc_sp_iters) =
                get_signage_point_vdf_info(
                    c,
                    finished,
                    false,
                    Some(&prev),
                    &self.records,
                    sp_total_iters,
                    sp_iters,
                )
                .map_err(SimError::Io)?;
            let disc = c.discriminant_size_bits;
            let (mut cc_sp_vdf, cc_sp_proof) = prove_vdf(cc_sp_chal, &cc_sp_in, cc_sp_iters, disc)?;
            // The challenge-chain signage-point VDF stores the block's sp_iters, but the proof runs
            // over the actual squarings since the sub-slot start; these coincide within the first
            // signage-point run and diverge later.
            cc_sp_vdf.number_of_iterations = sp_iters;
            let (rc_sp_vdf, rc_sp_proof) = prove_vdf(rc_sp_chal, &rc_sp_in, rc_sp_iters, disc)?;
            let cc_sp_hash = cc_sp_vdf.output.hash().map_err(SimError::Io)?;
            let rc_sp_hash = rc_sp_vdf.output.hash().map_err(SimError::Io)?;

            let Some((plot_index, pos)) =
                self.farm_at(sp_index, challenge_hash, cc_sp_hash, prefix_bits)?
            else {
                continue;
            };
            let quality = dg_xch_pos::verify_and_get_quality_string(
                &pos,
                c,
                challenge_hash,
                cc_sp_hash,
                0,
            )
            .ok_or_else(|| SimError::Invariant("farmed proof failed to verify".to_string()))?;
            let required_iters = calculate_iterations_quality_for_proof(
                c,
                &pos,
                quality,
                self.difficulty,
                cc_sp_hash,
            );
            if required_iters == 0 || required_iters >= sp_interval_iters {
                continue;
            }
            let ip_iters =
                calculate_ip_iters(c, ssi, sp_index, required_iters).map_err(SimError::Io)?;
            let total_iters = base + u128::from(ip_iters);
            if total_iters <= prev.total_iters {
                continue;
            }
            // is_transaction_block is determined by position: the block is a transaction block iff
            // its signage-point total iters exceed the previous transaction block's total iters.
            let our_sp_total_iters = base + u128::from(sp_iters);
            let prev_tx_total = self.prev_transaction_block_total_iters(&prev).unwrap_or(0);
            let is_tx = our_sp_total_iters > prev_tx_total;
            // Honor a caller preference by advancing the signage point until a block of the
            // requested kind lands; a reorg (no preference) takes whichever kind the position gives.
            if prefer.is_some_and(|want| want != is_tx) {
                continue;
            }
            let ipt = calculate_infusion_point_total_iters(base, sp_iters, ip_iters, ssi);

            let plot_keys = self.plots.plots[plot_index].keys.clone();
            // A key-based pool proof must carry a pool target signed by the pool key. A solo farmer
            // targets its own reward puzzle hash (not the pre-farm hash, which is genesis-only).
            let pool_target = PoolTarget {
                puzzle_hash: self.farmer_reward_puzzle_hash,
                max_height: 0,
            };
            let pool_target_bytes = pool_target
                .to_bytes(ChiaProtocolVersion::default())
                .map_err(SimError::Io)?;
            let pool_signature = dg_xch_core::blockchain::sized_bytes::Bytes96::parse(
                &sign(&plot_keys.pool, &pool_target_bytes).to_bytes(),
            )
            .map_err(|e| SimError::Invariant(format!("pool sig: {e:?}")))?;
            let signer =
                move |msg: Bytes32, _pk: &Bytes48| plot_keys.sign(msg).expect("plot signing");
            let (reward_claims, prev_tx_hash) = if is_tx {
                (
                    self.reward_claims_for(&prev)?,
                    self.prev_transaction_block_hash(&prev),
                )
            } else {
                (Vec::new(), self.constants.genesis_challenge)
            };
            let ub = create_unfinished_block(
                c,
                ipt,
                sp_index,
                pos,
                challenge_hash,
                Some(cc_sp_vdf),
                Some(cc_sp_proof),
                Some(rc_sp_vdf),
                Some(rc_sp_proof),
                cc_sp_hash,
                rc_sp_hash,
                finished.to_vec(),
                prev.height + 1,
                is_tx,
                &reward_claims,
                // A spend generator only belongs in a transaction block; a non-transaction block
                // carries none and leaves the mempool untouched.
                if is_tx { tx.as_ref() } else { None },
                prev.header_hash,
                prev_tx_hash,
                pool_target,
                Some(pool_signature),
                self.farmer_ph(),
                self.timestamp,
                b"dg_xch_simulator/successor",
                signer,
            )
            .map_err(SimError::Producer)?;

            // Finish: the challenge-chain infusion continues from the previous block's output; the
            // reward-chain infusion starts fresh from the previous block's reward-infusion challenge.
            let iters = FarmedIters {
                required_iters,
                sp_iters,
                ip_iters,
                infusion_point_total_iters: ipt,
            };
            let full = self.finish_successor(
                &ub,
                &prev,
                &iters,
                total_iters,
                sp_index,
                is_tx,
                cross.as_ref(),
            )?;
            return self.confirm(full).await;
        }
        Err(SimError::SubSlotExhausted)
    }

    /// The first plot that passes the filter and answers this signage point, with its v2 proof.
    fn farm_at(
        &self,
        sp_index: u8,
        challenge_hash: Bytes32,
        cc_sp_hash: Bytes32,
        prefix_bits: i8,
    ) -> Result<Option<(usize, ProofOfSpace)>, SimError> {
        for (plot_index, plot) in self.plots.plots.iter().enumerate() {
            if !passes_plot_filter(prefix_bits, plot.plot_id, challenge_hash, cc_sp_hash) {
                continue;
            }
            let pos_challenge = calculate_pos_challenge(plot.plot_id, challenge_hash, cc_sp_hash);
            for (found, chain) in self
                .plots
                .qualities_for_challenge(pos_challenge)?
                .into_iter()
                .filter(|(i, _)| *i == plot_index)
            {
                let proof = self.plots.solve(found, &chain);
                if proof.is_empty() {
                    continue;
                }
                let pos = ProofOfSpace::v2(
                    pos_challenge,
                    Some(plot.keys.pool_public_key),
                    None,
                    plot.keys.plot_public_key,
                    u16::try_from(plot_index).unwrap_or(u16::MAX),
                    0,
                    self.plots.strength,
                    ProofBytes::from(proof),
                );
                let _ = sp_index;
                return Ok(Some((plot_index, pos)));
            }
        }
        Ok(None)
    }

    /// The header hash of the most recent transaction block at or below `from`. A transaction block
    /// links to the previous one; a chain of transaction blocks each links to its predecessor.
    fn prev_transaction_block_hash(&self, from: &BlockRecord) -> Bytes32 {
        let mut curr = from;
        loop {
            if curr.is_transaction_block() {
                return curr.header_hash;
            }
            match self.records.get(&curr.prev_hash) {
                Some(r) => curr = r,
                None => return self.constants.genesis_challenge,
            }
        }
    }

    /// The total iters of the most recent transaction block at or below `from`.
    fn prev_transaction_block_total_iters(&self, from: &BlockRecord) -> Option<u128> {
        if from.is_transaction_block() {
            return Some(from.total_iters);
        }
        let mut curr = self.records.get(&from.prev_hash)?;
        loop {
            if curr.is_transaction_block() {
                return Some(curr.total_iters);
            }
            curr = self.records.get(&curr.prev_hash)?;
        }
    }

    /// The reward coins a transaction block after `prev` incorporates, matching the engine's
    /// `validate_reward_claims`: the previous transaction block's pool and farmer rewards, plus the
    /// non-transaction blocks that precede *it* (down to the transaction block before it). Rewards
    /// are thus claimed one transaction block late.
    fn reward_claims_for(&self, prev: &BlockRecord) -> Result<Vec<RewardBlockClaim>, SimError> {
        let missing = || SimError::Invariant("reward-claim walk missing ancestor".to_string());
        // The transaction block this one claims for.
        let mut cursor = prev;
        while !cursor.is_transaction_block() {
            cursor = self.records.get(&cursor.prev_hash).ok_or_else(missing)?;
        }
        let prev_tx = cursor;
        let mut claims = vec![RewardBlockClaim {
            height: prev_tx.height,
            pool_puzzle_hash: prev_tx.pool_puzzle_hash,
            farmer_puzzle_hash: prev_tx.farmer_puzzle_hash,
            fees: prev_tx.fees.unwrap_or(0),
        }];
        // The non-transaction blocks before the previous transaction block.
        if prev_tx.height > 0 {
            let mut curr = self.records.get(&prev_tx.prev_hash).ok_or_else(missing)?;
            while !curr.is_transaction_block() {
                claims.push(RewardBlockClaim {
                    height: curr.height,
                    pool_puzzle_hash: curr.pool_puzzle_hash,
                    farmer_puzzle_hash: curr.farmer_puzzle_hash,
                    fees: 0,
                });
                curr = self.records.get(&curr.prev_hash).ok_or_else(missing)?;
            }
        }
        let _ = (calculate_pool_reward, calculate_base_farmer_reward);
        Ok(claims)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_successor(
        &self,
        unfinished: &dg_xch_core::blockchain::unfinished_block::UnfinishedBlock,
        prev: &BlockRecord,
        iters: &FarmedIters,
        total_iters: u128,
        sp_index: u8,
        transaction: bool,
        cross: Option<&CrossSlot>,
    ) -> Result<FullBlock, SimError> {
        let c = &self.constants;
        let disc = c.discriminant_size_bits;
        let identity = ClassgroupElement::get_default_element();
        // A within-slot infusion continues the chains from the previous block over the squarings run
        // since it. The first block of a new sub-slot restarts every chain from the identity at the
        // slot boundary, over its own infusion offset, against the new slot's challenges.
        let ip_vdf_iters = if cross.is_some() {
            iters.ip_iters
        } else {
            u64::try_from(total_iters - prev.total_iters)
                .map_err(|_| SimError::Invariant("ip vdf iters overflow".to_string()))?
        };
        let cc_challenge = match cross {
            Some(x) => x.cc_challenge,
            None => self.sub_slot_challenge(prev)?,
        };
        let cc_input = if cross.is_some() {
            ClassgroupElement::get_default_element()
        } else {
            ClassgroupElement::try_from(&prev.challenge_vdf_output)
                .map_err(|_| SimError::Invariant("bad prev challenge vdf output".to_string()))?
        };
        let (mut cc_ip_vdf, cc_ip_proof) = prove_vdf(cc_challenge, &cc_input, ip_vdf_iters, disc)?;
        // The challenge-chain infusion VDF stores the block's ip_iters (offset from the sub-slot
        // start), but the proof is over the actual squarings run since the previous block. These
        // differ for a within-sub-slot successor; they coincide at genesis and at a slot boundary.
        cc_ip_vdf.number_of_iterations = iters.ip_iters;
        let rc_challenge = cross.map_or(prev.reward_infusion_new_challenge, |x| x.rc_challenge);
        let (rc_ip_vdf, rc_ip_proof) = prove_vdf(rc_challenge, &identity, ip_vdf_iters, disc)?;

        // The infused challenge chain runs once the sub-slot deficit drops below the challenge-block
        // threshold. Its challenge is the most recent challenge block (or finished icc slot), and it
        // starts from the identity at a challenge block or slot boundary, otherwise from the previous
        // icc output.
        let min = c.min_blocks_per_challenge_block;
        let overflow = is_overflow_block(c, sp_index).unwrap_or(false);
        let height = prev.height + 1;
        let num_finished = cross.map_or(0, |x| x.finished.len());
        let deficit = calculate_deficit(c, height, Some(prev), overflow, num_finished);
        let (icc_ip_vdf, icc_ip_proof) = if deficit < min - 1 {
            if let Some(x) = cross {
                let (v, p) = prove_vdf(x.icc_challenge, &identity, ip_vdf_iters, disc)?;
                (Some(v), Some(p))
            } else {
                let input = if prev.is_challenge_block(min) {
                    ClassgroupElement::get_default_element()
                } else {
                    prev.infused_challenge_vdf_output
                        .as_ref()
                        .map(ClassgroupElement::try_from)
                        .transpose()
                        .map_err(|_| SimError::Invariant("bad prev icc output".to_string()))?
                        .ok_or_else(|| SimError::Invariant("no prev icc output".to_string()))?
                };
                let mut curr = prev;
                while curr.finished_infused_challenge_slot_hashes.is_none()
                    && !curr.is_challenge_block(min)
                {
                    curr = self.records.get(&curr.prev_hash).ok_or_else(|| {
                        SimError::Invariant("icc walk missing ancestor".to_string())
                    })?;
                }
                let challenge = if curr.is_challenge_block(min) {
                    curr.challenge_block_info_hash
                } else {
                    *curr
                        .finished_infused_challenge_slot_hashes
                        .as_ref()
                        .and_then(|v| v.last())
                        .ok_or_else(|| SimError::Invariant("no finished icc slot".to_string()))?
                };
                let (v, p) = prove_vdf(challenge, &input, ip_vdf_iters, disc)?;
                (Some(v), Some(p))
            }
        } else {
            (None, None)
        };

        let finished_sub_slots = cross.map(|x| x.finished.clone()).unwrap_or_default();
        unfinished_block_to_full_block(
            unfinished,
            cc_ip_vdf,
            cc_ip_proof,
            rc_ip_vdf,
            rc_ip_proof,
            icc_ip_vdf,
            icc_ip_proof,
            finished_sub_slots,
            Some(prev),
            transaction,
            self.difficulty,
        )
        .map_err(SimError::Producer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::consensus::constants::SIMULATOR;
    use dg_xch_core::consensus::overrides::{ConsensusOverrides, apply_overrides};

    const K: u8 = 18;
    const STRENGTH: u8 = 2;

    fn constants() -> ConsensusConstants {
        apply_overrides(
            SIMULATOR,
            &ConsensusOverrides {
                plot_size_v2: Some(K),
                number_zero_bits_plot_filter_v2: Some(0),
                difficulty_constant_factor: Some(2u128.pow(25)),
                difficulty_starting: Some(7),
                discriminant_size_bits: Some(num_bigint::BigInt::from(16)),
                sub_slot_iters_starting: Some(65_536),
                ..Default::default()
            },
        )
    }

    async fn store() -> dg_xch_stores::SqliteStore {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("chain.sqlite");
        std::mem::forget(dir);
        dg_xch_stores::SqliteStore::open(&path)
            .await
            .expect("store")
    }

    #[tokio::test]
    async fn a_multi_block_chain_is_farmed_and_confirmed() {
        let dir = std::env::temp_dir().join("dgxch_sim_chain");
        let _ = std::fs::remove_dir_all(&dir);
        let plots = PlotSet::setup(&dir, 13, 12, K, STRENGTH, false).expect("plots");
        let mut chain =
            ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));

        assert!(matches!(
            chain.farm_genesis().await.expect("genesis"),
            AddBlockOutcome::NewPeak { height: 0 }
        ));
        for expected in 1..=3u32 {
            let outcome = chain.farm_next().await.expect("successor");
            // Genesis is NewPeak; a forward extension of the peak reports Extended.
            let height = match outcome {
                AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => {
                    height
                }
                other => panic!("block {expected} was not confirmed onto the peak: {other:?}"),
            };
            assert_eq!(height, expected, "block confirmed at the wrong height");
        }
        assert_eq!(chain.blocks().len(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_transaction_block_pays_and_claims_the_reward_coins() {
        use dg_xch_stores::traits::BlockStore;
        let dir = std::env::temp_dir().join("dgxch_sim_txblock");
        let _ = std::fs::remove_dir_all(&dir);
        let plots = PlotSet::setup(&dir, 15, 12, K, STRENGTH, false).expect("plots");
        let mut chain =
            ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
        chain.farm_genesis().await.expect("genesis");
        // A transaction block after genesis claims genesis's reward coins.
        let outcome = chain.farm_next_tx().await.expect("tx block");
        let height = match outcome {
            AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => height,
            other => panic!("tx block not confirmed: {other:?}"),
        };
        assert_eq!(height, 1);
        // The confirmed record is a transaction block (carries a timestamp).
        let record = chain
            .engine()
            .store()
            .get_block_record_by_height(1)
            .await
            .expect("store")
            .expect("record at 1");
        assert!(
            record.is_transaction_block(),
            "block 1 is not a transaction block"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_reorg_replaces_the_chain_with_a_heavier_branch() {
        use dg_xch_stores::traits::BlockStore;
        let dir = std::env::temp_dir().join("dgxch_sim_reorg");
        let _ = std::fs::remove_dir_all(&dir);
        let plots = PlotSet::setup(&dir, 17, 12, K, STRENGTH, false).expect("plots");
        let mut chain =
            ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
        chain.farm_genesis().await.expect("genesis");
        chain.farm_next().await.expect("b1");
        chain.farm_next().await.expect("b2");
        let (main_tip, main_h) = chain
            .engine()
            .store()
            .get_peak()
            .await
            .expect("peak")
            .expect("has peak");
        assert_eq!(main_h, 2);

        // Fork at height 1 and farm three blocks on a seeded branch, reaching height 4 — heavier
        // than the height-2 incumbent, so the engine reorgs onto it.
        let outcome = chain.reorg(1, 3, 42).await.expect("reorg");
        assert!(
            matches!(
                outcome,
                AddBlockOutcome::Reorg { .. } | AddBlockOutcome::NewPeak { .. }
            ),
            "the branch did not trigger a reorg: {outcome:?}"
        );
        let (new_tip, new_h) = chain
            .engine()
            .store()
            .get_peak()
            .await
            .expect("peak")
            .expect("has peak");
        assert_eq!(new_h, 4, "reorg did not reach the new height");
        assert_ne!(new_tip, main_tip, "the peak did not change");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_chain_crosses_into_a_new_sub_slot() {
        use dg_xch_stores::traits::BlockStore;
        let dir = std::env::temp_dir().join("dgxch_sim_subslot");
        let _ = std::fs::remove_dir_all(&dir);
        let plots = PlotSet::setup(&dir, 21, 12, K, STRENGTH, false).expect("plots");
        let mut chain =
            ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
        chain.farm_genesis().await.expect("genesis");
        chain.farm_next().await.expect("b1");
        // Close the sub-slot and open the next, several times over — a chain that outlives a single
        // sub-slot's signage points.
        let mut height = 1u32;
        for _ in 0..4 {
            let outcome = chain.farm_next_slot().await.expect("cross sub-slot");
            height = match outcome {
                AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => {
                    height
                }
                other => panic!("cross-slot block not confirmed: {other:?}"),
            };
            let record = chain
                .engine()
                .store()
                .get_block_record_by_height(height)
                .await
                .expect("store")
                .expect("cross-slot record");
            assert!(
                record.first_in_sub_slot(),
                "the cross-slot block is not first in its sub-slot"
            );
            // The chain keeps growing within the new sub-slot before the next crossing.
            chain.farm_next().await.expect("successor in the new slot");
            height += 1;
        }
        assert_eq!(
            height, 9,
            "expected four crossings each followed by a successor"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Difficulty can outgrow the plot set until whole sub-slots pass with no eligible signage
    // point. The chain's shape for that drought is empty finished sub-slots carried by the next
    // block; a farm that only ever closes one slot per attempt wedges on the second barren slot
    // (a long-running simulator did, once its difficulty had ratcheted for a day). Difficulty 65
    // with plot campaign 37 is a pinned fixture: genesis lands, and the drought hits within the
    // farmed span.
    #[tokio::test]
    async fn a_difficulty_drought_farms_through_empty_sub_slots() {
        let dir = std::env::temp_dir().join("dgxch_sim_drought");
        let _ = std::fs::remove_dir_all(&dir);
        let c = apply_overrides(
            constants(),
            &ConsensusOverrides {
                difficulty_starting: Some(65),
                ..Default::default()
            },
        );
        let plots = PlotSet::setup(&dir, 37, 12, K, STRENGTH, false).expect("plots");
        let mut chain = ChainBuilder::new(store().await, c, plots, Bytes32::from([0xAB; 32]));
        chain.farm_genesis().await.expect("genesis");
        let mut widest = 0usize;
        let mut after_drought = 0u32;
        for next in 1..=24u32 {
            let outcome = match chain.farm_next().await {
                Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
                other => other,
            }
            .unwrap_or_else(|e| panic!("block {next} wedged the farm: {e}"));
            let height = match outcome {
                AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => {
                    height
                }
                other => panic!("block {next} was not confirmed onto the peak: {other:?}"),
            };
            assert_eq!(height, next, "block confirmed at the wrong height");
            let crossed = chain
                .blocks()
                .last()
                .expect("confirmed block")
                .finished_sub_slots
                .len();
            widest = widest.max(crossed);
            // The point is made once a multi-slot block confirmed and the chain kept growing
            // past it; stop there rather than paying for the full span every run.
            if widest >= 2 {
                after_drought += 1;
                if after_drought >= 3 {
                    break;
                }
            }
        }
        assert!(
            widest >= 2,
            "no block carried an empty sub-slot (widest crossing {widest}); the fixture no \
             longer exercises the drought"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Sub-epoch boundaries at 16 and 32, with the epoch turn out at 64. min_blocks_per_challenge_block
    // is 16, so the deficit reaches zero at heights 15 and 31 — the two positions a sub-epoch can
    // finish once the next block starts a sub-slot.
    fn sub_epoch_constants() -> ConsensusConstants {
        apply_overrides(
            constants(),
            &ConsensusOverrides {
                sub_epoch_blocks: Some(16),
                epoch_blocks: Some(64),
                max_sub_slot_blocks: Some(8),
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn a_chain_farms_through_two_sub_epoch_boundaries() {
        use dg_xch_stores::traits::BlockStore;
        let dir = std::env::temp_dir().join("dgxch_sim_subepoch");
        let _ = std::fs::remove_dir_all(&dir);
        let c = sub_epoch_constants();
        let plots = PlotSet::setup(&dir, 23, 12, K, STRENGTH, false).expect("plots");
        let mut chain = ChainBuilder::new(store().await, c, plots, Bytes32::from([0xAB; 32]));
        chain.farm_genesis().await.expect("genesis");

        // Cross a sub-slot every fourth block, so a crossing lands on each height the deficit has
        // run down to zero — the summary positions.
        let target = c.sub_epoch_blocks * 2 + 4;
        for next in 1..=target {
            let outcome = if next.is_multiple_of(4) {
                chain.farm_next_slot().await
            } else {
                match chain.farm_next().await {
                    Err(SimError::SubSlotExhausted) => chain.farm_next_slot().await,
                    other => other,
                }
            }
            .unwrap_or_else(|e| panic!("block {next}: {e}"));
            let height = match outcome {
                AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => {
                    height
                }
                other => panic!("block {next} was not confirmed onto the peak: {other:?}"),
            };
            assert_eq!(height, next, "block confirmed at the wrong height");
        }

        let mut summaries = Vec::new();
        for height in 0..=target {
            let record = chain
                .engine()
                .store()
                .get_block_record_by_height(height)
                .await
                .expect("store")
                .expect("record");
            if record.sub_epoch_summary_included.is_some() {
                summaries.push(height);
            }
        }
        assert_eq!(
            summaries,
            vec![c.sub_epoch_blocks, c.sub_epoch_blocks * 2],
            "the chain did not include a summary at each sub-epoch boundary"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The epoch turn at height 64. sub_slot_time_target and slot_blocks_target are set against the
    // sim's own cadence — a block every 20 seconds, four to a sub-slot — so the retarget lands at a
    // workable multiple of the starting values instead of collapsing the sub-slot.
    fn epoch_constants() -> ConsensusConstants {
        apply_overrides(
            sub_epoch_constants(),
            &ConsensusOverrides {
                sub_slot_time_target: Some(num_bigint::BigInt::from(120)),
                slot_blocks_target: Some(4),
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn an_epoch_turn_retargets_the_difficulty_and_sub_slot_iters() {
        use dg_xch_stores::traits::BlockStore;
        let dir = std::env::temp_dir().join("dgxch_sim_epoch");
        let _ = std::fs::remove_dir_all(&dir);
        let c = epoch_constants();
        let plots = PlotSet::setup(&dir, 29, 12, K, STRENGTH, false).expect("plots");
        let mut chain = ChainBuilder::new(store().await, c, plots, Bytes32::from([0xAB; 32]));
        chain.farm_genesis().await.expect("genesis");

        // The epoch retarget reads timestamps and weights off transaction blocks, so the chain has
        // to carry them: every other block within a sub-slot is a transaction block.
        let target = c.epoch_blocks + 4;
        for next in 1..=target {
            let outcome = if next.is_multiple_of(4) {
                chain.farm_next_slot().await
            } else if next.is_multiple_of(2) {
                chain.farm_next_tx().await
            } else {
                chain.farm_next().await
            }
            .unwrap_or_else(|e| panic!("block {next}: {e}"));
            let height = match outcome {
                AddBlockOutcome::NewPeak { height } | AddBlockOutcome::Extended { height } => {
                    height
                }
                other => panic!("block {next} was not confirmed onto the peak: {other:?}"),
            };
            assert_eq!(height, next, "block confirmed at the wrong height");
        }

        // The summary at the epoch boundary carries the new epoch values, and the chain runs at them
        // afterwards.
        let turn = chain
            .engine()
            .store()
            .get_block_record_by_height(c.epoch_blocks)
            .await
            .expect("store")
            .expect("epoch record");
        let ses = turn
            .sub_epoch_summary_included
            .expect("the epoch boundary block includes a sub-epoch summary");
        assert_eq!(ses.new_sub_slot_iters, Some(turn.sub_slot_iters));
        assert_ne!(
            turn.sub_slot_iters, c.sub_slot_iters_starting,
            "the epoch turn did not retarget the sub-slot iterations"
        );
        let new_difficulty = ses
            .new_difficulty
            .expect("the epoch summary carries a new difficulty");
        assert_ne!(
            new_difficulty, c.difficulty_starting,
            "the epoch turn did not retarget the difficulty"
        );

        let tip = chain
            .engine()
            .store()
            .get_block_record_by_height(target)
            .await
            .expect("store")
            .expect("tip record");
        assert_eq!(
            tip.sub_slot_iters, turn.sub_slot_iters,
            "the new sub-slot iterations did not carry forward"
        );
        let prev = chain
            .engine()
            .store()
            .get_block_record_by_height(target - 1)
            .await
            .expect("store")
            .expect("record below the tip");
        assert_eq!(
            tip.weight - prev.weight,
            u128::from(new_difficulty),
            "the new difficulty did not carry forward"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_transaction_block_spends_a_reward_coin() {
        use dg_xch_core::blockchain::coin::Coin;
        use dg_xch_core::blockchain::coin_spend::CoinSpend;
        use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
        use dg_xch_core::blockchain::sized_bytes::Bytes96;
        use dg_xch_core::clvm::program::Program;
        use dg_xch_core::clvm::sexp::SExp;
        use dg_xch_core::consensus::coinbase::create_farmer_coin;
        use dg_xch_stores::traits::CoinStore;

        let dir = std::env::temp_dir().join("dgxch_sim_spend");
        let _ = std::fs::remove_dir_all(&dir);
        let plots = PlotSet::setup(&dir, 19, 12, K, STRENGTH, false).expect("plots");
        // Reward coins pay the `1` identity puzzle, so the farmer reward is spendable: run with the
        // solution as its condition list.
        let identity_ph = Program::to(1_u8).tree_hash();
        let mut chain = ChainBuilder::new(store().await, constants(), plots, identity_ph);
        chain.farm_genesis().await.expect("genesis");
        // A transaction block claims genesis's rewards, creating the farmer reward coin at height 1.
        chain.farm_next_tx().await.expect("tx block");

        // Genesis's farmer reward coin (its 1/8 of the pre-farm), created at height 1 by the claim.
        let genesis = chain.constants().genesis_challenge;
        let spendable =
            create_farmer_coin(0, identity_ph, calculate_base_farmer_reward(0), genesis);
        assert!(
            chain
                .engine()
                .store()
                .get_coin_record(&spendable.name())
                .await
                .expect("store")
                .is_some_and(|r| !r.spent),
            "the reward coin is not a spendable coin in the store"
        );

        // Spend the reward coin in full to a fresh puzzle hash.
        let out_ph = Bytes32::from([0x77; 32]);
        let puzzle = Program::to(1_u8);
        let solution = Program::to(vec![
            SExp::from(&ConditionWithArgs::CreateCoin(
                out_ph,
                spendable.amount,
                vec![],
            ))
            .to_owned(),
        ]);
        let mut infinity = [0u8; 96];
        infinity[0] = 0xc0;
        let bundle = SpendBundle {
            coin_spends: vec![CoinSpend {
                coin: spendable,
                puzzle_reveal: puzzle.serialized().expect("puzzle"),
                solution: solution.serialized().expect("solution"),
            }],
            aggregated_signature: Bytes96::from(infinity),
        };
        chain
            .farm_next_tx_with_bundle(bundle)
            .await
            .expect("spend block");

        // The reward coin is spent and the created coin is present and unspent.
        let spent = chain
            .engine()
            .store()
            .get_coin_record(&spendable.name())
            .await
            .expect("store")
            .expect("reward coin record");
        assert!(spent.spent, "the reward coin was not spent");
        let created = Coin {
            parent_coin_info: spendable.name(),
            puzzle_hash: out_ph,
            amount: spendable.amount,
        };
        let created = chain
            .engine()
            .store()
            .get_coin_record(&created.name())
            .await
            .expect("store")
            .expect("created coin record");
        assert!(!created.spent, "the created coin should be unspent");
        assert_eq!(created.coin.puzzle_hash, out_ph);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_fresh_node_syncs_the_farmed_chain() {
        use dg_xch_node::sync::BlockRangeSource;
        use dg_xch_node::{Chaser, SyncConfig, SyncError};
        use dg_xch_stores::traits::BlockStore;
        use std::collections::HashMap;
        use std::sync::Arc;

        // A block source over an in-memory height -> block map.
        struct FarmedSource {
            blocks: HashMap<u32, FullBlock>,
        }
        #[async_trait::async_trait]
        impl BlockRangeSource for FarmedSource {
            fn peer_id(&self) -> u64 {
                1
            }
            fn is_closed(&self) -> bool {
                false
            }
            async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
                Ok((start..=end)
                    .filter_map(|h| self.blocks.get(&h).cloned())
                    .collect())
            }
        }

        // Producer: farm a chain, capturing every block and the reward coins genesis created.
        let dir = std::env::temp_dir().join("dgxch_sim_sync");
        let _ = std::fs::remove_dir_all(&dir);
        let plots = PlotSet::setup(&dir, 21, 12, K, STRENGTH, false).expect("plots");
        let mut producer =
            ChainBuilder::new(store().await, constants(), plots, Bytes32::from([0xAB; 32]));
        producer.farm_genesis().await.expect("genesis");
        producer.farm_next().await.expect("successor");
        // Cross a sub-slot so the corpus carries a block with a finished sub-slot bundle: a fresh
        // node must validate the end-of-slot VDFs on sync, not just within-slot infusions.
        producer.farm_next_slot().await.expect("cross sub-slot");
        producer
            .farm_next()
            .await
            .expect("successor in the new slot");
        let corpus: HashMap<u32, FullBlock> = producer
            .blocks()
            .iter()
            .map(|b| (b.reward_chain_block.height, b.clone()))
            .collect();
        let last = *corpus.keys().max().expect("nonempty");

        // Consumer: a fresh node with an empty store follows the same blocks through the sync
        // pipeline, window by window.
        let consumer_store = Arc::new(store().await);
        let engine = Engine::new(consumer_store.clone(), NativePrimitives, constants());
        let mut chaser = Chaser::new(engine, SyncConfig::default());
        let source: Arc<dyn BlockRangeSource> = Arc::new(FarmedSource { blocks: corpus });
        let mut next = 0u32;
        while next <= last {
            let to = last.min(next + 31);
            chaser
                .follow_to(&source, next, to)
                .await
                .expect("sync window");
            next = to + 1;
        }

        // The synced node reached the producer's peak and holds the same block at every height.
        let producer_store = producer.engine().store();
        let producer_peak = producer_store.get_peak().await.expect("producer peak");
        let consumer_peak = consumer_store.get_peak().await.expect("consumer peak");
        assert_eq!(
            consumer_peak, producer_peak,
            "synced peak differs from the producer"
        );
        assert_eq!(
            consumer_peak.map(|(_, h)| h),
            Some(last),
            "did not sync to the tip"
        );
        for height in 0..=last {
            let p = producer_store
                .get_block_record_by_height(height)
                .await
                .expect("producer record");
            let cs = consumer_store
                .get_block_record_by_height(height)
                .await
                .expect("consumer record");
            assert_eq!(
                p.map(|r| r.header_hash),
                cs.map(|r| r.header_hash),
                "records differ at height {height}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
