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
        let cc_input = prev.challenge_vdf_output;
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
            prev.challenge_vdf_output
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
#[path = "../tests/unit/chain/tests.rs"]
mod tests;
