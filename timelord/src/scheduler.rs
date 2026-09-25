use crate::worker::{ProofRequest, ProofResult};
use dg_xch_core::blockchain::challenge_block_info::ChallengeBlockInfo;
use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::infused_challenge_chain_subslot::InfusedChallengeChainSubSlot;
use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality_for_proof, calculate_sp_interval_iters,
    calculate_sp_iters, is_overflow_block,
};
use dg_xch_core::consensus::producer::verify_plot_signature;
use dg_xch_core::protocols::timelord::{
    NewEndOfSubSlotVDF, NewGenesisTimelord, NewInfusionPointVDF, NewPeakTimelord,
    NewSignagePointVDF, NewUnfinishedBlockTimelord,
};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{Error, ErrorKind};
use std::time::{Duration, Instant};

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChainInput {
    challenge: Bytes32,
    input: ClassgroupElement,
}

impl ChainInput {
    fn identity(challenge: Bytes32) -> Self {
        Self {
            challenge,
            input: ClassgroupElement::get_default_element(),
        }
    }

    fn request(self, generation: u64, iterations: u64, bits: usize) -> ProofRequest {
        ProofRequest {
            generation,
            challenge: self.challenge,
            input: self.input,
            iterations,
            discriminant_bits: bits,
        }
    }
}

#[derive(Debug, Clone)]
struct State {
    total_iters: u128,
    slot_start: u128,
    sub_slot_iters: u64,
    difficulty: u64,
    height: Option<u32>,
    weight: u128,
    deficit: u8,
    challenge: ChainInput,
    reward: ChainInput,
    infused: Option<ChainInput>,
    last_challenge_start: u128,
    summary: Option<SubEpochSummary>,
    passed_summary_height: bool,
    at_peak: bool,
    new_epoch: bool,
    anchor: Instant,
}

