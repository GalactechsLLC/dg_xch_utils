// The slot state machine: finished sub-slots and signage points from the peak's slot onward, so
// the node knows where the network is within a slot, not just at block boundaries. Everything a
// peer sends is validated against the current tip state before it enters a cache — the future
// caches hold only objects whose sole missing dependency is an infusion we have not seen yet,
// keyed by that infusion's reward-chain challenge.

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::difficulty_adjustment::can_finish_sub_and_full_epoch;
use dg_xch_core::consensus::make_sub_epoch_summary::make_sub_epoch_summary;
use dg_xch_core::consensus::pot_iterations::calculate_sp_interval_iters;
use dg_xch_vdf::validate_vdf_info;
use log::debug;
use std::collections::HashMap;

// Future-cache bounds. TTL eviction is omitted — the caps alone bound memory, and every cache is
// dropped wholesale when its key's infusion arrives or the slot list resets.
const FUTURE_CACHE_MAX_KEYS: usize = 128;
const FUTURE_EOS_MAX_PER_KEY: usize = 4;
const FUTURE_SP_MAX_PER_KEY: usize = 64;

// One finished (or genesis) sub-slot: the EOS bundle (`None` for the chain's first slot), the
// 64 signage-point slots filled as gossip arrives, and the total iters at the slot's START.
struct FinishedSubSlot {
    eos: Option<EndOfSubSlotBundle>,
    sps: Vec<Option<SignagePoint>>,
    start_total_iters: u128,
}

// A bounded per-key list cache: at most `max_keys` keys, each holding at most `max_per_key`
// entries; a full cache rejects rather than evicts.
struct KeyedCache<T> {
    map: HashMap<Bytes32, Vec<T>>,
    max_keys: usize,
    max_per_key: usize,
}

impl<T> KeyedCache<T> {
    fn new(max_keys: usize, max_per_key: usize) -> Self {
        Self {
            map: HashMap::new(),
            max_keys,
            max_per_key,
        }
    }

    fn append(&mut self, key: Bytes32, value: T) -> bool {
        if !self.map.contains_key(&key) && self.map.len() >= self.max_keys {
            return false;
        }
        let entries = self.map.entry(key).or_default();
        if entries.len() >= self.max_per_key {
            return false;
        }
        entries.push(value);
        true
    }

    fn take(&mut self, key: &Bytes32) -> Vec<T> {
        self.map.remove(key).unwrap_or_default()
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

// The peak's slot context: the EOS bundles ending the slots the peak's signage point and
// infusion sit in (`None` for the first slots / non-overflow), and the fork block on a reorg.
pub struct PeakSlotContext<'a> {
    pub sp_sub_slot: Option<&'a EndOfSubSlotBundle>,
    pub ip_sub_slot: Option<&'a EndOfSubSlotBundle>,
    pub fork_block: Option<&'a BlockRecord>,
}

pub struct SlotState {
    constants: ConsensusConstants,
    finished_sub_slots: Vec<FinishedSubSlot>,
    // End-of-sub-slots that chain onto an infusion we have not seen, keyed by that infusion's
    // reward-chain challenge (`peak.reward_infusion_new_challenge` once it exists).
    future_eos: KeyedCache<EndOfSubSlotBundle>,
    // Signage points in the same position, keyed by their rc challenge.
    future_sp: KeyedCache<(u8, SignagePoint)>,
}

impl SlotState {
    #[must_use]
    pub fn new(constants: ConsensusConstants) -> Self {
        let mut state = Self {
            constants,
            finished_sub_slots: Vec::new(),
            future_eos: KeyedCache::new(FUTURE_CACHE_MAX_KEYS, FUTURE_EOS_MAX_PER_KEY),
            future_sp: KeyedCache::new(FUTURE_CACHE_MAX_KEYS, FUTURE_SP_MAX_PER_KEY),
        };
        state.initialize_genesis_sub_slot();
        state
    }

    fn empty_sps(&self) -> Vec<Option<SignagePoint>> {
        vec![None; self.constants.num_sps_sub_slot as usize]
    }

    pub fn initialize_genesis_sub_slot(&mut self) {
        self.finished_sub_slots = vec![FinishedSubSlot {
            eos: None,
            sps: self.empty_sps(),
            start_total_iters: 0,
        }];
    }

    // The finished sub-slot whose challenge-chain hash is `challenge_hash`, with its index and
    // start iters.
    #[must_use]
    pub fn get_sub_slot(
        &self,
        challenge_hash: &Bytes32,
    ) -> Option<(&EndOfSubSlotBundle, usize, u128)> {
        for (index, slot) in self.finished_sub_slots.iter().enumerate() {
            if let Some(eos) = &slot.eos
                && eos.challenge_chain.hash().ok()? == *challenge_hash
            {
                return Some((eos, index, slot.start_total_iters));
            }
        }
        None
    }

    // The cached signage point whose cc-vdf output hashes to `cc_signage_point`. Returns an
    // owned value because the sub-slot-start point (all-`None`) is synthesized, not stored: when
    // `cc_signage_point` is the genesis challenge or a finished sub-slot's challenge-chain hash,
    // this is the index-0 SP. Otherwise it is the stored SP matched by `cc_vdf.output` hash (the
    // farmer/pool lookup key).
    #[must_use]
    pub fn get_signage_point(&self, cc_signage_point: &Bytes32) -> Option<SignagePoint> {
        if *cc_signage_point == self.constants.genesis_challenge {
            return Some(SignagePoint::sub_slot_start());
        }
        for slot in &self.finished_sub_slots {
            if let Some(eos) = &slot.eos
                && eos.challenge_chain.hash().ok()? == *cc_signage_point
            {
                return Some(SignagePoint::sub_slot_start());
            }
            for sp in slot.sps.iter().flatten() {
                if let Some(cc_vdf) = &sp.cc_vdf
                    && cc_vdf.output.hash().ok()? == *cc_signage_point
                {
                    return Some(sp.clone());
                }
            }
        }
        None
    }

    // The cc challenge of a slot: its EOS cc hash, or the genesis challenge for the first slot.
    fn slot_cc_hash(&self, slot: &FinishedSubSlot) -> Option<Bytes32> {
        match &slot.eos {
            Some(eos) => eos.challenge_chain.hash().ok(),
            None => Some(self.constants.genesis_challenge),
        }
    }

    // The signage point at `index` in the slot with cc hash `challenge_hash`, provided it was
    // built on `last_rc_infusion`. Index 0 is the slot start itself, not a stored SP — callers
    // handle it via `get_sub_slot`.
    #[must_use]
    pub fn get_signage_point_by_index(
        &self,
        challenge_hash: &Bytes32,
        index: u8,
        last_rc_infusion: &Bytes32,
    ) -> Option<&SignagePoint> {
        for slot in &self.finished_sub_slots {
            if self.slot_cc_hash(slot)? != *challenge_hash {
                continue;
            }
            return slot.sps.get(index as usize)?.as_ref().filter(|sp| {
                sp.rc_vdf
                    .as_ref()
                    .is_some_and(|rc| rc.challenge == *last_rc_infusion)
            });
        }
        None
    }

    // True if we hold a signage point at `index` built on a newer infusion than
    // `last_rc_infusion` — an earlier index in the same slot proves we saw that infusion, yet
    // the SP at `index` chains past it. The announce handler uses this to ignore outdated gossip.
    #[must_use]
    pub fn have_newer_signage_point(
        &self,
        challenge_hash: &Bytes32,
        index: u8,
        last_rc_infusion: &Bytes32,
    ) -> bool {
        for slot in &self.finished_sub_slots {
            if self.slot_cc_hash(slot) != Some(*challenge_hash) {
                continue;
            }
            let found_rc = slot.sps[..(index as usize).min(slot.sps.len())]
                .iter()
                .flatten()
                .any(|sp| {
                    sp.rc_vdf
                        .as_ref()
                        .is_some_and(|rc| rc.challenge == *last_rc_infusion)
                });
            return found_rc
                && slot
                    .sps
                    .get(index as usize)
                    .and_then(Option::as_ref)
                    .is_some_and(|sp| {
                        sp.rc_vdf
                            .as_ref()
                            .is_some_and(|rc| rc.challenge != *last_rc_infusion)
                    });
        }
        false
    }