impl State {
    fn end(&self) -> Result<u128, Error> {
        self.slot_start
            .checked_add(u128::from(self.sub_slot_iters))
            .ok_or_else(|| invalid("slot iteration overflow"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    SignagePoint(u8),
    InfusionPoint(Bytes32),
    EndOfSubSlot,
}

#[derive(Debug, Clone)]
pub struct WorkPlan {
    pub generation: u64,
    pub total_iters: u128,
    pub event: Event,
    pub challenge: ProofRequest,
    pub reward: ProofRequest,
    pub infused: Option<ProofRequest>,
    pub not_before: Instant,
}

#[derive(Debug, Clone)]
pub struct WorkResult {
    pub challenge: ProofResult,
    pub reward: ProofResult,
    pub infused: Option<ProofResult>,
}

#[derive(Debug, Clone)]
pub enum Output {
    SignagePoint(NewSignagePointVDF),
    InfusionPoint(NewInfusionPointVDF),
    EndOfSubSlot(NewEndOfSubSlotVDF),
}

#[derive(Debug, Clone)]
struct Candidate {
    hash: Bytes32,
    total_iters: u128,
    overflow: bool,
    slot_start: u128,
    rc_prev: Bytes32,
    signage_total_iters: u128,
}

pub struct Scheduler {
    constants: ConsensusConstants,
    allows_bootstrap: bool,
    iterations_per_second: Option<u64>,
    state: Option<State>,
    generation: u64,
    last_peak: Option<Bytes32>,
    transaction_height: Option<u32>,
    rewards: VecDeque<(Bytes32, u128)>,
    candidates: BTreeMap<[u8; 32], Candidate>,
    signage: HashSet<u8>,
    infused: HashSet<Bytes32>,
    awaiting_peak: Option<Instant>,
}

impl Scheduler {
    pub fn new(
        constants: ConsensusConstants,
        allows_bootstrap: bool,
        iterations_per_second: Option<u64>,
    ) -> Result<Self, Error> {
        if constants.num_sps_sub_slot == 0
            || constants.num_sps_sub_slot > 256
            || constants.num_sp_intervals_extra >= u64::from(constants.num_sps_sub_slot)
            || constants.min_blocks_per_challenge_block == 0
            || constants.max_sub_slot_blocks == 0
            || constants.sub_epoch_blocks == 0
            || iterations_per_second == Some(0)
            || !(16..=1024).contains(&constants.discriminant_size_bits)
            || !constants.discriminant_size_bits.is_multiple_of(8)
        {
            return Err(invalid("invalid regular timelord parameters"));
        }
        Self::validate_timing(
            &constants,
            constants.difficulty_starting,
            constants.sub_slot_iters_starting,
        )?;
        Ok(Self {
            constants,
            allows_bootstrap,
            iterations_per_second,
            state: None,
            generation: 0,
            last_peak: None,
            transaction_height: None,
            rewards: VecDeque::new(),
            candidates: BTreeMap::new(),
            signage: HashSet::new(),
            infused: HashSet::new(),
            awaiting_peak: None,
        })
    }

    fn validate_timing(
        constants: &ConsensusConstants,
        difficulty: u64,
        iterations: u64,
    ) -> Result<(), Error> {
        if difficulty == 0
            || iterations == 0
            || calculate_sp_interval_iters(constants, iterations)? == 0
        {
            return Err(invalid("invalid chain difficulty or sub-slot iterations"));
        }
        Ok(())
    }

    fn reset_generation(&mut self) -> Result<(), Error> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("timelord generation overflow"))?;
        self.signage.clear();
        self.infused.clear();
        self.awaiting_peak = None;
        Ok(())
    }

    pub fn initialized(&self) -> bool {
        self.state.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn needs_resync(&self) -> bool {
        self.awaiting_peak
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub fn set_genesis(&mut self, message: NewGenesisTimelord) -> Result<bool, Error> {
        if !self.allows_bootstrap
            || message.genesis_challenge != self.constants.genesis_challenge
            || message.difficulty != self.constants.difficulty_starting
            || message.sub_slot_iters != self.constants.sub_slot_iters_starting
            || message.discriminant_size_bits != self.constants.discriminant_size_bits
        {
            return Err(invalid("unauthorized or mismatched genesis work"));
        }
        if self.state.is_some() {
            return Ok(false);
        }
        self.reset_generation()?;
        self.transaction_height = Some(0);
        self.rewards.push_back((message.genesis_challenge, 0));
        self.state = Some(State {
            total_iters: 0,
            slot_start: 0,
            sub_slot_iters: message.sub_slot_iters,
            difficulty: message.difficulty,
            height: None,
            weight: 0,
            deficit: self.constants.min_blocks_per_challenge_block,
            challenge: ChainInput::identity(message.genesis_challenge),
            reward: ChainInput::identity(message.genesis_challenge),
            infused: None,
            last_challenge_start: 0,
            summary: None,
            passed_summary_height: false,
            at_peak: false,
            new_epoch: false,
            anchor: Instant::now(),
        });
        Ok(true)
    }

    pub fn set_peak(&mut self, message: NewPeakTimelord) -> Result<bool, Error> {
        Self::validate_timing(&self.constants, message.difficulty, message.sub_slot_iters)?;
        let block = &message.reward_chain_block;
        let hash = block.hash()?;
        if self.last_peak == Some(hash)
            || self
                .state
                .as_ref()
                .is_some_and(|state| block.weight < state.weight)
        {
            return Ok(false);
        }
        let offset = block.challenge_chain_ip_vdf.number_of_iterations;
        let slot_start = block
            .total_iters
            .checked_sub(u128::from(offset))
            .ok_or_else(|| invalid("peak precedes slot origin"))?;
        if offset == 0
            || offset >= message.sub_slot_iters
            || message.deficit > self.constants.min_blocks_per_challenge_block
            || message.last_challenge_sb_or_eos_total_iters > block.total_iters
            || message.previous_reward_challenges.len()
                > self
                    .reward_limit()
                    .saturating_add(self.constants.max_sub_slot_blocks as usize)
            || block.weight == 0
        {
            return Err(invalid("invalid peak chain state"));
        }
        let mut rewards = VecDeque::from(message.previous_reward_challenges);
        if rewards.iter().any(|(_, total)| *total > block.total_iters)
            || rewards
                .iter()
                .zip(rewards.iter().skip(1))
                .any(|(previous, next)| previous.1 >= next.1)
        {
            return Err(invalid("invalid reward challenge history"));
        }
        if rewards.front().is_some_and(|(challenge, total)| {
            *total == 0 && *challenge != self.constants.genesis_challenge
        }) {
            return Err(invalid("reward history has a different genesis challenge"));
        }
        if rewards.front().is_none_or(|(_, total)| *total != 0) {
            rewards.push_front((self.constants.genesis_challenge, 0));
        }
        if rewards.back().is_some_and(|(challenge, total)| {
            (*total == block.total_iters && *challenge != hash)
                || (*challenge == hash && *total != block.total_iters)
        }) {
            return Err(invalid("peak conflicts with reward challenge history"));
        }
        if rewards
            .back()
            .is_none_or(|(challenge, _)| *challenge != hash)
        {
            rewards.push_back((hash, block.total_iters));
        }
        let infused = if let Some(info) = block.infused_challenge_chain_ip_vdf {
            Some(ChainInput {
                challenge: info.challenge,
                input: info.output,
            })
        } else if message.deficit == self.constants.min_blocks_per_challenge_block - 1 {
            Some(ChainInput::identity(
                ChallengeBlockInfo {
                    proof_of_space: block.proof_of_space.clone(),
                    challenge_chain_sp_vdf: block.challenge_chain_sp_vdf,
                    challenge_chain_sp_signature: block.challenge_chain_sp_signature,
                    challenge_chain_ip_vdf: block.challenge_chain_ip_vdf,
                }
                .hash()?,
            ))
        } else {
            None
        };
        if message.deficit < self.constants.min_blocks_per_challenge_block - 1 && infused.is_none()
        {
            return Err(invalid("peak is missing its infused challenge chain"));
        }
        let contiguous = self.state.as_ref().is_some_and(|state| {
            state.height.and_then(|height| height.checked_add(1)) == Some(block.height)
                && rewards
                    .iter()
                    .any(|(reward, _)| Some(*reward) == self.last_peak)
        });
        let new_epoch = self
            .state
            .as_ref()
            .is_some_and(|state| state.new_epoch && state.slot_start == slot_start);
        if block.is_transaction_block {
            self.transaction_height = Some(block.height);
        } else if !contiguous {
            self.transaction_height = None;
        }
        self.reset_generation()?;
        self.last_peak = Some(hash);
        self.rewards = rewards;
        self.trim_rewards();
        self.state = Some(State {
            total_iters: block.total_iters,
            slot_start,
            sub_slot_iters: message.sub_slot_iters,
            difficulty: message.difficulty,
            height: Some(block.height),
            weight: block.weight,
            deficit: message.deficit,
            challenge: ChainInput {
                challenge: block.challenge_chain_ip_vdf.challenge,
                input: block.challenge_chain_ip_vdf.output,
            },
            reward: ChainInput::identity(hash),
            infused,
            last_challenge_start: message.last_challenge_sb_or_eos_total_iters,
            summary: message.sub_epoch_summary,
            passed_summary_height: block
                .height
                .checked_add(1)
                .is_some_and(|height| height.is_multiple_of(self.constants.sub_epoch_blocks))
                || message.passes_ses_height_but_not_yet_included,
            at_peak: true,
            new_epoch,
            anchor: Instant::now(),
        });
        self.prune_candidates();
        Ok(true)
    }

    fn reward_limit(&self) -> usize {
        (self.constants.max_sub_slot_blocks as usize).saturating_mul(2)
    }

    fn trim_rewards(&mut self) {
        while self.rewards.len() > self.reward_limit() {
            self.rewards.pop_front();
        }
    }

    fn reward_matches(&self, challenge: Bytes32, signage_total: u128) -> bool {
        self.rewards
            .iter()
            .rev()
            .find(|(_, total)| *total <= signage_total)
            .is_some_and(|(expected, _)| *expected == challenge)
    }

    fn prune_candidates(&mut self) {
        if let Some(state) = &self.state {
            let rewards = &self.rewards;
            self.candidates.retain(|_, candidate| {
                candidate.total_iters > state.total_iters
                    && candidate.slot_start >= state.slot_start
                    && (!state.new_epoch
                        || !candidate.overflow
                        || candidate.slot_start > state.slot_start)
                    && rewards
                        .iter()
                        .rev()
                        .find(|(_, total)| *total <= candidate.signage_total_iters)
                        .is_some_and(|(hash, _)| *hash == candidate.rc_prev)
            });
        }
    }

    pub fn add_unfinished(&mut self, message: NewUnfinishedBlockTimelord) -> Result<bool, Error> {
        let Some(state) = &self.state else {
            return Ok(false);
        };
        let block = &message.reward_chain_block;
        let hash = block.hash()?;
        if self.candidates.contains_key(&hash.const_bytes()) || self.infused.contains(&hash) {
            return Ok(false);
        }
        if self.candidates.len() >= self.reward_limit()
            || message.difficulty != state.difficulty
            || message.sub_slot_iters != state.sub_slot_iters
        {
            return Ok(false);
        }
        let height = match state.height {
            Some(height) => height
                .checked_add(1)
                .ok_or_else(|| invalid("chain height overflow"))?,
            None => 0,
        };
        let signage_hash = match block.challenge_chain_sp_vdf {
            Some(info) if block.signage_point_index != 0 => info.output.hash()?,
            None if block.signage_point_index == 0 => block.pos_ss_cc_challenge_hash,
            _ => return Err(invalid("invalid unfinished signage-point fields")),
        };
        let quality = match self.transaction_height {
            Some(transaction_height) => dg_xch_pos::verify_and_get_quality_string_with_context(
                &block.proof_of_space,
                &self.constants,
                block.pos_ss_cc_challenge_hash,
                signage_hash,
                height,
                transaction_height,
            ),
            None => dg_xch_pos::verify_and_get_quality_string_without_activation(
                &block.proof_of_space,
                &self.constants,
                block.pos_ss_cc_challenge_hash,
                signage_hash,
                height,
            ),
        }
        .ok_or_else(|| invalid("unfinished block proof of space is invalid"))?;
        if !verify_plot_signature(
            &block.proof_of_space.plot_public_key,
            signage_hash,
            &block.challenge_chain_sp_signature,
        ) {
            return Err(invalid("unfinished block signage signature is invalid"));
        }
        let required = calculate_iterations_quality_for_proof(
            &self.constants,
            &block.proof_of_space,
            quality,
            message.difficulty,
            signage_hash,
        );
        let offset = calculate_ip_iters(
            &self.constants,
            message.sub_slot_iters,
            block.signage_point_index,
            required,
        )?;
        let signage_offset = calculate_sp_iters(
            &self.constants,
            message.sub_slot_iters,
            block.signage_point_index,
        )?;
        if block.challenge_chain_sp_vdf.is_some_and(|info| {
            info.challenge != block.pos_ss_cc_challenge_hash
                || info.number_of_iterations != signage_offset
        }) {
            return Err(invalid(
                "unfinished challenge signage VDF metadata mismatch",
            ));
        }
        let reward_signage_hash = match block.reward_chain_sp_vdf {
            Some(info) if block.signage_point_index != 0 && info.challenge == message.rc_prev => {
                info.output.hash()?
            }
            None if block.signage_point_index == 0 => message.rc_prev,
            _ => return Err(invalid("invalid unfinished reward signage-point fields")),
        };
        if !verify_plot_signature(
            &block.proof_of_space.plot_public_key,
            reward_signage_hash,
            &block.reward_chain_sp_signature,
        ) {
            return Err(invalid("unfinished reward signage signature is invalid"));
        }
        let overflow = is_overflow_block(&self.constants, block.signage_point_index)?;
        let slot_start = block
            .total_iters
            .checked_sub(u128::from(offset))
            .ok_or_else(|| invalid("unfinished block iteration underflow"))?;
        let next_slot = state.end()?;
        if block.total_iters <= state.total_iters
            || (slot_start != state.slot_start && !(overflow && slot_start == next_slot))
            || (overflow && state.new_epoch && slot_start == state.slot_start)
        {
            return Ok(false);
        }
        if (!overflow || slot_start == next_slot)
            && block.pos_ss_cc_challenge_hash != state.challenge.challenge
        {
            return Ok(false);
        }
        let signage_total_iters = slot_start
            .checked_add(u128::from(signage_offset))
            .and_then(|total| {
                if overflow {
                    total.checked_sub(u128::from(message.sub_slot_iters))
                } else {
                    Some(total)
                }
            })
            .ok_or_else(|| invalid("unfinished signage iteration overflow"))?;
        if !self.reward_matches(message.rc_prev, signage_total_iters) {
            return Ok(false);
        }
        self.candidates.insert(
            hash.const_bytes(),
            Candidate {
                hash,
                total_iters: block.total_iters,
                overflow,
                slot_start,
                rc_prev: message.rc_prev,
                signage_total_iters,
            },
        );
        Ok(true)
    }

    pub fn next_plan(&self) -> Result<Option<WorkPlan>, Error> {
        let Some(state) = &self.state else {
            return Ok(None);
        };
        if self
            .awaiting_peak
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Ok(None);
        }
        let mut total_iters = state.end()?;
        let mut event = Event::EndOfSubSlot;
        for index in 1..self.constants.num_sps_sub_slot {
            let index = u8::try_from(index).map_err(Error::other)?;
            let offset = calculate_sp_iters(&self.constants, state.sub_slot_iters, index)?;
            let total = state
                .slot_start
                .checked_add(u128::from(offset))
                .ok_or_else(|| invalid("signage iteration overflow"))?;
            if total > state.total_iters && !self.signage.contains(&index) && total < total_iters {
                total_iters = total;
                event = Event::SignagePoint(index);
            }
        }
        let blocks = self
            .rewards
            .iter()
            .filter(|(_, total)| *total > state.slot_start)
            .count();
        if blocks < self.constants.max_sub_slot_blocks as usize {
            for candidate in self.candidates.values() {
                if candidate.slot_start == state.slot_start
                    && !self.infused.contains(&candidate.hash)
                    && candidate.total_iters > state.total_iters
                    && candidate.total_iters <= total_iters
                    && (!state.new_epoch || !candidate.overflow)
                {
                    total_iters = candidate.total_iters;
                    event = Event::InfusionPoint(candidate.hash);
                }
            }
        }
        let iterations = u64::try_from(
            total_iters
                .checked_sub(state.total_iters)
                .ok_or_else(|| invalid("work iteration underflow"))?,
        )
        .map_err(Error::other)?;
        if iterations == 0 {
            return Err(invalid("zero-length regular VDF work"));
        }
        let bits = usize::try_from(self.constants.discriminant_size_bits).map_err(Error::other)?;
        let delay = match self.iterations_per_second {
            Some(rate) => Duration::from_secs(iterations / rate)
                .checked_add(Duration::from_nanos(
                    (u128::from(iterations % rate) * 1_000_000_000 / u128::from(rate)) as u64,
                ))
                .ok_or_else(|| invalid("pacing duration overflow"))?,
            None => Duration::ZERO,
        };
        let not_before = state
            .anchor
            .checked_add(delay)
            .ok_or_else(|| invalid("pacing deadline overflow"))?;
        let infused = if matches!(event, Event::SignagePoint(_)) {
            None
        } else {
            state
                .infused
                .map(|chain| chain.request(self.generation, iterations, bits))
        };
        Ok(Some(WorkPlan {
            generation: self.generation,
            total_iters,
            event,
            challenge: state.challenge.request(self.generation, iterations, bits),
            reward: state.reward.request(self.generation, iterations, bits),
            infused,
            not_before,
        }))
    }

    fn validate_result(&self, request: &ProofRequest, result: &ProofResult) -> Result<(), Error> {
        if result.generation != request.generation
            || result.info.challenge != request.challenge
            || result.info.number_of_iterations != request.iterations
            || result.proof.normalized_to_identity
            || !dg_xch_vdf::validate_vdf_info_serial(
                &self.constants,
                &request.input,
                &result.info,
                &result.proof,
                None,
            )
        {
            return Err(invalid("backend returned an invalid regular VDF proof"));
        }
        Ok(())
    }

    pub fn finish(&mut self, plan: WorkPlan, result: WorkResult) -> Result<Option<Output>, Error> {
        if plan.generation != self.generation {
            return Ok(None);
        }
        let Some(expected) = self.next_plan()? else {
            return Ok(None);
        };
        if plan.total_iters != expected.total_iters
            || plan.event != expected.event
            || plan.challenge != expected.challenge
            || plan.reward != expected.reward
            || plan.infused != expected.infused
        {
            return Ok(None);
        }
        if Instant::now() < expected.not_before {
            return Err(Error::new(
                ErrorKind::WouldBlock,
                "regular VDF pacing deadline has not elapsed",
            ));
        }
        self.validate_result(&plan.challenge, &result.challenge)?;
        self.validate_result(&plan.reward, &result.reward)?;
        match (&plan.infused, &result.infused) {
            (Some(request), Some(proof)) => self.validate_result(request, proof)?,
            (None, None) => {}
            _ => return Err(invalid("backend returned mismatched infused-chain work")),
        }
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| invalid("timelord state is not initialized"))?
            .clone();
        let mut challenge = result.challenge.info;
        challenge.number_of_iterations = u64::try_from(
            plan.total_iters
                .checked_sub(state.slot_start)
                .ok_or_else(|| invalid("work precedes slot origin"))?,
        )
        .map_err(Error::other)?;
        match plan.event {
            Event::SignagePoint(index) => {
                self.signage.insert(index);
                Ok(Some(Output::SignagePoint(NewSignagePointVDF {
                    index_from_challenge: index,
                    challenge_chain_sp_vdf: challenge,
                    challenge_chain_sp_proof: result.challenge.proof,
                    reward_chain_sp_vdf: result.reward.info,
                    reward_chain_sp_proof: result.reward.proof,
                })))
            }
            Event::InfusionPoint(hash) => {
                if self.infused.len() >= self.reward_limit() {
                    return Err(invalid("too many unconfirmed infusion points"));
                }
                self.infused.insert(hash);
                self.candidates.remove(&hash.const_bytes());
                self.awaiting_peak = Instant::now().checked_add(Duration::from_secs(5));
                let (infused_info, infused_proof) = match result.infused {
                    Some(proof) => (Some(proof.info), Some(proof.proof)),
                    None => (None, None),
                };
                Ok(Some(Output::InfusionPoint(NewInfusionPointVDF {
                    unfinished_reward_hash: hash,
                    challenge_chain_ip_vdf: challenge,
                    challenge_chain_ip_proof: result.challenge.proof,
                    reward_chain_ip_vdf: result.reward.info,
                    reward_chain_ip_proof: result.reward.proof,
                    infused_challenge_chain_ip_vdf: infused_info,
                    infused_challenge_chain_ip_proof: infused_proof,
                })))
            }
            Event::EndOfSubSlot => {
                let summary = if state.at_peak && state.passed_summary_height && state.deficit == 0
                {
                    state.summary
                } else {
                    None
                };
                let new_difficulty = summary.and_then(|summary| summary.new_difficulty);
                let new_iterations = summary.and_then(|summary| summary.new_sub_slot_iters);
                if new_difficulty.is_some() != new_iterations.is_some() {
                    return Err(invalid("incomplete epoch adjustment"));
                }
                Self::validate_timing(
                    &self.constants,
                    new_difficulty.unwrap_or(state.difficulty),
                    new_iterations.unwrap_or(state.sub_slot_iters),
                )?;
                let (infused_chain, infused_proof) = match result.infused {
                    Some(mut result) => {
                        result.info.number_of_iterations = u64::try_from(
                            plan.total_iters
                                .checked_sub(state.last_challenge_start)
                                .ok_or_else(|| invalid("infused-chain iteration underflow"))?,
                        )
                        .map_err(Error::other)?;
                        (
                            Some(InfusedChallengeChainSubSlot {
                                infused_challenge_chain_end_of_slot_vdf: result.info,
                            }),
                            Some(result.proof),
                        )
                    }
                    None => (None, None),
                };
                let infused_hash = infused_chain
                    .as_ref()
                    .map(InfusedChallengeChainSubSlot::hash)
                    .transpose()?;
                let challenge_chain = ChallengeChainSubSlot {
                    challenge_chain_end_of_slot_vdf: challenge,
                    infused_challenge_chain_sub_slot_hash: if state.deficit == 0 {
                        infused_hash
                    } else {
                        None
                    },
                    subepoch_summary_hash: summary
                        .as_ref()
                        .map(SubEpochSummary::hash)
                        .transpose()?,
                    new_sub_slot_iters: new_iterations,
                    new_difficulty,
                };
                let deficit = if state.deficit == 0 {
                    self.constants.min_blocks_per_challenge_block
                } else {
                    state.deficit
                };
                let challenge_hash = challenge_chain.hash()?;
                let reward_chain = RewardChainSubSlot {
                    end_of_slot_vdf: result.reward.info,
                    challenge_chain_sub_slot_hash: challenge_hash,
                    infused_challenge_chain_sub_slot_hash: infused_hash,
                    deficit,
                };
                let reward_hash = reward_chain.hash()?;
                let bundle = EndOfSubSlotBundle {
                    challenge_chain,
                    infused_challenge_chain: infused_chain,
                    reward_chain,
                    proofs: SubSlotProofs {
                        challenge_chain_slot_proof: result.challenge.proof,
                        infused_challenge_chain_slot_proof: infused_proof,
                        reward_chain_slot_proof: result.reward.proof,
                    },
                };
                let infused = if deficit < self.constants.min_blocks_per_challenge_block {
                    Some(ChainInput::identity(infused_hash.ok_or_else(|| {
                        invalid("end-of-slot is missing infused chain")
                    })?))
                } else {
                    None
                };
                self.reset_generation()?;
                self.state = Some(State {
                    total_iters: plan.total_iters,
                    slot_start: plan.total_iters,
                    sub_slot_iters: new_iterations.unwrap_or(state.sub_slot_iters),
                    difficulty: new_difficulty.unwrap_or(state.difficulty),
                    height: state.height,
                    weight: state.weight,
                    deficit,
                    challenge: ChainInput::identity(challenge_hash),
                    reward: ChainInput::identity(reward_hash),
                    infused,
                    last_challenge_start: plan.total_iters,
                    summary: None,
                    passed_summary_height: state.passed_summary_height && summary.is_none(),
                    at_peak: false,
                    new_epoch: new_difficulty.is_some(),
                    anchor: Instant::now(),
                });
                self.rewards.push_back((reward_hash, plan.total_iters));
                self.trim_rewards();
                self.prune_candidates();
                Ok(Some(Output::EndOfSubSlot(NewEndOfSubSlotVDF {
                    end_of_sub_slot_bundle: bundle,
                })))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::{RegularProofRequest, prove_regular};
    use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
    use dg_xch_core::blockchain::reward_chain_block::RewardChainBlock;
    use dg_xch_core::blockchain::sized_bytes::{Bytes48, Bytes96};
    use dg_xch_core::consensus::constants::MAINNET;

    fn constants() -> ConsensusConstants {
        ConsensusConstants {
            genesis_challenge: Bytes32::from([73; 32]),
            difficulty_starting: 1,
            sub_slot_iters_starting: 64,
            num_sps_sub_slot: 8,
            discriminant_size_bits: 32,
            ..MAINNET
        }
    }

    fn genesis(constants: &ConsensusConstants) -> NewGenesisTimelord {
        NewGenesisTimelord {
            genesis_challenge: constants.genesis_challenge,
            difficulty: constants.difficulty_starting,
            sub_slot_iters: constants.sub_slot_iters_starting,
            discriminant_size_bits: constants.discriminant_size_bits,
        }
    }

    fn scheduler() -> Scheduler {
        let constants = constants();
        let mut scheduler = Scheduler::new(constants, true, None).unwrap();
        scheduler.set_genesis(genesis(&constants)).unwrap();
        scheduler
    }

    fn prove_plan(plan: &WorkPlan) -> WorkResult {
        let prove = |request: &ProofRequest| {
            prove_regular(&RegularProofRequest {
                request: request.clone(),
                memory_bytes: 128 * 1024,
            })
            .unwrap()
        };
        WorkResult {
            challenge: prove(&plan.challenge),
            reward: prove(&plan.reward),
            infused: plan.infused.as_ref().map(prove),
        }
    }

    fn advance(scheduler: &mut Scheduler) -> Output {
        let plan = scheduler.next_plan().unwrap().unwrap();
        let result = prove_plan(&plan);
        scheduler.finish(plan, result).unwrap().unwrap()
    }

    fn candidate(scheduler: &mut Scheduler, total_iters: u128, overflow: bool) -> Bytes32 {
        let state = scheduler.state.as_ref().unwrap();
        let hash = Bytes32::from([total_iters as u8; 32]);
        let slot_start =
            total_iters / u128::from(state.sub_slot_iters) * u128::from(state.sub_slot_iters);
        scheduler.candidates.insert(
            hash.const_bytes(),
            Candidate {
                hash,
                total_iters,
                overflow,
                slot_start,
                rc_prev: state.reward.challenge,
                signage_total_iters: state.total_iters,
            },
        );
        hash
    }

    fn peak(scheduler: &Scheduler, point: &NewInfusionPointVDF, deficit: u8) -> NewPeakTimelord {
        let state = scheduler.state.as_ref().unwrap();
        let height = state.height.map_or(0, |height| height + 1);
        NewPeakTimelord {
            reward_chain_block: RewardChainBlock {
                weight: state.weight + u128::from(state.difficulty),
                height,
                total_iters: state.slot_start
                    + u128::from(point.challenge_chain_ip_vdf.number_of_iterations),
                signage_point_index: 0,
                pos_ss_cc_challenge_hash: state.challenge.challenge,
                proof_of_space: ProofOfSpace {
                    challenge: Bytes32::default(),
                    pool_public_key: None,
                    pool_contract_puzzle_hash: Some(Bytes32::default()),
                    plot_public_key: Bytes48::default(),
                    version: 0,
                    plot_index: 0,
                    meta_group: 0,
                    strength: 0,
                    size: 32,
                    proof: Vec::new().into(),
                },
                challenge_chain_sp_vdf: None,
                challenge_chain_sp_signature: Bytes96::default(),
                challenge_chain_ip_vdf: point.challenge_chain_ip_vdf,
                reward_chain_sp_vdf: None,
                reward_chain_sp_signature: Bytes96::default(),
                reward_chain_ip_vdf: point.reward_chain_ip_vdf,
                infused_challenge_chain_ip_vdf: point.infused_challenge_chain_ip_vdf,
                is_transaction_block: true,
            },
            difficulty: state.difficulty,
            deficit,
            sub_slot_iters: state.sub_slot_iters,
            sub_epoch_summary: None,
            previous_reward_challenges: scheduler.rewards.iter().copied().collect(),
            last_challenge_sb_or_eos_total_iters: if deficit
                == scheduler.constants.min_blocks_per_challenge_block - 1
            {
                state.slot_start + u128::from(point.challenge_chain_ip_vdf.number_of_iterations)
            } else {
                state.last_challenge_start
            },
            passes_ses_height_but_not_yet_included: false,
        }
    }

    #[test]
    fn genesis_requires_explicit_matching_authorization() {
        let constants = constants();
        let mut public = Scheduler::new(constants, false, None).unwrap();
        assert!(public.next_plan().unwrap().is_none());
        assert!(public.set_genesis(genesis(&constants)).is_err());
        let mut custom = Scheduler::new(constants, true, None).unwrap();
        let mut wrong = genesis(&constants);
        wrong.difficulty += 1;
        assert!(custom.set_genesis(wrong).is_err());
        assert!(custom.set_genesis(genesis(&constants)).unwrap());
        assert!(!custom.set_genesis(genesis(&constants)).unwrap());
        assert_eq!(custom.next_plan().unwrap().unwrap().challenge.iterations, 8);
    }

    #[test]
    fn real_signage_and_empty_slots_keep_height_zero_bootstrap() {
        let mut scheduler = scheduler();
        for index in 1..8 {
            let Output::SignagePoint(point) = advance(&mut scheduler) else {
                panic!("expected signage point")
            };
            assert_eq!(point.index_from_challenge, index);
            assert_eq!(
                point.challenge_chain_sp_vdf.number_of_iterations,
                u64::from(index) * 8
            );
            assert!(!point.challenge_chain_sp_proof.normalized_to_identity);
            assert_eq!(point.challenge_chain_sp_vdf, point.reward_chain_sp_vdf);
        }
        let Output::EndOfSubSlot(slot) = advance(&mut scheduler) else {
            panic!("expected slot")
        };
        assert_eq!(
            slot.end_of_sub_slot_bundle
                .challenge_chain
                .challenge_chain_end_of_slot_vdf
                .number_of_iterations,
            64
        );
        assert_eq!(slot.end_of_sub_slot_bundle.reward_chain.deficit, 16);
        assert!(
            slot.end_of_sub_slot_bundle
                .infused_challenge_chain
                .is_none()
        );
        assert_eq!(scheduler.state.as_ref().unwrap().height, None);
        assert_eq!(scheduler.state.as_ref().unwrap().total_iters, 64);
        assert_eq!(
            scheduler.next_plan().unwrap().unwrap().challenge.challenge,
            slot.end_of_sub_slot_bundle.challenge_chain.hash().unwrap()
        );
    }

    #[test]
    fn infusion_restarts_real_cc_rc_and_icc_with_partial_proof_lengths() {
        let mut scheduler = scheduler();
        candidate(&mut scheduler, 25, false);
        for _ in 0..3 {
            advance(&mut scheduler);
        }
        let Output::InfusionPoint(point) = advance(&mut scheduler) else {
            panic!("expected infusion")
        };
        assert_eq!(point.challenge_chain_ip_vdf.number_of_iterations, 25);
        assert!(scheduler.next_plan().unwrap().is_none());
        let peak = peak(&scheduler, &point, 15);
        assert!(scheduler.set_peak(peak.clone()).unwrap());
        let plan = scheduler.next_plan().unwrap().unwrap();
        assert_eq!(plan.challenge.iterations, 7);
        assert_eq!(plan.challenge.input, point.challenge_chain_ip_vdf.output);
        assert_eq!(
            plan.reward.challenge,
            peak.reward_chain_block.hash().unwrap()
        );
        let Output::SignagePoint(point) = advance(&mut scheduler) else {
            panic!("expected signage")
        };
        assert_eq!(point.challenge_chain_sp_vdf.number_of_iterations, 32);
        assert_eq!(point.reward_chain_sp_vdf.number_of_iterations, 7);
        assert!(!point.challenge_chain_sp_proof.normalized_to_identity);
        for _ in 0..3 {
            advance(&mut scheduler);
        }
        let plan = scheduler.next_plan().unwrap().unwrap();
        assert_eq!(plan.event, Event::EndOfSubSlot);
        assert_eq!(plan.challenge.iterations, 39);
        assert!(plan.infused.is_some());
        let Output::EndOfSubSlot(slot) = advance(&mut scheduler) else {
            panic!("expected slot")
        };
        let bundle = slot.end_of_sub_slot_bundle;
        assert_eq!(
            bundle
                .challenge_chain
                .challenge_chain_end_of_slot_vdf
                .number_of_iterations,
            64
        );
        assert_eq!(bundle.reward_chain.end_of_slot_vdf.number_of_iterations, 39);
        assert_eq!(
            bundle
                .infused_challenge_chain
                .unwrap()
                .infused_challenge_chain_end_of_slot_vdf
                .number_of_iterations,
            39
        );
        assert!(
            bundle
                .challenge_chain
                .infused_challenge_chain_sub_slot_hash
                .is_none()
        );
        assert!(
            bundle
                .reward_chain
                .infused_challenge_chain_sub_slot_hash
                .is_some()
        );
        assert_eq!(bundle.reward_chain.deficit, 15);
        assert!(scheduler.state.as_ref().unwrap().infused.is_some());
        assert!(!scheduler.set_peak(peak).unwrap());
        assert_eq!(scheduler.state.as_ref().unwrap().slot_start, 64);
    }

    #[test]
    fn new_peaks_cancel_old_generation_and_reject_invalid_backend_proof() {
        let mut scheduler = scheduler();
        let stale = scheduler.next_plan().unwrap().unwrap();
        let stale_result = prove_plan(&stale);
        candidate(&mut scheduler, 5, false);
        let Output::InfusionPoint(point) = advance(&mut scheduler) else {
            panic!("expected infusion")
        };
        scheduler.set_peak(peak(&scheduler, &point, 15)).unwrap();
        assert!(scheduler.finish(stale, stale_result).unwrap().is_none());
        let plan = scheduler.next_plan().unwrap().unwrap();
        let mut result = prove_plan(&plan);
        result.challenge.info.number_of_iterations += 1;
        assert!(scheduler.finish(plan, result).is_err());
    }

    #[test]
    fn overflow_survives_slot_boundary_but_not_epoch_transition() {
        let mut scheduler = scheduler();
        let hash = candidate(&mut scheduler, 65, true);
        for _ in 0..8 {
            advance(&mut scheduler);
        }
        assert!(scheduler.candidates.contains_key(&hash.const_bytes()));
        assert_eq!(
            scheduler.next_plan().unwrap().unwrap().event,
            Event::InfusionPoint(hash)
        );
        scheduler.state.as_mut().unwrap().new_epoch = true;
        scheduler.prune_candidates();
        assert!(!scheduler.candidates.contains_key(&hash.const_bytes()));
    }

    #[test]
    fn first_epoch_slot_keeps_overflow_candidates_for_the_following_slot() {
        let mut scheduler = scheduler();
        scheduler.state.as_mut().unwrap().new_epoch = true;
        let future = candidate(&mut scheduler, 65, true);
        let current = candidate(&mut scheduler, 5, true);
        scheduler.prune_candidates();
        assert!(scheduler.candidates.contains_key(&future.const_bytes()));
        assert!(!scheduler.candidates.contains_key(&current.const_bytes()));
        candidate(&mut scheduler, 6, false);
        let Output::InfusionPoint(point) = advance(&mut scheduler) else {
            panic!("expected infusion")
        };
        scheduler.set_peak(peak(&scheduler, &point, 15)).unwrap();
        assert!(scheduler.state.as_ref().unwrap().new_epoch);
        assert!(scheduler.candidates.contains_key(&future.const_bytes()));
    }

    #[test]
    fn first_peak_after_an_empty_slot_keeps_genesis_reward_history() {
        let mut scheduler = scheduler();
        for _ in 0..8 {
            advance(&mut scheduler);
        }
        candidate(&mut scheduler, 65, true);
        let Output::InfusionPoint(point) = advance(&mut scheduler) else {
            panic!("expected infusion")
        };
        let mut message = peak(&scheduler, &point, 15);
        message
            .previous_reward_challenges
            .retain(|(_, total)| *total != 0);
        scheduler.set_peak(message).unwrap();
        assert!(scheduler.reward_matches(scheduler.constants.genesis_challenge, 56));
    }

    #[test]
    fn epoch_summary_resets_deficit_and_changes_future_work() {
        let mut scheduler = scheduler();
        candidate(&mut scheduler, 5, false);
        let Output::InfusionPoint(point) = advance(&mut scheduler) else {
            panic!("expected infusion")
        };
        scheduler.set_peak(peak(&scheduler, &point, 15)).unwrap();
        let state = scheduler.state.as_mut().unwrap();
        state.deficit = 0;
        state.passed_summary_height = true;
        state.summary = Some(SubEpochSummary {
            prev_subepoch_summary_hash: Bytes32::from([1; 32]),
            reward_chain_hash: Bytes32::from([2; 32]),
            num_blocks_overflow: 0,
            new_difficulty: Some(3),
            new_sub_slot_iters: Some(128),
        });
        for index in 1..8 {
            scheduler.signage.insert(index);
        }
        let Output::EndOfSubSlot(slot) = advance(&mut scheduler) else {
            panic!("expected slot")
        };
        let bundle = slot.end_of_sub_slot_bundle;
        assert_eq!(bundle.reward_chain.deficit, 16);
        assert!(bundle.challenge_chain.subepoch_summary_hash.is_some());
        assert!(
            bundle
                .challenge_chain
                .infused_challenge_chain_sub_slot_hash
                .is_some()
        );
        assert_eq!(bundle.challenge_chain.new_difficulty, Some(3));
        assert_eq!(bundle.challenge_chain.new_sub_slot_iters, Some(128));
        let state = scheduler.state.as_ref().unwrap();
        assert!(state.new_epoch);
        assert!(!state.passed_summary_height);
        assert!(state.infused.is_none());
        assert_eq!(
            scheduler.next_plan().unwrap().unwrap().challenge.iterations,
            16
        );
    }

    #[test]
    fn pacing_applies_to_signage_infusions_and_slot_ends() {
        let mut scheduler = Scheduler::new(constants(), true, Some(2)).unwrap();
        scheduler.set_genesis(genesis(&constants())).unwrap();
        let anchor = scheduler.state.as_ref().unwrap().anchor;
        let first = scheduler.next_plan().unwrap().unwrap();
        assert_eq!(
            first.not_before.duration_since(anchor),
            Duration::from_secs(4)
        );
        candidate(&mut scheduler, 5, false);
        let infusion = scheduler.next_plan().unwrap().unwrap();
        assert_eq!(
            infusion.not_before.duration_since(anchor),
            Duration::from_millis(2500)
        );
        scheduler.candidates.clear();
        for index in 1..8 {
            scheduler.signage.insert(index);
        }
        assert_eq!(
            scheduler
                .next_plan()
                .unwrap()
                .unwrap()
                .not_before
                .duration_since(anchor),
            Duration::from_secs(32)
        );
        assert!(Scheduler::new(constants(), true, Some(0)).is_err());
    }

    #[test]
    fn caller_cannot_remove_a_pacing_deadline() {
        let mut scheduler = Scheduler::new(constants(), true, Some(1)).unwrap();
        scheduler.set_genesis(genesis(&constants())).unwrap();
        let mut plan = scheduler.next_plan().unwrap().unwrap();
        let result = prove_plan(&plan);
        plan.not_before = Instant::now();
        assert_eq!(
            scheduler.finish(plan, result).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    }
}