    /// The finished sub-slots to include in a candidate being farmed. Collects the EOS bundles
    /// from the one whose challenge-chain end-of-slot VDF challenge equals `challenge_in_chain`
    /// (the last challenge already in the chain at the candidate's previous block) up to and
    /// including the one whose challenge-chain hash equals `last_challenge_to_add` (the
    /// candidate's `cc_challenge_hash`).
    ///
    /// `challenge_in_chain` is supplied by the caller because it is block-store-derived and
    /// `SlotState` holds no block store: `GENESIS_CHALLENGE` when there is no previous block,
    /// else the previous block's first-in-sub-slot ancestor's `finished_challenge_slot_hashes[-1]`.
    ///
    /// Returns `Some([])` when `last_challenge_to_add == challenge_in_chain` (nothing to add), or
    /// `None` when the last challenge is not found connected to `challenge_in_chain`. The genesis
    /// slot (`finished_sub_slots[0]`, whose `eos` is `None`) is skipped.
    #[must_use]
    pub fn get_finished_sub_slots(
        &self,
        challenge_in_chain: Bytes32,
        last_challenge_to_add: Bytes32,
    ) -> Option<Vec<EndOfSubSlotBundle>> {
        if last_challenge_to_add == challenge_in_chain {
            return Some(Vec::new());
        }
        let mut collected: Vec<EndOfSubSlotBundle> = Vec::new();
        let mut found_connecting = false;
        for slot in self.finished_sub_slots.iter().skip(1) {
            // slots[1..] always carry an eos; only the genesis slot [0] has `eos == None`.
            let eos = slot.eos.as_ref()?;
            if eos
                .challenge_chain
                .challenge_chain_end_of_slot_vdf
                .challenge
                == challenge_in_chain
            {
                found_connecting = true;
            }
            if found_connecting {
                collected.push(eos.clone());
                if eos.challenge_chain.hash().ok()? == last_challenge_to_add {
                    return Some(collected);
                }
            }
        }
        None
    }

    /// Backtrack a reward-chain challenge through the empty finished sub-slots we hold: while a
    /// stored EOS's reward-chain hashes to the current `rc_challenge`, step to that slot's
    /// reward-chain end-of-slot VDF challenge. Resolves the reward-chain challenge the candidate's
    /// previous block must carry before the block-store backtrack (which the server runs).
    #[must_use]
    pub fn backtrack_rc_challenge(&self, mut rc_challenge: Bytes32) -> Bytes32 {
        for slot in self.finished_sub_slots.iter().rev() {
            if let Some(eos) = &slot.eos
                && eos.reward_chain.hash().ok() == Some(rc_challenge)
            {
                rc_challenge = eos.reward_chain.end_of_slot_vdf.challenge;
            }
        }
        rc_challenge
    }

    /// Validate and append a gossiped end-of-sub-slot bundle. `Some(())` means appended; `None`
    /// means rejected or cached for a future infusion. Timelord infusion-point outputs are not
    /// modeled.
    ///
    /// `blocks` is the record ancestry the deficit/ICC walks read (the engine's walk cache).
    #[allow(clippy::too_many_lines)]
    pub fn new_finished_sub_slot(
        &mut self,
        eos: &EndOfSubSlotBundle,
        blocks: &HashMap<Bytes32, BlockRecord>,
        peak: Option<&BlockRecord>,
        next_sub_slot_iters: u64,
        next_difficulty: u64,
        skip_vdf_validation: bool,
    ) -> Option<()> {
        let last = self.finished_sub_slots.last()?;
        let cc_challenge = match &last.eos {
            Some(s) => s.challenge_chain.hash().ok()?,
            None => self.constants.genesis_challenge,
        };
        let mut rc_challenge = match &last.eos {
            Some(s) => s.reward_chain.hash().ok()?,
            None => self.constants.genesis_challenge,
        };
        let last_slot_iters = last.start_total_iters;
        let last_slot_eos = last.eos.clone();

        // Already present — idempotent accept.
        if self
            .finished_sub_slots
            .iter()
            .any(|s| s.eos.as_ref() == Some(eos))
        {
            return Some(());
        }

        if eos
            .challenge_chain
            .challenge_chain_end_of_slot_vdf
            .challenge
            != cc_challenge
        {
            // Does not append to our next slot — never cache it (a peer could otherwise grow
            // the cache with fabricated VDF chains).
            debug!("bad cc_challenge in new_finished_sub_slot");
            return None;
        }

        let sub_slot_iters =
            peak.map_or(self.constants.sub_slot_iters_starting, |p| p.sub_slot_iters);
        let total_iters = last_slot_iters + u128::from(sub_slot_iters);

        let mut icc_challenge: Option<Bytes32> = None;
        let mut icc_iters: Option<u64> = None;
        let cc_start_element;
        let icc_start_element;
        let iters;

        if let Some(peak) = peak.filter(|p| p.total_iters > last_slot_iters) {
            // The peak is inside this slot: the EOS proofs run from the peak's infusion.
            if total_iters < peak.total_iters {
                debug!("dont add slot, total_iters < peak.total_iters");
                return None;
            }
            rc_challenge = eos.reward_chain.end_of_slot_vdf.challenge;
            cc_start_element = peak.challenge_vdf_output;
            iters = u64::try_from(total_iters - peak.total_iters).ok()?;
            if peak.reward_infusion_new_challenge != rc_challenge {
                // Depends on an infusion we have not seen: cache under that challenge.
                self.future_eos.append(rc_challenge, eos.clone());
                debug!("dont have challenge hash, caching EOS");
                return None;
            }

            if peak.deficit == 0 {
                if eos.reward_chain.deficit != self.constants.min_blocks_per_challenge_block {
                    return None;
                }
            } else if eos.reward_chain.deficit != peak.deficit {
                return None;
            }

            icc_start_element = if peak.deficit == self.constants.min_blocks_per_challenge_block {
                None
            } else if peak.deficit == self.constants.min_blocks_per_challenge_block - 1 {
                Some(ClassgroupElement::get_default_element())
            } else {
                peak.infused_challenge_vdf_output
            };

            if peak.deficit < self.constants.min_blocks_per_challenge_block {
                let mut curr = peak;
                while !curr.first_in_sub_slot()
                    && !curr.is_challenge_block(self.constants.min_blocks_per_challenge_block)
                {
                    curr = blocks.get(&curr.prev_hash)?;
                }
                if curr.is_challenge_block(self.constants.min_blocks_per_challenge_block) {
                    icc_challenge = Some(curr.challenge_block_info_hash);
                    icc_iters = Some(u64::try_from(total_iters - curr.total_iters).ok()?);
                } else {
                    icc_challenge = Some(
                        *curr
                            .finished_infused_challenge_slot_hashes
                            .as_ref()?
                            .last()?,
                    );
                    icc_iters = Some(sub_slot_iters);
                }
            }

            let (finish_se, finish_epoch) = can_finish_sub_and_full_epoch(
                &self.constants,
                blocks,
                peak.height,
                peak.prev_hash,
                peak.deficit,
                peak.sub_epoch_summary_included.is_some(),
            )
            .ok()?;
            if finish_se {
                // First slot of a new sub-epoch: the EOS must carry the expected SES hash.
                let prev = blocks.get(&peak.prev_hash)?;
                let prev_prev = blocks.get(&prev.prev_hash)?;
                let expected = make_sub_epoch_summary(
                    &self.constants,
                    blocks,
                    peak.height,
                    prev_prev,
                    finish_epoch.then_some(next_difficulty),
                    finish_epoch.then_some(next_sub_slot_iters),
                )
                .ok()?;
                if eos.challenge_chain.subepoch_summary_hash != Some(expected.hash().ok()?) {
                    debug!("bad SES in new_finished_sub_slot");
                    return None;
                }
                if finish_epoch {
                    if eos.challenge_chain.new_sub_slot_iters != Some(next_sub_slot_iters)
                        || eos.challenge_chain.new_difficulty != Some(next_difficulty)
                    {
                        return None;
                    }
                } else if eos.challenge_chain.new_sub_slot_iters.is_some()
                    || eos.challenge_chain.new_difficulty.is_some()
                {
                    return None;
                }
            }
        } else {
            // Empty slot (no infusion since the last slot boundary).
            if eos.challenge_chain.subepoch_summary_hash.is_some() {
                debug!("SES not correct, should be None in an empty slot");
                return None;
            }
            cc_start_element = ClassgroupElement::get_default_element();
            icc_start_element = Some(ClassgroupElement::get_default_element());
            iters = sub_slot_iters;
            icc_iters = Some(sub_slot_iters);
            icc_challenge = match &last_slot_eos {
                Some(s)
                    if s.infused_challenge_chain.is_some()
                        && s.reward_chain.deficit
                            != self.constants.min_blocks_per_challenge_block =>
                {
                    Some(s.infused_challenge_chain.as_ref()?.hash().ok()?)
                }
                _ => None,
            };
        }

        // cc VDF: the claimed info states the WHOLE sub-slot; the proof covers only the delta
        // from the last infusion.
        let claimed_cc = &eos.challenge_chain.challenge_chain_end_of_slot_vdf;
        let partial_cc = VdfInfo {
            challenge: cc_challenge,
            number_of_iterations: iters,
            output: claimed_cc.output,
        };
        if *claimed_cc
            != (VdfInfo {
                number_of_iterations: sub_slot_iters,
                ..partial_cc
            })
        {
            return None;
        }
        if !skip_vdf_validation {
            let proof = &eos.proofs.challenge_chain_slot_proof;
            if !proof.normalized_to_identity
                && !validate_vdf_info(&self.constants, &cc_start_element, &partial_cc, proof, None)
            {
                return None;
            }
            if proof.normalized_to_identity
                && !validate_vdf_info(
                    &self.constants,
                    &ClassgroupElement::get_default_element(),
                    claimed_cc,
                    proof,
                    None,
                )
            {
                return None;
            }

            // rc VDF always runs from the default element at the last infusion.
            if !validate_vdf_info(
                &self.constants,
                &ClassgroupElement::get_default_element(),
                &eos.reward_chain.end_of_slot_vdf,
                &eos.proofs.reward_chain_slot_proof,
                Some(&VdfInfo {
                    challenge: rc_challenge,
                    number_of_iterations: iters,
                    output: eos.reward_chain.end_of_slot_vdf.output,
                }),
            ) {
                return None;
            }
        }

        if let Some(icc_challenge) = icc_challenge {
            let icc = eos.infused_challenge_chain.as_ref()?;
            let icc_proof = eos.proofs.infused_challenge_chain_slot_proof.as_ref()?;
            if eos.reward_chain.deficit == self.constants.min_blocks_per_challenge_block {
                // Only at the end of a challenge slot does the cc commit to the ICC.
                if eos.challenge_chain.infused_challenge_chain_sub_slot_hash
                    != Some(icc.hash().ok()?)
                {
                    return None;
                }
            } else if eos
                .challenge_chain
                .infused_challenge_chain_sub_slot_hash
                .is_some()
            {
                return None;
            }
            if eos.reward_chain.infused_challenge_chain_sub_slot_hash != Some(icc.hash().ok()?) {
                return None;
            }

            // The claimed ICC info states the delta from the ICC start (`icc_iters`); the
            // proof covers only `iters` — the stretch since the peak's infusion.
            let claimed_icc = &icc.infused_challenge_chain_end_of_slot_vdf;
            let partial_icc = VdfInfo {
                challenge: icc_challenge,
                number_of_iterations: iters,
                output: claimed_icc.output,
            };
            if *claimed_icc
                != (VdfInfo {
                    number_of_iterations: icc_iters?,
                    ..partial_icc
                })
            {
                return None;
            }
            if !skip_vdf_validation {
                if !icc_proof.normalized_to_identity
                    && !validate_vdf_info(
                        &self.constants,
                        icc_start_element.as_ref()?,
                        &partial_icc,
                        icc_proof,
                        None,
                    )
                {
                    return None;
                }
                if icc_proof.normalized_to_identity
                    && !validate_vdf_info(
                        &self.constants,
                        &ClassgroupElement::get_default_element(),
                        claimed_icc,
                        icc_proof,
                        None,
                    )
                {
                    return None;
                }
            }
        } else {
            // First, empty sub-slot: no ICC anywhere in the bundle.
            if eos.infused_challenge_chain.is_some()
                || eos.proofs.infused_challenge_chain_slot_proof.is_some()
                || eos
                    .challenge_chain
                    .infused_challenge_chain_sub_slot_hash
                    .is_some()
                || eos
                    .reward_chain
                    .infused_challenge_chain_sub_slot_hash
                    .is_some()
            {
                return None;
            }
        }

        let sps = self.empty_sps();
        self.finished_sub_slots.push(FinishedSubSlot {
            eos: Some(eos.clone()),
            sps,
            start_total_iters: total_iters,
        });
        Some(())
    }

    /// Validate and cache a gossiped signage point at `index`. True means cached; false means
    /// rejected outright or parked in the future cache (its slot's infusion has not reached us
    /// yet).
    #[allow(clippy::too_many_lines)]
    pub fn new_signage_point(
        &mut self,
        index: u8,
        blocks: &HashMap<Bytes32, BlockRecord>,
        peak: Option<&BlockRecord>,
        next_sub_slot_iters: u64,
        signage_point: &SignagePoint,
        skip_vdf_validation: bool,
    ) -> bool {
        let sub_slot_iters = match peak {
            Some(p) if p.height >= 2 => p.sub_slot_iters,
            _ => self.constants.sub_slot_iters_starting,
        };
        if index == 0 || u32::from(index) >= self.constants.num_sps_sub_slot {
            return false;
        }
        // All four VDF/proof fields must be present for a real (index > 0) signage point. The
        // sub-slot-start SP (index 0, all-None) is rejected above; any partially-populated SP is
        // malformed gossip — reject rather than panic.
        let (Some(sp_cc_vdf), Some(sp_cc_proof), Some(sp_rc_vdf), Some(sp_rc_proof)) = (
            signage_point.cc_vdf.as_ref(),
            signage_point.cc_proof.as_ref(),
            signage_point.rc_vdf.as_ref(),
            signage_point.rc_proof.as_ref(),
        ) else {
            return false;
        };

        for slot_idx in 0..self.finished_sub_slots.len() {
            let (ss_challenge, ss_reward) = match &self.finished_sub_slots[slot_idx].eos {
                Some(s) => match (s.challenge_chain.hash(), s.reward_chain.hash()) {
                    (Ok(c), Ok(r)) => (c, r),
                    _ => return false,
                },
                None => (
                    self.constants.genesis_challenge,
                    self.constants.genesis_challenge,
                ),
            };
            if ss_challenge != sp_cc_vdf.challenge {
                continue;
            }
            let start_ss_total_iters = self.finished_sub_slots[slot_idx].start_total_iters;

            // A slot past the peak may carry the NEXT sub-slot iters.
            let future_sub_slot = peak.is_some_and(|p| start_ss_total_iters > p.total_iters);
            let checkpoint_size = if future_sub_slot {
                next_sub_slot_iters / u64::from(self.constants.num_sps_sub_slot)
            } else {
                sub_slot_iters / u64::from(self.constants.num_sps_sub_slot)
            };
            let delta_iters = checkpoint_size * u64::from(index);
            let sp_total_iters = start_ss_total_iters + u128::from(delta_iters);

            // Find the last infused block before this SP inside the slot; from-slot-start if
            // there is none.
            let mut curr = peak;
            let mut check_from_start = peak.is_none() || future_sub_slot;
            if !check_from_start {
                while let Some(c) = curr {
                    if c.total_iters <= start_ss_total_iters || c.total_iters <= sp_total_iters {
                        break;
                    }
                    if c.first_in_sub_slot() {
                        check_from_start = true;
                        break;
                    }
                    curr = blocks.get(&c.prev_hash);
                }
                if curr.is_none() {
                    check_from_start = true;
                }
            }

            let (cc_expected, rc_expected) = if check_from_start {
                (
                    VdfInfo {
                        challenge: ss_challenge,
                        number_of_iterations: delta_iters,
                        output: sp_cc_vdf.output,
                    },
                    VdfInfo {
                        challenge: ss_reward,
                        number_of_iterations: delta_iters,
                        output: sp_rc_vdf.output,
                    },
                )
            } else {
                let Some(c) = curr else { return false };
                let Ok(partial) = u64::try_from(sp_total_iters - c.total_iters) else {
                    return false;
                };
                (
                    VdfInfo {
                        challenge: ss_challenge,
                        number_of_iterations: partial,
                        output: sp_cc_vdf.output,
                    },
                    VdfInfo {
                        challenge: c.reward_infusion_new_challenge,
                        number_of_iterations: partial,
                        output: sp_rc_vdf.output,
                    },
                )
            };

            // The claimed cc info always states the delta from the slot start.
            if *sp_cc_vdf
                != (VdfInfo {
                    number_of_iterations: delta_iters,
                    ..cc_expected
                })
            {
                Self::trace_sp(
                    "claimed-cc-mismatch",
                    &format!(
                        "claimed iters {} challenge {} vs expected delta {delta_iters} challenge {} (from_start {check_from_start})",
                        sp_cc_vdf.number_of_iterations, sp_cc_vdf.challenge, cc_expected.challenge
                    ),
                );
                self.add_to_future_sp(signage_point, index);
                return false;
            }
            let start_ele = if check_from_start {
                ClassgroupElement::get_default_element()
            } else {
                match curr {
                    Some(c) => c.challenge_vdf_output,
                    None => return false,
                }
            };
            if !skip_vdf_validation {
                if !sp_cc_proof.normalized_to_identity
                    && !validate_vdf_info(
                        &self.constants,
                        &start_ele,
                        &cc_expected,
                        sp_cc_proof,
                        None,
                    )
                {
                    Self::trace_sp(
                        "cc-proof-invalid",
                        &format!(
                            "iters {} from_start {check_from_start}",
                            cc_expected.number_of_iterations
                        ),
                    );
                    self.add_to_future_sp(signage_point, index);
                    return false;
                }
                if sp_cc_proof.normalized_to_identity
                    && !validate_vdf_info(
                        &self.constants,
                        &ClassgroupElement::get_default_element(),
                        sp_cc_vdf,
                        sp_cc_proof,
                        None,
                    )
                {
                    Self::trace_sp("cc-proof-invalid-normalized", "");
                    self.add_to_future_sp(signage_point, index);
                    return false;
                }
            }

            if rc_expected.challenge != sp_rc_vdf.challenge {
                // Probably outdated relative to a newer infusion.
                Self::trace_sp(
                    "rc-challenge-mismatch",
                    &format!(
                        "expected {} claimed {} (from_start {check_from_start})",
                        rc_expected.challenge, sp_rc_vdf.challenge
                    ),
                );
                self.add_to_future_sp(signage_point, index);
                return false;
            }
            if !skip_vdf_validation
                && !validate_vdf_info(
                    &self.constants,
                    &ClassgroupElement::get_default_element(),
                    sp_rc_vdf,
                    sp_rc_proof,
                    Some(&rc_expected),
                )
            {
                Self::trace_sp(
                    "rc-proof-invalid",
                    &format!("iters {}", rc_expected.number_of_iterations),
                );
                self.add_to_future_sp(signage_point, index);
                return false;
            }

            self.finished_sub_slots[slot_idx].sps[index as usize] = Some(signage_point.clone());
            return true;
        }
        Self::trace_sp("no-slot-for-challenge", &format!("{}", sp_cc_vdf.challenge));
        self.add_to_future_sp(signage_point, index);
        false
    }

    fn trace_sp(reason: &str, detail: &str) {
        if std::env::var("DGXCH_TRACE_SP").is_ok() {
            eprintln!("sp-reject {reason}: {detail}");
        }
    }

    fn add_to_future_sp(&mut self, signage_point: &SignagePoint, index: u8) {
        // Keyed by the SP's reward-chain challenge. The sub-slot-start SP (all-None) never
        // reaches here — new_signage_point rejects index 0 before this call.
        if let Some(rc_vdf) = &signage_point.rc_vdf {
            self.future_sp
                .append(rc_vdf.challenge, (index, signage_point.clone()));
        }
    }

    /// Reset the slot list around a newly-confirmed peak. Returns the future-cached EOS and
    /// signage points that became connectable at this peak, already re-validated.
    pub fn new_peak(
        &mut self,
        peak: &BlockRecord,
        ctx: PeakSlotContext<'_>,
        blocks: &HashMap<Bytes32, BlockRecord>,
        next_sub_slot_iters: u64,
        next_difficulty: u64,
        skip_vdf_validation: bool,
    ) -> (Option<EndOfSubSlotBundle>, Vec<(u8, SignagePoint)>) {
        let PeakSlotContext {
            sp_sub_slot,
            ip_sub_slot,
            fork_block,
        } = ctx;
        if ip_sub_slot.is_none() {
            // Still in the chain's first sub-slot.
            self.initialize_genesis_sub_slot();
        } else {
            let mut sp_slot_sps = self.empty_sps();
            let mut ip_slot_sps = self.empty_sps();

            if fork_block.is_some_and(|f| f.sub_slot_iters != peak.sub_slot_iters) {
                // Reorg across a difficulty adjustment: drop every cached SP.
            } else if let Ok(interval_iters) =
                calculate_sp_interval_iters(&self.constants, peak.sub_slot_iters)
            {
                // Keep signage points at or before the fork point (the peak itself when this
                // is a plain extension).
                let fork_total_iters = fork_block.unwrap_or(peak).total_iters;
                for slot in &self.finished_sub_slots {
                    if slot.eos.is_none() {
                        continue;
                    }
                    let mut replaced = self.empty_sps();
                    for (i, sp) in slot.sps.iter().enumerate() {
                        if slot.start_total_iters + (i as u128) * u128::from(interval_iters)
                            < fork_total_iters
                        {
                            replaced[i] = sp.clone();
                        }
                    }
                    if slot.eos.as_ref() == sp_sub_slot {
                        sp_slot_sps = replaced.clone();
                    }
                    if slot.eos.as_ref() == ip_sub_slot {
                        ip_slot_sps = replaced;
                    }
                }
            }

            self.finished_sub_slots.clear();
            // 0 here is a load-bearing genesis sentinel — the branch below keys
            // `prev_sub_slot_total_iters == 0` as "first sub-slot from genesis". A non-genesis
            // underflow cannot reach this point: staging validates ip/sp iters before a record
            // confirms. Do not swap for error propagation without re-keying the genesis branch.
            let prev_sub_slot_total_iters =
                peak.sp_sub_slot_total_iters(&self.constants).unwrap_or(0);
            if sp_sub_slot.is_some() || prev_sub_slot_total_iters == 0 {
                self.finished_sub_slots.push(FinishedSubSlot {
                    eos: sp_sub_slot.cloned(),
                    sps: sp_slot_sps,
                    start_total_iters: prev_sub_slot_total_iters,
                });
            }
            let ip_sub_slot_total_iters =
                peak.ip_sub_slot_total_iters(&self.constants).unwrap_or(0);
            self.finished_sub_slots.push(FinishedSubSlot {
                eos: ip_sub_slot.cloned(),
                sps: ip_slot_sps,
                start_total_iters: ip_sub_slot_total_iters,
            });
        }

        // Anything future-cached on this peak's infusion is now connectable.
        let mut new_eos = None;
        for eos in self.future_eos.take(&peak.reward_infusion_new_challenge) {
            if self
                .new_finished_sub_slot(
                    &eos,
                    blocks,
                    Some(peak),
                    next_sub_slot_iters,
                    next_difficulty,
                    skip_vdf_validation,
                )
                .is_some()
            {
                new_eos = Some(eos);
                break;
            }
        }
        let mut new_sps = Vec::new();
        for (index, sp) in self.future_sp.take(&peak.reward_infusion_new_challenge) {
            if self.new_signage_point(
                index,
                blocks,
                Some(peak),
                peak.sub_slot_iters,
                &sp,
                skip_vdf_validation,
            ) {
                new_sps.push((index, sp));
            }
        }
        (new_eos, new_sps)
    }

    pub fn clear_slots(&mut self) {
        self.finished_sub_slots.clear();
        self.future_eos.clear();
        self.future_sp.clear();
    }

    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.finished_sub_slots.len()
    }
}

#[cfg(test)]
#[path = "../tests/unit/slots/tests.rs"]
mod tests;
