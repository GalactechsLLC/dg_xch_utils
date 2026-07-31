// Validation for Chia weight proofs.

use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::weight_proof::{
    SubEpochChallengeSegment, SubEpochData, SubSlotData, WeightProof,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality, calculate_sp_iters, is_overflow_block,
};
use dg_xch_core::utils::hash_256;
use dg_xch_pos::verify_and_get_quality_string;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_vdf::{default_classgroup_element, validate_vdf_info};

/// Limits validation work for untrusted proofs.
pub const MAX_SUB_EPOCHS: usize = 300_000;
pub const MAX_SEGMENTS: usize = 20_000;
pub const MAX_RECENT_BLOCKS: usize = 2_000;

// Bounds attacker-controlled segment and VDF counts.
const MAX_SUB_SLOTS_PER_SEGMENT: usize = 10_000;
const MAX_VDFS_TO_VERIFY: usize = 200_000;

// Sampling constants match Chia's `WeightProofHandler`.
const SAMPLING_LAMBDA_L: f64 = 100.0; // security parameter (`LAMBDA_L`)
const SAMPLING_C: f64 = 0.5; // adversary-advantage base (`C`)
const MAX_SAMPLES: usize = 20; // cap on distinct sampled sub-epochs
/// DoS guard on the sampling-query loop: a hostile proof with a near-zero weight span drives the
/// reference's `int(queries)+1` unbounded. A legitimate proof needs only a handful of queries (tens),
/// so this cap is orders of magnitude above any real value — it never triggers on a valid proof (no
/// divergence from the reference) but stops an adversary from forcing unbounded RNG draws.
const MAX_SAMPLING_QUERIES: i64 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeightProofError {
    /// A phase is not yet ported. Fail-closed: treated as "invalid", never "valid".
    PhaseUnimplemented(&'static str),
    /// The proof is structurally malformed or empty.
    Malformed(&'static str),
    /// The proof exceeds a safety bound (DoS guard).
    TooLarge(&'static str),
    /// A specific validation phase rejected the proof.
    Rejected(&'static str),
}

/// Validate a weight proof against `constants`, returning `(valid, summaries)` on success — mirroring
/// the reference's `(bool, list[SubEpochSummary])`. Pure: no network, trusts no peer.
///
/// The six phases (port map to the reference):
/// 1. sampling            ← `validate_sub_epoch_sampling` / `_get_weights_for_sampling` / `_sample_sub_epoch`
/// 2. sub-epoch summaries ← `_validate_sub_epoch_summaries`
/// 3. summaries weight    ← `_validate_summaries_weight`
/// 4. sampled segments    ← `_validate_sub_epoch_segments` / `_validate_segment` / `__validate_pospace`
/// 5. recent blocks       ← `validate_recent_blocks` / `_validate_pospace_recent_chain`
/// 6. total weight & peak  ← folded into phase 3 (`_validate_summaries_weight`); the reference has no
///    further standalone total-weight check, so phase 6 is a documented no-op.
///
/// All six phases are ported and verified against a real mainnet weight proof: this returns
/// `Ok((true, summaries))` on a valid proof and fails closed (`Err`) on any structural, weight, VDF,
/// PoSpace, signature, or reconstruction violation.
pub fn validate_weight_proof(
    wp: &WeightProof,
    constants: &ConsensusConstants,
) -> Result<(bool, Vec<SubEpochSummary>), WeightProofError> {
    validate_weight_proof_with_progress(wp, constants, &mut |_| {})
}

fn validate_weight_proof_with_progress<F>(
    wp: &WeightProof,
    constants: &ConsensusConstants,
    progress: &mut F,
) -> Result<(bool, Vec<SubEpochSummary>), WeightProofError>
where
    F: FnMut(&'static str),
{
    // --- Cheap structural + bounds gate (implemented; consensus-safe to check up front) ---
    if wp.sub_epochs.is_empty() {
        return Err(WeightProofError::Malformed("no sub-epochs"));
    }
    if wp.sub_epochs.len() > MAX_SUB_EPOCHS {
        return Err(WeightProofError::TooLarge("sub_epochs"));
    }
    if wp.sub_epoch_segments.len() > MAX_SEGMENTS {
        return Err(WeightProofError::TooLarge("sub_epoch_segments"));
    }
    validate_segment_order(&wp.sub_epoch_segments)?;
    if wp.recent_chain_data.is_empty() {
        return Err(WeightProofError::Malformed("no recent chain data"));
    }
    if wp.recent_chain_data.len() > MAX_RECENT_BLOCKS {
        return Err(WeightProofError::TooLarge("recent_chain_data"));
    }

    // --- The six phases, in order. Each is fail-closed until ported + verified. ---
    // Phase 2 also yields the total accumulated weight and the per-sub-epoch cumulative weight list,
    // consumed by phase 3 (total) and phase 1 (list).
    progress("phase 2: validating sub-epoch summaries");
    let (summaries, total_weight, sub_epoch_weight_list) =
        validate_sub_epoch_summaries(wp, constants)?;
    progress("phase 2: complete");

    progress("phase 1: validating sub-epoch sampling");
    validate_sub_epoch_sampling(wp, &summaries, &sub_epoch_weight_list, constants)?;
    progress("phase 1: complete");

    progress("phase 3: validating summaries weight");
    validate_summaries_weight(wp, &summaries, total_weight, constants)?;
    progress("phase 3: complete");

    progress("phase 4: validating sampled segments");
    validate_sub_epoch_segments(wp, &summaries, constants)?;
    progress("phase 4: complete");

    progress("phase 5: validating recent blocks");
    validate_recent_blocks(wp, &summaries, constants)?;
    progress("phase 5: complete");

    progress("phase 6: validating total weight");
    validate_total_weight(wp, &summaries, constants)?;
    progress("phase 6: complete");

    Ok((true, summaries))
}

/// The consensus hash of a summary: `std_hash(bytes(ses))` in the reference. `SubEpochSummary` is a
/// blockchain (not network) type, so its streamable encoding — hence this hash — is independent of the
/// negotiated protocol version.
fn ses_hash(ses: &SubEpochSummary) -> Result<Bytes32, WeightProofError> {
    let bytes = ses
        .to_bytes(ChiaProtocolVersion::default())
        .map_err(|_| WeightProofError::Malformed("sub_epoch_summary serialization"))?;
    Ok(Bytes32::from(hash_256(bytes)))
}

/// Find the last on-chain sub-epoch-summary hash (and its block height) in the recent chain: walk from
/// the tip back to the first block sitting on a sub-epoch boundary, then scan forward from there for the
/// first finished sub-slot that carries a `subepoch_summary_hash`. That hash is the commitment the
/// reconstructed summary chain must terminate in. (ref: `_get_last_ses_hash`.)
fn get_last_ses_hash(
    sub_epoch_blocks: u32,
    recent_chain: &[HeaderBlock],
) -> Option<(Bytes32, u32)> {
    for boundary_idx in (0..recent_chain.len()).rev() {
        let block = &recent_chain[boundary_idx];
        if block
            .reward_chain_block
            .height
            .is_multiple_of(sub_epoch_blocks)
        {
            for curr in &recent_chain[boundary_idx..] {
                for slot in &curr.finished_sub_slots {
                    if let Some(hash) = slot.challenge_chain.subepoch_summary_hash {
                        return Some((hash, curr.reward_chain_block.height));
                    }
                }
            }
        }
    }
    None
}

/// Reconstruct the sub-epoch-summary hash-chain from the proof's `SubEpochData`, genesis-anchored, and
/// accumulate the implied sub-epoch weights. Returns `(summaries, total_weight, sub_epoch_weight_list)`;
/// phase 2 uses `summaries`, phases 3/6 consume the weights. Pure hashing + integer weight arithmetic —
/// no VDF, no proof-of-space. (ref: `_map_sub_epoch_summaries`.)
///
/// Each summary links the previous by hash (`prev_subepoch_summary_hash`), so the chain is genesis-rooted
/// and tamper-evident: change any `SubEpochData` field and every downstream hash — including the last —
/// changes with it. `curr_difficulty` follows the proof's declared `new_difficulty`; it is not re-derived
/// here (the light-client reference does not re-run difficulty adjustment) — it is pinned instead by the
/// last-summary hash anchor in [`validate_sub_epoch_summaries`] and the weight check in phase 3.
fn map_sub_epoch_summaries(
    sub_blocks_for_se: u32,
    genesis_challenge: Bytes32,
    sub_epoch_data: &[SubEpochData],
    difficulty_starting: u64,
) -> Result<(Vec<SubEpochSummary>, u128, Vec<u128>), WeightProofError> {
    let mut prev_ses_hash = genesis_challenge;
    let mut curr_difficulty: u128 = difficulty_starting as u128;
    let mut total_weight: u128 = 0;
    let n = sub_epoch_data.len();
    let mut summaries: Vec<SubEpochSummary> = Vec::with_capacity(n);
    let mut sub_epoch_weight_list: Vec<u128> = Vec::with_capacity(n + 1);

    for (idx, data) in sub_epoch_data.iter().enumerate() {
        let ses = SubEpochSummary {
            prev_subepoch_summary_hash: prev_ses_hash,
            reward_chain_hash: data.reward_chain_hash,
            num_blocks_overflow: data.num_blocks_overflow,
            new_difficulty: data.new_difficulty,
            new_sub_slot_iters: data.new_sub_slot_iters,
        };

        if idx + 1 < n {
            // Blocks credited to this sub-epoch: the fixed sub-epoch length, plus the *next* sub-epoch's
            // overflow blocks (which belong to this challenge), minus this sub-epoch's own overflow
            // (`delta`, from idx 1 on) so overflow blocks are counted exactly once across the boundary.
            // For u8 overflow fields `sub_blocks_for_se (384) + next >= delta` always, so the subtraction
            // cannot underflow; `saturating_sub` makes that DoS-safe without diverging on valid input.
            let delta = if idx > 0 {
                u128::from(data.num_blocks_overflow)
            } else {
                0
            };
            sub_epoch_weight_list.push(total_weight + curr_difficulty);
            let next_overflow = u128::from(sub_epoch_data[idx + 1].num_blocks_overflow);
            let blocks = (u128::from(sub_blocks_for_se) + next_overflow).saturating_sub(delta);
            total_weight += curr_difficulty * blocks;
        }

        // A new epoch declares a new difficulty; subsequent sub-epochs accrue weight at that rate.
        if let Some(new_difficulty) = data.new_difficulty {
            curr_difficulty = u128::from(new_difficulty);
        }

        prev_ses_hash = ses_hash(&ses)?;
        summaries.push(ses);
    }
    // The final sub-epoch's start weight closes the list (mirrors the reference's trailing append).
    sub_epoch_weight_list.push(total_weight + curr_difficulty);

    Ok((summaries, total_weight, sub_epoch_weight_list))
}

/// Phase 2 — reconstruct the [`SubEpochSummary`] chain from the proof's `SubEpochData` and prove it
/// terminates in the sub-epoch-summary hash actually committed on-chain (in the recent chain's finished
/// sub-slots). Pure hashing/weight arithmetic; no VDF/PoSpace. Produces the summaries the later phases
/// consume. (ref: `_validate_sub_epoch_summaries`.)
///
/// The last-hash equality IS the anti-grinding control: because each summary hashes the previous, a
/// prover who tampers with any `SubEpochData` (difficulty, sub-slot iters, overflow, reward-chain hash)
/// produces a different final hash, which cannot match the on-chain commitment — so a re-weighted or
/// forged history is rejected here without re-deriving difficulty. The light-client surface deliberately
/// carries no independent `difficulty_change_max_factor`/`significant_bits` clamp; the hash anchor (plus
/// phase-3 weight and phase-4 sampled-segment checks) is what pins the declared difficulties.
fn validate_sub_epoch_summaries(
    wp: &WeightProof,
    c: &ConsensusConstants,
) -> Result<(Vec<SubEpochSummary>, u128, Vec<u128>), WeightProofError> {
    let (last_ses_hash, _last_ses_height) =
        get_last_ses_hash(c.sub_epoch_blocks, &wp.recent_chain_data).ok_or(
            WeightProofError::Rejected("no sub-epoch-summary hash in recent chain"),
        )?;

    let (summaries, total_weight, sub_epoch_weight_list) = map_sub_epoch_summaries(
        c.sub_epoch_blocks,
        c.genesis_challenge,
        &wp.sub_epochs,
        c.difficulty_starting,
    )?;

    let last = summaries
        .last()
        .ok_or(WeightProofError::Malformed("no sub-epochs to summarize"))?;
    if ses_hash(last)? != last_ses_hash {
        return Err(WeightProofError::Rejected(
            "last sub-epoch-summary hash mismatch",
        ));
    }

    Ok((summaries, total_weight, sub_epoch_weight_list))
}

/// A byte-exact reimplementation of CPython's `random.Random` (MT19937) — the sampling determinism is
/// the security property, so the prover and verifier must draw the identical sequence. Seeding follows
/// `random.seed(bytes, version=2)`: `int.from_bytes(seed || sha512(seed), "big")`, then the C
/// `init_by_array` over that integer's little-endian 32-bit words; `random()` is the 53-bit double the
/// C `random_random` produces. Verified against CPython reference vectors in the tests below.
mod recent_blocks;

mod py_random {
    use sha2::{Digest, Sha512};

    const N: usize = 624;
    const M: usize = 397;
    const MATRIX_A: u32 = 0x9908_b0df;
    const UPPER_MASK: u32 = 0x8000_0000;
    const LOWER_MASK: u32 = 0x7fff_ffff;

    pub struct PyRandom {
        mt: [u32; N],
        mti: usize,
    }

    impl PyRandom {
        /// Seed exactly as `random.Random(seed)` for a `bytes` seed.
        pub fn new(seed: &[u8]) -> Self {
            let mut buf = seed.to_vec();
            buf.extend_from_slice(&Sha512::digest(seed));
            let key = be_bytes_to_le_words(&buf);
            let mut r = PyRandom {
                mt: [0u32; N],
                mti: N + 1,
            };
            r.init_by_array(&key);
            r
        }

        fn init_genrand(&mut self, s: u32) {
            self.mt[0] = s;
            for i in 1..N {
                self.mt[i] = 1_812_433_253u32
                    .wrapping_mul(self.mt[i - 1] ^ (self.mt[i - 1] >> 30))
                    .wrapping_add(i as u32);
            }
            self.mti = N;
        }

        fn init_by_array(&mut self, key: &[u32]) {
            self.init_genrand(19_650_218);
            let mut i = 1usize;
            let mut j = 0usize;
            let mut k = N.max(key.len());
            while k > 0 {
                self.mt[i] = (self.mt[i]
                    ^ ((self.mt[i - 1] ^ (self.mt[i - 1] >> 30)).wrapping_mul(1_664_525)))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
                i += 1;
                j += 1;
                if i >= N {
                    self.mt[0] = self.mt[N - 1];
                    i = 1;
                }
                if j >= key.len() {
                    j = 0;
                }
                k -= 1;
            }
            k = N - 1;
            while k > 0 {
                self.mt[i] = (self.mt[i]
                    ^ ((self.mt[i - 1] ^ (self.mt[i - 1] >> 30)).wrapping_mul(1_566_083_941)))
                .wrapping_sub(i as u32);
                i += 1;
                if i >= N {
                    self.mt[0] = self.mt[N - 1];
                    i = 1;
                }
                k -= 1;
            }
            self.mt[0] = UPPER_MASK;
        }

        fn genrand_u32(&mut self) -> u32 {
            if self.mti >= N {
                for kk in 0..(N - M) {
                    let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                    self.mt[kk] = self.mt[kk + M] ^ (y >> 1) ^ ((y & 1) * MATRIX_A);
                }
                for kk in (N - M)..(N - 1) {
                    let y = (self.mt[kk] & UPPER_MASK) | (self.mt[kk + 1] & LOWER_MASK);
                    self.mt[kk] = self.mt[kk + M - N] ^ (y >> 1) ^ ((y & 1) * MATRIX_A);
                }
                let y = (self.mt[N - 1] & UPPER_MASK) | (self.mt[0] & LOWER_MASK);
                self.mt[N - 1] = self.mt[M - 1] ^ (y >> 1) ^ ((y & 1) * MATRIX_A);
                self.mti = 0;
            }
            let mut y = self.mt[self.mti];
            self.mti += 1;
            y ^= y >> 11;
            y ^= (y << 7) & 0x9d2c_5680;
            y ^= (y << 15) & 0xefc6_0000;
            y ^= y >> 18;
            y
        }

        /// The `random()` double: `(a*2^26 + b) / 2^53`, `a = next>>5` (27 bits), `b = next>>6` (26 bits).
        pub fn random(&mut self) -> f64 {
            let a = (self.genrand_u32() >> 5) as f64;
            let b = (self.genrand_u32() >> 6) as f64;
            (a * 67_108_864.0 + b) * (1.0 / 9_007_199_254_740_992.0)
        }

        /// CPython's `getrandbits(k)` (`random_getrandbits`): little-endian 32-bit words, the top word
        /// shifted right to keep exactly `k` bits. Phase 4 only ever needs `k <= 64` (segment counts), so
        /// the two-word path suffices; wider `k` is not used here.
        pub fn getrandbits(&mut self, k: u32) -> u64 {
            debug_assert!((1..=64).contains(&k));
            if k <= 32 {
                return u64::from(self.genrand_u32() >> (32 - k));
            }
            let low = u64::from(self.genrand_u32());
            let hi_bits = k - 32;
            let hi = u64::from(self.genrand_u32() >> (32 - hi_bits));
            low | (hi << 32)
        }

        /// CPython's `Random._randbelow_with_getrandbits(n)`: uniform in `[0, n)` by rejection sampling
        /// over `getrandbits(n.bit_length())`. `random.choice(range(n))` is exactly this.
        pub fn randbelow(&mut self, n: u64) -> u64 {
            if n == 0 {
                return 0;
            }
            let k = 64 - n.leading_zeros(); // n.bit_length() for n > 0
            loop {
                let r = self.getrandbits(k);
                if r < n {
                    return r;
                }
            }
        }
    }

    /// A big-endian byte string → the little-endian 32-bit word array CPython feeds to `init_by_array`
    /// (the abs value's 32-bit digits, low word first, high zero words dropped per its true bit length).
    fn be_bytes_to_le_words(be: &[u8]) -> Vec<u32> {
        let Some(first_nonzero) = be.iter().position(|&b| b != 0) else {
            return vec![0]; // value 0 → CPython uses a single 0 word
        };
        let sig = &be[first_nonzero..];
        let bits = (sig.len() - 1) * 8 + (8 - sig[0].leading_zeros() as usize);
        let key_words = bits.div_ceil(32).max(1);
        let mut le: Vec<u8> = be.iter().rev().copied().collect();
        le.resize(key_words * 4, 0);
        (0..key_words)
            .map(|w| u32::from_le_bytes([le[w * 4], le[w * 4 + 1], le[w * 4 + 2], le[w * 4 + 3]]))
            .collect()
    }
}

/// Draw the sampling weights (`_get_weights_for_sampling`): a set of target weights, one per RNG query,
/// where the query count is derived from how much of the chain the recent window covers. `Ok(None)`
/// means "sample every sub-epoch" (the reference returns `None` when the adversary-success probability is
/// non-positive). The float math mirrors the reference operation-for-operation so the draw count and the
/// resulting set match. (ref: `_get_weights_for_sampling`.)
fn get_weights_for_sampling(
    rng: &mut py_random::PyRandom,
    total_weight: u128,
    recent_chain: &[HeaderBlock],
) -> Result<Option<Vec<u128>>, WeightProofError> {
    let (Some(last), Some(first)) = (recent_chain.last(), recent_chain.first()) else {
        return Err(WeightProofError::Malformed(
            "empty recent chain in sampling",
        ));
    };
    if total_weight == 0 {
        return Err(WeightProofError::Rejected("zero total weight in sampling"));
    }
    // A valid recent chain has non-decreasing weight; a hostile one that inverts it would underflow the
    // u128 subtraction (the Python reference would raise on the resulting negative log). Reject instead.
    let last_l_weight = last
        .reward_chain_block
        .weight
        .checked_sub(first.reward_chain_block.weight)
        .ok_or(WeightProofError::Rejected("recent chain weight decreases"))?;
    let delta = last_l_weight as f64 / total_weight as f64;
    // prob = 1 - log_delta(C) = 1 - ln(C)/ln(delta)
    let prob_of_adv_succeeding = 1.0 - (SAMPLING_C.ln() / delta.ln());
    if prob_of_adv_succeeding <= 0.0 {
        return Ok(None);
    }
    // queries = -LAMBDA_L * log_prob(2) = -LAMBDA_L * ln(2)/ln(prob); int() truncates toward zero.
    let queries = -SAMPLING_LAMBDA_L * (core::f64::consts::LN_2 / prob_of_adv_succeeding.ln());
    let count = queries as i64;
    if !(0..=MAX_SAMPLING_QUERIES).contains(&count) {
        return Err(WeightProofError::TooLarge("sampling queries"));
    }
    let mut weight_to_check = Vec::with_capacity(count as usize + 1);
    for _ in 0..=count {
        let u = rng.random();
        let q = 1.0 - delta.powf(u);
        let weight = q * total_weight as f64;
        weight_to_check.push(weight as u128);
    }
    weight_to_check.sort_unstable();
    Ok(Some(weight_to_check))
}

/// Does the sampling hit this sub-epoch's `[start, end)` weight band? `weight_to_check` is sorted; `None`
/// means sample-everything. (ref: `_sample_sub_epoch`.)
fn sample_sub_epoch(start: u128, end: u128, weight_to_check: Option<&[u128]>) -> bool {
    let Some(wtc) = weight_to_check else {
        return true;
    };
    if wtc.is_empty() {
        return false;
    }
    if *wtc.last().unwrap() < start {
        return false;
    }
    if wtc[0] > end {
        return false;
    }
    for &w in wtc {
        if w > end {
            return false;
        }
        if start < w && w < end {
            return true;
        }
    }
    false
}

/// Phase 1 — the sampled sub-epochs are exactly those the seed-derived RNG (seeded by the
/// second-to-last summary hash) selects, and every sampled sub-epoch is backed by challenge segments in
/// the proof. A prover therefore cannot prove a cherry-picked subset: the seed is fixed by the summary
/// chain (phase 2), so the sample set is not the prover's to choose, and any sampled sub-epoch missing
/// its segments is rejected. (ref: `validate_sub_epoch_sampling`, `_get_weights_for_sampling`,
/// `_sample_sub_epoch`.)
fn validate_sub_epoch_sampling(
    wp: &WeightProof,
    summaries: &[SubEpochSummary],
    sub_epoch_weight_list: &[u128],
    _c: &ConsensusConstants,
) -> Result<(), WeightProofError> {
    if summaries.len() < 2 {
        return Err(WeightProofError::Rejected(
            "fewer than two sub-epoch summaries",
        ));
    }
    // Seed = the second-to-last summary's hash. Fixed by the (already-anchored) summary chain, so the
    // prover cannot steer which sub-epochs get sampled.
    let seed = ses_hash(&summaries[summaries.len() - 2])?;
    let mut rng = py_random::PyRandom::new(seed.as_ref());

    let tip = wp
        .recent_chain_data
        .last()
        .ok_or(WeightProofError::Malformed("no recent chain data"))?;
    let weight_to_check = get_weights_for_sampling(
        &mut rng,
        tip.reward_chain_block.weight,
        &wp.recent_chain_data,
    )?;

    // The sub-epochs the RNG selects (by index `idx-1`, matching the reference).
    let mut sampled: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for idx in 1..sub_epoch_weight_list.len() {
        if sample_sub_epoch(
            sub_epoch_weight_list[idx - 1],
            sub_epoch_weight_list[idx],
            weight_to_check.as_deref(),
        ) {
            sampled.insert((idx - 1) as u32);
            if sampled.len() == MAX_SAMPLES {
                break;
            }
        }
    }

    // Every sampled sub-epoch must be covered by challenge segments in the proof; strike off each one
    // whose segments are present. Anything left un-struck is a sampled sub-epoch with no segments.
    let mut curr_sub_epoch_n: i64 = -1;
    for segment in &wp.sub_epoch_segments {
        let n = segment.sub_epoch_n;
        if curr_sub_epoch_n < i64::from(n) {
            sampled.remove(&n);
        }
        curr_sub_epoch_n = i64::from(n);
    }

    if !sampled.is_empty() {
        return Err(WeightProofError::Rejected(
            "a sampled sub-epoch is not covered by challenge segments",
        ));
    }
    Ok(())
}

/// Phase 3 — the weight accumulated across the summaries (phase 2's `total_weight`) must equal the weight
/// the recent chain actually reports at the last sub-epoch boundary. This ties the succinct summary chain
/// to a concrete, on-chain weight: a prover cannot inflate or deflate the accrued weight without
/// contradicting the recent block at `ses_end_height`. (ref: `_validate_summaries_weight`.)
///
/// `ses_end_height = (len(summaries) - 1) * SUB_EPOCH_BLOCKS + num_blocks_overflow(last) - 1`, computed in
/// signed space: for a degenerate proof it can go negative (e.g. one summary, zero overflow), which simply
/// matches no block — the reference reaches the same `curr is None → False`. No panics on malformed input;
/// the scan is bounded by `recent_chain_data` (already capped by `MAX_RECENT_BLOCKS`).
fn validate_summaries_weight(
    wp: &WeightProof,
    summaries: &[SubEpochSummary],
    total_weight: u128,
    c: &ConsensusConstants,
) -> Result<(), WeightProofError> {
    let num_over = summaries
        .last()
        .ok_or(WeightProofError::Malformed("no summaries for weight check"))?
        .num_blocks_overflow;
    // Signed to mirror Python's integer arithmetic (a negative height just never matches a block).
    let ses_end_height: i64 =
        (summaries.len() as i64 - 1) * i64::from(c.sub_epoch_blocks) + i64::from(num_over) - 1;

    // The reference scans the whole recent chain and keeps the LAST block at that height (heights are
    // unique, so this is the block at the sub-epoch boundary). No match → reject.
    let mut curr: Option<&HeaderBlock> = None;
    for block in &wp.recent_chain_data {
        if i64::from(block.reward_chain_block.height) == ses_end_height {
            curr = Some(block);
        }
    }
    let curr = curr.ok_or(WeightProofError::Rejected(
        "no recent block at the sub-epoch end height",
    ))?;

    if curr.reward_chain_block.weight != total_weight {
        return Err(WeightProofError::Rejected(
            "summaries weight does not match the recent chain",
        ));
    }
    Ok(())
}

/// The consensus hash of any streamable blockchain value: `std_hash(bytes(x))`.
pub(crate) fn hash_of<T: ChiaSerialize>(x: &T) -> Result<Bytes32, WeightProofError> {
    let bytes = x
        .to_bytes(ChiaProtocolVersion::default())
        .map_err(|_| WeightProofError::Malformed("serialize"))?;
    Ok(Bytes32::from(hash_256(bytes)))
}

// `SubSlotData` predicates: a challenge block carries a proof of space; an end-of-slot
// entry carries a challenge-chain slot-end proof.
fn ss_is_challenge(s: &SubSlotData) -> bool {
    s.proof_of_space.is_some()
}
fn ss_is_end_of_slot(s: &SubSlotData) -> bool {
    s.cc_slot_end.is_some()
}

/// Verify one collected VDF against its reconstructed input — this is `dg_xch_vdf::validate_vdf_info`,
/// i.e. chia's `validate_vdf` (target check → witness-size check → Wesolowski `verify_vdf`). Counts toward
/// the DoS bound so a hostile proof cannot force unbounded VDF work.
fn check_vdf(
    c: &ConsensusConstants,
    input: &ClassgroupElement,
    info: &VdfInfo,
    proof: &dg_xch_core::blockchain::vdf_proof::VdfProof,
    count: &mut usize,
) -> Result<(), WeightProofError> {
    *count += 1;
    if *count > MAX_VDFS_TO_VERIFY {
        return Err(WeightProofError::TooLarge("vdf count"));
    }
    if validate_vdf_info(c, input, info, proof, None) {
        Ok(())
    } else {
        Err(WeightProofError::Rejected("invalid VDF"))
    }
}

/// `_max_sub_epoch_segments`: the per-sub-epoch segment ceiling implied by the sub-epoch length.
fn max_sub_epoch_segments(c: &ConsensusConstants) -> usize {
    let mbpcb = u32::from(c.min_blocks_per_challenge_block);
    let max_blocks = c.sub_epoch_blocks + mbpcb - 1;
    ((max_blocks - 1) / mbpcb + 1) as usize
}

/// Rejects segments that are not ordered by `sub_epoch_n`.
fn validate_segment_order(segments: &[SubEpochChallengeSegment]) -> Result<(), WeightProofError> {
    if segments
        .windows(2)
        .any(|pair| pair[0].sub_epoch_n > pair[1].sub_epoch_n)
    {
        return Err(WeightProofError::Malformed(
            "sub_epoch_segments are not ordered by sub_epoch_n",
        ));
    }
    Ok(())
}

/// Groups ordered segments by `sub_epoch_n`.
fn map_segments_by_sub_epoch(
    segments: &[SubEpochChallengeSegment],
) -> Vec<(u32, Vec<&SubEpochChallengeSegment>)> {
    let mut out: Vec<(u32, Vec<&SubEpochChallengeSegment>)> = Vec::new();
    let mut curr: i64 = -1;
    for seg in segments {
        if curr < i64::from(seg.sub_epoch_n) {
            curr = i64::from(seg.sub_epoch_n);
            out.push((seg.sub_epoch_n, Vec::new()));
        }
        out.last_mut().unwrap().1.push(seg);
    }
    out
}

/// The current difficulty and sub-slot-iters entering sub-epoch `idx`: the most recent prior summary that
/// declared new values, else the starting constants. (ref: `_get_curr_diff_ssi`.)
fn get_curr_diff_ssi(
    c: &ConsensusConstants,
    idx: usize,
    summaries: &[SubEpochSummary],
) -> (u64, u64) {
    let mut curr_difficulty = c.difficulty_starting;
    let mut curr_ssi = c.sub_slot_iters_starting;
    let upto = idx.min(summaries.len());
    for ses in summaries[0..upto].iter().rev() {
        if let Some(ssi) = ses.new_sub_slot_iters {
            curr_ssi = ssi;
            if let Some(d) = ses.new_difficulty {
                curr_difficulty = d;
            }
            break;
        }
    }
    (curr_difficulty, curr_ssi)
}

/// `get_sp_total_iters`: the total iters at this block's signage point.
fn get_sp_total_iters(
    c: &ConsensusConstants,
    is_overflow: bool,
    ssi: u64,
    ssd: &SubSlotData,
) -> Result<u128, WeightProofError> {
    let cc_ip = ssd
        .cc_ip_vdf_info
        .as_ref()
        .ok_or(WeightProofError::Rejected("cc_ip_vdf_info"))?;
    let total_iters = ssd
        .total_iters
        .ok_or(WeightProofError::Rejected("total_iters"))?;
    let sp_index = ssd
        .signage_point_index
        .ok_or(WeightProofError::Rejected("signage_point_index"))?;
    let sp_iters = u128::from(
        calculate_sp_iters(c, ssi, sp_index).map_err(|_| WeightProofError::Rejected("sp_iters"))?,
    );
    let ip_iters = u128::from(cc_ip.number_of_iterations);
    let mut sp_sub_slot_total_iters = total_iters
        .checked_sub(ip_iters)
        .ok_or(WeightProofError::Rejected("sp_sub_slot_total_iters"))?;
    if is_overflow {
        sp_sub_slot_total_iters = sp_sub_slot_total_iters.checked_sub(u128::from(ssi)).ok_or(
            WeightProofError::Rejected("sp_sub_slot_total_iters overflow"),
        )?;
    }
    Ok(sp_sub_slot_total_iters + sp_iters)
}

/// Reconstruct the challenge-chain VDF *input* for a signage-point VDF by walking back to the correct
/// prior infusion point. (ref: `sub_slot_data_vdf_input`.)
fn sub_slot_data_vdf_input(
    c: &ConsensusConstants,
    ssd: &SubSlotData,
    sub_slot_idx: usize,
    sub_slots: &[SubSlotData],
    is_overflow: bool,
    new_sub_slot: bool,
    ssi: u64,
) -> Result<ClassgroupElement, WeightProofError> {
    let mut cc_input = default_classgroup_element();
    let sp_total_iters = get_sp_total_iters(c, is_overflow, ssi, ssd)?;

    if is_overflow && new_sub_slot {
        if sub_slot_idx >= 2 && sub_slots[sub_slot_idx - 2].cc_slot_end_info.is_none() {
            let mut sel: Option<&SubSlotData> = None;
            for i in (0..(sub_slot_idx - 1)).rev() {
                let cand = &sub_slots[i];
                sel = Some(cand);
                if cand.cc_slot_end_info.is_some() {
                    sel = Some(&sub_slots[i + 1]);
                    break;
                }
                let ti = cand
                    .total_iters
                    .ok_or(WeightProofError::Rejected("total_iters"))?;
                if ti <= sp_total_iters {
                    break;
                }
            }
            if let Some(sel) = sel
                && let Some(info) = &sel.cc_ip_vdf_info
            {
                let ti = sel
                    .total_iters
                    .ok_or(WeightProofError::Rejected("total_iters"))?;
                if ti < sp_total_iters {
                    cc_input = info.output;
                }
            }
        }
        return Ok(cc_input);
    } else if !is_overflow && !new_sub_slot {
        let mut sel: Option<&SubSlotData> = None;
        for i in (0..sub_slot_idx).rev() {
            let cand = &sub_slots[i];
            sel = Some(cand);
            if cand.cc_slot_end_info.is_some() {
                sel = Some(&sub_slots[i + 1]);
                break;
            }
            let ti = cand
                .total_iters
                .ok_or(WeightProofError::Rejected("total_iters"))?;
            if ti <= sp_total_iters {
                break;
            }
        }
        let sel = sel.ok_or(WeightProofError::Rejected("no sub-slot for vdf input"))?;
        if let Some(info) = &sel.cc_ip_vdf_info {
            let ti = sel
                .total_iters
                .ok_or(WeightProofError::Rejected("total_iters"))?;
            if ti < sp_total_iters {
                cc_input = info.output;
            }
        }
        return Ok(cc_input);
    } else if !new_sub_slot && is_overflow {
        let mut slots_seen = 0;
        let mut sel: Option<&SubSlotData> = None;
        for i in (0..sub_slot_idx).rev() {
            let cand = &sub_slots[i];
            sel = Some(cand);
            if cand.cc_slot_end_info.is_some() {
                slots_seen += 1;
                if slots_seen == 2 {
                    return Ok(default_classgroup_element());
                }
            }
            if cand.cc_slot_end_info.is_none() {
                let ti = cand
                    .total_iters
                    .ok_or(WeightProofError::Rejected("total_iters"))?;
                if ti <= sp_total_iters {
                    break;
                }
            }
        }
        let sel = sel.ok_or(WeightProofError::Rejected("no sub-slot for vdf input"))?;
        if let Some(info) = &sel.cc_ip_vdf_info {
            let ti = sel
                .total_iters
                .ok_or(WeightProofError::Rejected("total_iters"))?;
            if ti < sp_total_iters {
                cc_input = info.output;
            }
        }
    }
    Ok(cc_input)
}

/// Reconstruct the challenge-chain sub-slot at `idx` (for the pospace challenge hash). Walks back to the
/// nearest prior slot-end. (ref: `__get_cc_sub_slot`.)
fn get_cc_sub_slot(
    sub_slots: &[SubSlotData],
    idx: usize,
    ses: Option<&SubEpochSummary>,
) -> Result<ChallengeChainSubSlot, WeightProofError> {
    let mut sub_slot: Option<&SubSlotData> = None;
    for i in (0..idx).rev() {
        sub_slot = Some(&sub_slots[i]);
        if sub_slots[i].cc_slot_end_info.is_some() {
            break;
        }
    }
    let sub_slot = sub_slot.ok_or(WeightProofError::Rejected("no cc sub slot"))?;
    let cc_slot_end_info = sub_slot
        .cc_slot_end_info
        .as_ref()
        .ok_or(WeightProofError::Rejected("cc_slot_end_info"))?;
    let icc_vdf_hash = match &sub_slot.icc_slot_end_info {
        Some(icc) => Some(hash_of(icc)?),
        None => None,
    };
    Ok(ChallengeChainSubSlot {
        challenge_chain_end_of_slot_vdf: *cc_slot_end_info,
        infused_challenge_chain_sub_slot_hash: icc_vdf_hash,
        subepoch_summary_hash: match ses {
            Some(s) => Some(hash_of(s)?),
            None => None,
        },
        new_sub_slot_iters: ses.and_then(|s| s.new_sub_slot_iters),
        new_difficulty: ses.and_then(|s| s.new_difficulty),
    })
}

/// Reconstruct the reward-chain sub-slot for a sub-epoch's first segment, to check its hash against the
/// summary's `reward_chain_hash`. (ref: `__get_rc_sub_slot`.)
fn get_rc_sub_slot(
    c: &ConsensusConstants,
    segment: &SubEpochChallengeSegment,
    summaries: &[SubEpochSummary],
    curr_ssi: u64,
) -> Result<Option<RewardChainSubSlot>, WeightProofError> {
    let se_idx = (segment.sub_epoch_n as usize)
        .checked_sub(1)
        .ok_or(WeightProofError::Rejected("sub_epoch_n underflow"))?;
    let ses = summaries
        .get(se_idx)
        .ok_or(WeightProofError::Rejected("summary index out of range"))?;

    // First challenge block in the segment = first entry that is not a slot-end.
    let mut first_idx: Option<usize> = None;
    for (i, curr) in segment.sub_slots.iter().enumerate() {
        if curr.cc_slot_end.is_none() {
            first_idx = Some(i);
            break;
        }
    }
    let Some(first_idx) = first_idx else {
        return Ok(None);
    };
    let first = &segment.sub_slots[first_idx];
    let Some(first_sp_index) = first.signage_point_index else {
        return Ok(None);
    };
    let slots = &segment.sub_slots;

    let mut slots_n: i64 = 1;
    let overflow =
        is_overflow_block(c, first_sp_index).map_err(|_| WeightProofError::Rejected("overflow"))?;
    if overflow && first_idx >= 2 && slots[first_idx - 2].cc_slot_end.is_none() {
        slots_n = 2;
    }

    let mut new_diff = ses.new_difficulty;
    let mut new_ssi = ses.new_sub_slot_iters;
    let mut ses_hash_v: Option<Bytes32> = Some(hash_of(ses)?);
    if overflow
        && first_idx >= 2
        && slots[first_idx - 2].cc_slot_end.is_some()
        && slots[first_idx - 1].cc_slot_end.is_some()
    {
        ses_hash_v = None;
        new_ssi = None;
        new_diff = None;
    }

    // Walk back to the slots_n-th slot-end.
    let mut idx = first_idx;
    loop {
        if slots[idx].cc_slot_end.is_some() {
            slots_n -= 1;
            if slots_n == 0 {
                break;
            }
        }
        if idx == 0 {
            return Ok(None);
        }
        idx -= 1;
    }

    let sub_slot = &slots[idx];
    let cc_slot_end_info = match &sub_slot.cc_slot_end_info {
        Some(x) => x,
        None => return Ok(None),
    };
    let rc_slot_end_info = match &segment.rc_slot_end_info {
        Some(x) => x,
        None => return Ok(None),
    };

    let mut icc_sub_slot_hash: Option<Bytes32> = None;
    let cc_vdf_info: VdfInfo;
    if idx != 0 {
        ses_hash_v = None;
        new_ssi = None;
        new_diff = None;
        cc_vdf_info = VdfInfo {
            challenge: cc_slot_end_info.challenge,
            number_of_iterations: curr_ssi,
            output: cc_slot_end_info.output,
        };
        if let Some(icc) = &sub_slot.icc_slot_end_info {
            let icc_info = VdfInfo {
                challenge: icc.challenge,
                number_of_iterations: curr_ssi,
                output: icc.output,
            };
            icc_sub_slot_hash = Some(hash_of(&icc_info)?);
        }
    } else {
        cc_vdf_info = *cc_slot_end_info;
        if let Some(icc) = &sub_slot.icc_slot_end_info {
            icc_sub_slot_hash = Some(hash_of(icc)?);
        }
    }

    let cc_sub_slot = ChallengeChainSubSlot {
        challenge_chain_end_of_slot_vdf: cc_vdf_info,
        infused_challenge_chain_sub_slot_hash: icc_sub_slot_hash,
        subepoch_summary_hash: ses_hash_v,
        new_sub_slot_iters: new_ssi,
        new_difficulty: new_diff,
    };
    let rc_sub_slot = RewardChainSubSlot {
        end_of_slot_vdf: *rc_slot_end_info,
        challenge_chain_sub_slot_hash: hash_of(&cc_sub_slot)?,
        infused_challenge_chain_sub_slot_hash: icc_sub_slot_hash,
        deficit: c.min_blocks_per_challenge_block,
    };
    Ok(Some(rc_sub_slot))
}

/// Proof of space for the sampled challenge block → required iters. Wires the real pospace verifier
/// (`dg_xch_pos::verify_and_get_quality_string`, chia's height-agnostic V1 path) then
/// `calculate_iterations_quality` with `difficulty_constant_factor` pinned from `ConsensusConstants`.
/// `None` means the pospace did not verify. (ref: `__validate_pospace`.)
fn validate_pospace(
    c: &ConsensusConstants,
    segment: &SubEpochChallengeSegment,
    idx: usize,
    curr_diff: u64,
    ses: Option<&SubEpochSummary>,
    first_in_sub_epoch: bool,
    height: u32,
) -> Result<Option<u64>, WeightProofError> {
    let cc_sub_slot_hash = if first_in_sub_epoch && segment.sub_epoch_n == 0 && idx == 0 {
        c.genesis_challenge
    } else {
        hash_of(&get_cc_sub_slot(&segment.sub_slots, idx, ses)?)?
    };
    let ssd = &segment.sub_slots[idx];

    // Python truthiness: `if sub_slot_data.signage_point_index and is_overflow_block(...)` — a `Some(0)`
    // signage-point index is falsy there, so it takes the non-overflow branch.
    let sp_overflow = match ssd.signage_point_index {
        Some(i) if i != 0 => {
            is_overflow_block(c, i).map_err(|_| WeightProofError::Rejected("overflow"))?
        }
        _ => false,
    };
    let challenge = if sp_overflow {
        if idx < 1 {
            return Ok(None);
        }
        segment.sub_slots[idx - 1]
            .cc_slot_end_info
            .as_ref()
            .ok_or(WeightProofError::Rejected("cc_slot_end_info"))?
            .challenge
    } else {
        cc_sub_slot_hash
    };

    let cc_sp_hash = match &ssd.cc_sp_vdf_info {
        None => cc_sub_slot_hash,
        Some(info) => hash_of(&info.output)?,
    };

    let pos = ssd
        .proof_of_space
        .as_ref()
        .ok_or(WeightProofError::Rejected("no proof of space"))?;
    let q_str = match verify_and_get_quality_string(pos, c, challenge, cc_sp_hash, height) {
        Some(q) => q,
        None => return Ok(None),
    };
    // difficulty_constant_factor pinned from the target chain's constants — never defaulted.
    let required_iters = calculate_iterations_quality(
        c.difficulty_constant_factor,
        q_str,
        pos.size,
        curr_diff,
        cc_sp_hash,
    );
    Ok(Some(required_iters))
}

/// The challenge block's own VDFs (signage-point + infusion-point), verified inline. (ref:
/// `_get_challenge_block_vdfs`.)
fn validate_challenge_block_vdfs(
    c: &ConsensusConstants,
    sub_slot_idx: usize,
    sub_slots: &[SubSlotData],
    ssi: u64,
    count: &mut usize,
) -> Result<(), WeightProofError> {
    let ssd = &sub_slots[sub_slot_idx];
    if let (Some(cc_sp), Some(cc_sp_info)) = (&ssd.cc_signage_point, &ssd.cc_sp_vdf_info) {
        // reference asserts a truthy signage_point_index here (not None, not 0).
        let sp_index = ssd
            .signage_point_index
            .filter(|&i| i != 0)
            .ok_or(WeightProofError::Rejected("signage_point_index"))?;
        let mut sp_input = default_classgroup_element();
        if !cc_sp.normalized_to_identity && sub_slot_idx >= 1 {
            let is_overflow = is_overflow_block(c, sp_index)
                .map_err(|_| WeightProofError::Rejected("overflow"))?;
            let prev_ssd = &sub_slots[sub_slot_idx - 1];
            sp_input = sub_slot_data_vdf_input(
                c,
                ssd,
                sub_slot_idx,
                sub_slots,
                is_overflow,
                ss_is_end_of_slot(prev_ssd),
                ssi,
            )?;
        }
        check_vdf(c, &sp_input, cc_sp_info, cc_sp, count)?;
    }

    let cc_ip = ssd
        .cc_infusion_point
        .as_ref()
        .ok_or(WeightProofError::Rejected("cc_infusion_point"))?;
    let cc_ip_info0 = ssd
        .cc_ip_vdf_info
        .as_ref()
        .ok_or(WeightProofError::Rejected("cc_ip_vdf_info"))?;
    let mut ip_input = default_classgroup_element();
    let mut cc_ip_info = *cc_ip_info0;
    if !cc_ip.normalized_to_identity && sub_slot_idx >= 1 {
        let prev_ssd = &sub_slots[sub_slot_idx - 1];
        if prev_ssd.cc_slot_end.is_none() {
            let prev_cc_ip = prev_ssd
                .cc_ip_vdf_info
                .as_ref()
                .ok_or(WeightProofError::Rejected("prev cc_ip_vdf_info"))?;
            let ti = ssd
                .total_iters
                .ok_or(WeightProofError::Rejected("total_iters"))?;
            let prev_ti = prev_ssd
                .total_iters
                .ok_or(WeightProofError::Rejected("prev total_iters"))?;
            ip_input = prev_cc_ip.output;
            let ip_vdf_iters =
                ti.checked_sub(prev_ti)
                    .ok_or(WeightProofError::Rejected("ip_vdf_iters"))? as u64;
            cc_ip_info = VdfInfo {
                challenge: cc_ip_info0.challenge,
                number_of_iterations: ip_vdf_iters,
                output: cc_ip_info0.output,
            };
        }
    }
    check_vdf(c, &ip_input, &cc_ip_info, cc_ip, count)?;
    Ok(())
}

/// A non-challenge sub-slot's VDFs (end-of-slot cc/icc, or intermediate signage/infusion), verified
/// inline. Blue-boxed (normalized) slots skip intermediate VDFs. (ref: `_validate_sub_slot_data`.)
fn validate_sub_slot_data(
    c: &ConsensusConstants,
    sub_slot_idx: usize,
    sub_slots: &[SubSlotData],
    ssi: u64,
    count: &mut usize,
) -> Result<(), WeightProofError> {
    if sub_slot_idx == 0 {
        return Err(WeightProofError::Rejected("sub_slot_data at index 0"));
    }
    let ssd = &sub_slots[sub_slot_idx];
    let prev_ssd = &sub_slots[sub_slot_idx - 1];

    if ss_is_end_of_slot(ssd) {
        if let Some(icc_slot_end) = &ssd.icc_slot_end {
            let mut input = default_classgroup_element();
            if !icc_slot_end.normalized_to_identity
                && let Some(prev_icc) = &prev_ssd.icc_ip_vdf_info
            {
                input = prev_icc.output;
            }
            let icc_info = ssd
                .icc_slot_end_info
                .as_ref()
                .ok_or(WeightProofError::Rejected("icc_slot_end_info"))?;
            check_vdf(c, &input, icc_info, icc_slot_end, count)?;
        }
        let cc_slot_end_info = ssd
            .cc_slot_end_info
            .as_ref()
            .ok_or(WeightProofError::Rejected("cc_slot_end_info"))?;
        let cc_slot_end = ssd
            .cc_slot_end
            .as_ref()
            .ok_or(WeightProofError::Rejected("cc_slot_end"))?;
        let mut input = default_classgroup_element();
        if !ss_is_end_of_slot(prev_ssd) && !cc_slot_end.normalized_to_identity {
            let prev_cc_ip = prev_ssd
                .cc_ip_vdf_info
                .as_ref()
                .ok_or(WeightProofError::Rejected("prev cc_ip_vdf_info"))?;
            input = prev_cc_ip.output;
        }
        check_vdf(c, &input, cc_slot_end_info, cc_slot_end, count)?;
    } else {
        // Find the enclosing slot-end; if it is blue-boxed (normalized), skip intermediate VDFs.
        let mut i = sub_slot_idx;
        while i < sub_slots.len() - 1 {
            let curr_slot = &sub_slots[i];
            if ss_is_end_of_slot(curr_slot) {
                let cc_se = curr_slot
                    .cc_slot_end
                    .as_ref()
                    .ok_or(WeightProofError::Rejected("cc_slot_end"))?;
                if cc_se.normalized_to_identity {
                    return Ok(());
                }
                break;
            }
            i += 1;
        }
        if let (Some(icc_ip), Some(icc_info)) = (&ssd.icc_infusion_point, &ssd.icc_ip_vdf_info) {
            let mut input = default_classgroup_element();
            if !ss_is_challenge(prev_ssd)
                && let Some(prev_icc) = &prev_ssd.icc_ip_vdf_info
            {
                input = prev_icc.output;
            }
            check_vdf(c, &input, icc_info, icc_ip, count)?;
        }
        let sp_index = ssd
            .signage_point_index
            .ok_or(WeightProofError::Rejected("signage_point_index"))?;
        if let Some(cc_sp) = &ssd.cc_signage_point {
            let cc_sp_info = ssd
                .cc_sp_vdf_info
                .as_ref()
                .ok_or(WeightProofError::Rejected("cc_sp_vdf_info"))?;
            let mut input = default_classgroup_element();
            if !cc_sp.normalized_to_identity {
                let is_overflow = is_overflow_block(c, sp_index)
                    .map_err(|_| WeightProofError::Rejected("overflow"))?;
                input = sub_slot_data_vdf_input(
                    c,
                    ssd,
                    sub_slot_idx,
                    sub_slots,
                    is_overflow,
                    ss_is_end_of_slot(prev_ssd),
                    ssi,
                )?;
            }
            check_vdf(c, &input, cc_sp_info, cc_sp, count)?;
        }
        let mut input = default_classgroup_element();
        let cc_ip_info0 = ssd
            .cc_ip_vdf_info
            .as_ref()
            .ok_or(WeightProofError::Rejected("cc_ip_vdf_info"))?;
        let cc_ip = ssd
            .cc_infusion_point
            .as_ref()
            .ok_or(WeightProofError::Rejected("cc_infusion_point"))?;
        let mut cc_ip_info = *cc_ip_info0;
        if !cc_ip.normalized_to_identity && prev_ssd.cc_slot_end.is_none() {
            let prev_cc_ip = prev_ssd
                .cc_ip_vdf_info
                .as_ref()
                .ok_or(WeightProofError::Rejected("prev cc_ip_vdf_info"))?;
            input = prev_cc_ip.output;
            let ti = ssd
                .total_iters
                .ok_or(WeightProofError::Rejected("total_iters"))?;
            let prev_ti = prev_ssd
                .total_iters
                .ok_or(WeightProofError::Rejected("prev total_iters"))?;
            let ip_vdf_iters =
                ti.checked_sub(prev_ti)
                    .ok_or(WeightProofError::Rejected("ip_vdf_iters"))? as u64;
            cc_ip_info = VdfInfo {
                challenge: cc_ip_info0.challenge,
                number_of_iterations: ip_vdf_iters,
                output: cc_ip_info0.output,
            };
        }
        check_vdf(c, &input, &cc_ip_info, cc_ip, count)?;
    }
    Ok(())
}

/// Validate one segment: only the rng-sampled segment is cryptographically checked (its challenge block's
/// pospace + all its VDFs); others contribute nothing to verify here. Returns `Ok(false)` when the pospace
/// does not verify (reject), `Err` on malformed/invalid VDF. (ref: `_validate_segment`.)
#[allow(clippy::too_many_arguments)]
fn validate_segment(
    c: &ConsensusConstants,
    segment: &SubEpochChallengeSegment,
    curr_ssi: u64,
    curr_difficulty: u64,
    ses: Option<&SubEpochSummary>,
    first_segment_in_se: bool,
    sampled: bool,
    height: u32,
    count: &mut usize,
) -> Result<bool, WeightProofError> {
    if segment.sub_slots.len() > MAX_SUB_SLOTS_PER_SEGMENT {
        return Err(WeightProofError::TooLarge("sub_slots per segment"));
    }
    let mut after_challenge = false;
    for idx in 0..segment.sub_slots.len() {
        let ssd = &segment.sub_slots[idx];
        if sampled && ss_is_challenge(ssd) {
            after_challenge = true;
            let required_iters = match validate_pospace(
                c,
                segment,
                idx,
                curr_difficulty,
                ses,
                first_segment_in_se,
                height,
            )? {
                Some(ri) => ri,
                None => return Ok(false),
            };
            let sp_index = ssd
                .signage_point_index
                .ok_or(WeightProofError::Rejected("signage_point_index"))?;
            // Validates required_iters is in range — quality→iters is not bypassed.
            calculate_ip_iters(c, curr_ssi, sp_index, required_iters)
                .map_err(|_| WeightProofError::Rejected("ip_iters"))?;
            validate_challenge_block_vdfs(c, idx, &segment.sub_slots, curr_ssi, count)?;
        } else if sampled && after_challenge {
            validate_sub_slot_data(c, idx, &segment.sub_slots, curr_ssi, count)?;
        }
    }
    Ok(true)
}

/// Phase 4 — for each sub-epoch that carries segments, reconstruct its reward-chain hash (checked against
/// the summary), and cryptographically validate the ONE rng-chosen segment: its proof of space (→ required
/// iters) and every challenge-chain / infused-challenge-chain / reward-chain VDF. The rng that picks the
/// segment is the *same continued stream* phase 1 seeded (`summaries[-2]` hash) and advanced through its
/// sampling draws — so the prover cannot know which segment will be checked, and every one must be valid.
/// (ref: `_validate_sub_epoch_segments` / `_validate_segment` / `__validate_pospace` / `_validate_vdf_batch`.)
fn validate_sub_epoch_segments(
    wp: &WeightProof,
    summaries: &[SubEpochSummary],
    c: &ConsensusConstants,
) -> Result<(), WeightProofError> {
    if summaries.len() < 2 {
        return Err(WeightProofError::Rejected(
            "fewer than two sub-epoch summaries",
        ));
    }
    // Continue the phase-1 RNG: re-seed from summaries[-2] and replay the sampling draws so `randbelow`
    // (chia's `rng.choice`) below starts from the exact state the reference's shared rng is in.
    let seed = ses_hash(&summaries[summaries.len() - 2])?;
    let mut rng = py_random::PyRandom::new(seed.as_ref());
    let tip = wp
        .recent_chain_data
        .last()
        .ok_or(WeightProofError::Malformed("no recent chain data"))?;
    let height = tip.reward_chain_block.height;
    let _ = get_weights_for_sampling(
        &mut rng,
        tip.reward_chain_block.weight,
        &wp.recent_chain_data,
    )?;

    let segments_by_sub_epoch = map_segments_by_sub_epoch(&wp.sub_epoch_segments);
    let max_seg = max_sub_epoch_segments(c);
    let mut rc_sub_slot_hash = c.genesis_challenge;
    let mut prev_ses: Option<&SubEpochSummary> = None;
    let mut vdf_count: usize = 0;

    for (sub_epoch_n, segments) in &segments_by_sub_epoch {
        let sub_epoch_n = *sub_epoch_n;
        if segments.len() > max_seg {
            return Err(WeightProofError::TooLarge("segments per sub-epoch"));
        }
        if (sub_epoch_n as usize) >= summaries.len() {
            return Err(WeightProofError::Rejected(
                "segment sub_epoch_n out of range",
            ));
        }
        // Recomputed per sub-epoch (matches the reference; the running `prev_ssi` it also derives is
        // passed to segment validation but never used there, so we omit it).
        let (curr_difficulty, curr_ssi) = get_curr_diff_ssi(c, sub_epoch_n as usize, summaries);

        // Which segment gets cryptographically checked — chia's `rng.choice(range(len(segments)))`.
        let sampled_seg_index = rng.randbelow(segments.len() as u64) as usize;

        if sub_epoch_n > 0 {
            let rc_sub_slot = get_rc_sub_slot(c, segments[0], summaries, curr_ssi)?.ok_or(
                WeightProofError::Rejected("could not reconstruct rc sub slot"),
            )?;
            prev_ses = Some(&summaries[(sub_epoch_n - 1) as usize]);
            rc_sub_slot_hash = hash_of(&rc_sub_slot)?;
        }
        if summaries[sub_epoch_n as usize].reward_chain_hash != rc_sub_slot_hash {
            return Err(WeightProofError::Rejected("reward_chain_hash mismatch"));
        }

        for (idx, segment) in segments.iter().enumerate() {
            let valid = validate_segment(
                c,
                segment,
                curr_ssi,
                curr_difficulty,
                prev_ses,
                idx == 0,
                sampled_seg_index == idx,
                height,
                &mut vdf_count,
            )?;
            if !valid {
                return Err(WeightProofError::Rejected("segment validation failed"));
            }
            prev_ses = None;
        }
    }
    Ok(())
}

/// Phase 5 — fully validate the recent-chain tail (PoSpace + VDFs + summary inclusion) so the proof
/// connects to a concrete, checkable peak. (ref: `validate_recent_blocks`, `_validate_pospace_recent_chain`.)
fn validate_recent_blocks(
    wp: &WeightProof,
    summaries: &[SubEpochSummary],
    c: &ConsensusConstants,
) -> Result<(), WeightProofError> {
    recent_blocks::validate_recent_blocks(wp, summaries, c)
}

/// Phase 6 — the whole proof sums to the claimed heaviest chain; the peak height/weight in the last
/// recent block is consistent with the summaries.
fn validate_total_weight(
    _wp: &WeightProof,
    _summaries: &[SubEpochSummary],
    _c: &ConsensusConstants,
) -> Result<(), WeightProofError> {
    // No-op by faithful reference diff, not by omission. The reference's ONLY total-weight reconciliation
    // is `_validate_summaries_weight` (chia `weight_proof.py:915`): it pins the summaries' accumulated
    // `total_weight` to the concrete recent-chain block at the last sub-epoch boundary
    // (`ses_end_height`). That check is already ported and enforced as phase 3
    // (`validate_summaries_weight`), with the inflate-boundary-weight tamper covered by
    // `phase3_tamper_inflate_boundary_weight_rejects`. In the reference, `recent_chain_data[-1]` (the
    // claimed peak) is used only as `peak_height` to bound segment sampling — never as a separate weight
    // check — and the recent chain's per-block weight/height continuity up to the peak is enforced by
    // phase 5's header validation (checks 27/28). The only remaining reference step,
    // `WeightProofHandler.get_fork_point`, compares the proof against a *local* blockchain
    // (`self.blockchain.get_ses_heights()`) to locate the fork height; a standalone light-client trust
    // anchor has no local chain and that step is out of scope for "is this proof valid". So there is no
    // additional consensus check to port here: with phases 1-5 green, the proof is fully validated.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_out_of_order_segments() {
        let segment = |sub_epoch_n| SubEpochChallengeSegment {
            sub_epoch_n,
            sub_slots: vec![],
            rc_slot_end_info: None,
        };
        assert!(matches!(
            validate_segment_order(&[segment(1), segment(0)]),
            Err(WeightProofError::Malformed(_))
        ));
    }

    /// Real-prod-data phase-2 accept path: a live-fetched MAINNET weight proof (tip height 9,054,698,
    /// weight 55,606,644,880). Reconstruct the summary chain from the proof's `SubEpochData` and prove
    /// its last summary's `ses_hash` equals the sub-epoch-summary hash actually committed on-chain (read
    /// from the recent chain's finished sub-slots). This is ground truth from mainnet itself — it only
    /// passes with the corrected 6-field `SubEpochSummary` (the missing `challenge_merkle_root` shifted
    /// the hash by one trailing byte). Independent of the reference's golden; both must agree.
    #[test]
    fn phase2_anchor_matches_on_real_mainnet_proof() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("real mainnet fixture at {}: {e}", path.display()));
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        let wp = WeightProof::from_bytes(&mut cur, ChiaProtocolVersion::default())
            .expect("real mainnet weight proof deserializes");
        assert_eq!(wp.sub_epochs.len(), 23_579, "fixture sub-epoch count");

        let c = &dg_xch_core::consensus::constants::MAINNET;
        let (last_ses_hash, _height) = get_last_ses_hash(c.sub_epoch_blocks, &wp.recent_chain_data)
            .expect("on-chain sub-epoch-summary hash present in recent chain");
        let (summaries, _total, _weights) = map_sub_epoch_summaries(
            c.sub_epoch_blocks,
            c.genesis_challenge,
            &wp.sub_epochs,
            c.difficulty_starting,
        )
        .expect("reconstruct summaries");
        assert_eq!(summaries.len(), wp.sub_epochs.len());

        let last = summaries.last().expect("summaries non-empty");
        assert_eq!(
            ses_hash(last).expect("hash last summary"),
            last_ses_hash,
            "reconstructed last-summary hash must equal mainnet's on-chain sub-epoch-summary commitment"
        );

        // Full phase-2 entry point accepts the real proof and returns summaries + total + weight list.
        let (out, _total, weights) =
            validate_sub_epoch_summaries(&wp, c).expect("phase 2 accepts the real mainnet proof");
        assert_eq!(out.len(), 23_579);
        // The weight list has one entry per sub-epoch: (n-1) accrued in-loop + 1 trailing close = n.
        assert_eq!(weights.len(), out.len());
    }

    /// Phase 5 accept path on the real mainnet proof, exercised directly (reconstruct summaries via
    /// phase 2, then run `validate_recent_blocks`) so it doesn't pay phase 4's ~min of VDF batch work.
    /// This is the gate-ready proof: the recent-chain header validator (PoSpace + CC/RC/ICC VDFs + BLS
    /// signatures + SES reconstruction + deficit/epoch state machine) accepts the real chain tail. Runs
    /// real VDF/BLS verification on the recent tip blocks — run in release.
    #[test]
    #[ignore = "heavyweight: recent-chain VDF/BLS verification; run in release with --ignored"]
    fn phase5_accepts_real_mainnet_proof() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("real mainnet fixture at {}: {e}", path.display()));
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        let wp = WeightProof::from_bytes(&mut cur, ChiaProtocolVersion::default())
            .expect("real mainnet weight proof deserializes");
        let c = &dg_xch_core::consensus::constants::MAINNET;
        let (summaries, _total, _weights) =
            validate_sub_epoch_summaries(&wp, c).expect("phase 2 accepts the real mainnet proof");
        validate_recent_blocks(&wp, &summaries, c).expect("phase 5 accepts the real mainnet proof");
    }

    /// End-to-end completion: the FULL `validate_weight_proof` accepts the real mainnet proof through all
    /// six phases and returns `Ok((true, summaries))` with NO `PhaseUnimplemented` markers left. This is
    /// the "validator complete" bar. Heavyweight — runs phase 4's ~1000-VDF batch plus phase 5's recent
    /// chain (~10 min); run in release with `--ignored`.
    #[test]
    #[ignore = "full pipeline: ~10 min of real VDF/BLS verification (phase 4 batch + phase 5); run in release"]
    fn phase6_full_validator_accepts_real_mainnet_proof() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("real mainnet fixture at {}: {e}", path.display()));
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        let wp = WeightProof::from_bytes(&mut cur, ChiaProtocolVersion::default())
            .expect("real mainnet weight proof deserializes");
        let c = &dg_xch_core::consensus::constants::MAINNET;
        let (valid, summaries) = validate_weight_proof_with_progress(&wp, c, &mut |status| {
            eprintln!("weight-proof: {status}");
        })
        .expect("full validator accepts the real mainnet proof");
        assert!(
            valid,
            "validator must return valid=true on the real mainnet proof"
        );
        assert_eq!(summaries.len(), 23_579, "returned summary chain length");
    }

    /// Byte-exact parity of the sampling RNG against CPython's `random.Random`. Determinism is the whole
    /// security property of phase 1, so this asserts bit-identical `random()` output for fixed seeds
    /// (vectors captured from CPython, including a leading-zero-byte seed that exercises key sizing).
    #[test]
    fn mt19937_getrandbits_and_randbelow_match_cpython() {
        // getrandbits sequence on one rng (seed 00..1f), then choice(range(n)) on fresh rng each.
        let seed: Vec<u8> = (0u8..32).collect();
        let mut r = py_random::PyRandom::new(&seed);
        let got: Vec<u64> = [1u32, 4, 8, 15, 32, 33, 64]
            .iter()
            .map(|&k| r.getrandbits(k))
            .collect();
        assert_eq!(
            got,
            vec![
                1,
                5,
                231,
                1175,
                2_810_937_230,
                6_094_649_597,
                11_238_162_993_324_450_277
            ]
        );
        for (n, want) in [
            (1u64, 0u64),
            (2, 1),
            (3, 1),
            (5, 2),
            (10, 5),
            (100, 40),
            (236, 81),
        ] {
            let mut rr = py_random::PyRandom::new(&seed);
            assert_eq!(rr.randbelow(n), want, "choice(range({n}))");
        }
    }

    #[test]
    fn mt19937_matches_cpython_random_vectors() {
        let seed: Vec<u8> = (0u8..32).collect();
        let mut r = py_random::PyRandom::new(&seed);
        let want: [f64; 5] = [
            0.9592884430034848,
            0.904383003978874,
            0.6544723243938474,
            0.5561377199205005,
            0.6092220395645366,
        ];
        for w in want {
            assert_eq!(
                r.random().to_bits(),
                w.to_bits(),
                "random() must be bit-exact vs CPython"
            );
        }

        let seed2: Vec<u8> = vec![0u8, 0u8].into_iter().chain(0u8..30).collect();
        let mut r2 = py_random::PyRandom::new(&seed2);
        let want2: [f64; 3] = [0.48331334892446376, 0.18455929743918742, 0.01555251624192];
        for w in want2 {
            assert_eq!(r2.random().to_bits(), w.to_bits());
        }
    }

    /// Real-prod-data phase-1 accept path on the live mainnet proof: the seed-derived RNG selects a set
    /// of sub-epochs, and every one must be backed by challenge segments in the proof. A divergent RNG
    /// would select different sub-epochs and reject here — so this passing is strong evidence the sample
    /// set matches chia's. (The independent reference-parity on the exact index set is the independent gate.)
    #[test]
    fn phase1_accepts_on_real_mainnet_proof() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("real mainnet fixture: {e}"));
        let wp = WeightProof::from_bytes(
            &mut std::io::Cursor::new(bytes.as_slice()),
            ChiaProtocolVersion::default(),
        )
        .expect("deserialize real mainnet weight proof");
        let c = &dg_xch_core::consensus::constants::MAINNET;
        let (summaries, _total, weights) = validate_sub_epoch_summaries(&wp, c).expect("phase 2");
        validate_sub_epoch_sampling(&wp, &summaries, &weights, c).expect(
            "phase 1 accepts the real mainnet proof (RNG sample set is covered by segments)",
        );
    }

    /// Real-prod-data phase-3 accept path: phase 2's accumulated `total_weight` must equal the weight the
    /// recent chain reports at the last sub-epoch boundary, on the live mainnet proof. Also a writer-side
    /// negative: perturbing the total by one must reject (the reference's independent gate inflates a weight in the
    /// proof bytes via the public API).
    #[test]
    fn phase3_matches_weight_on_real_mainnet_proof() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("real mainnet fixture: {e}"));
        let wp = WeightProof::from_bytes(
            &mut std::io::Cursor::new(bytes.as_slice()),
            ChiaProtocolVersion::default(),
        )
        .expect("deserialize real mainnet weight proof");
        let c = &dg_xch_core::consensus::constants::MAINNET;
        let (summaries, total, _weights) = validate_sub_epoch_summaries(&wp, c).expect("phase 2");

        validate_summaries_weight(&wp, &summaries, total, c)
            .expect("phase 3 accepts: summaries weight equals the recent chain's boundary weight");

        // A one-unit perturbation of the accumulated weight must be rejected.
        assert!(matches!(
            validate_summaries_weight(&wp, &summaries, total + 1, c),
            Err(WeightProofError::Rejected(_))
        ));
    }

    /// Phase 4 (segments) accepts the REAL mainnet weight proof end-to-end: every sampled challenge
    /// segment's proof of space and all its challenge-chain / infused-challenge-chain / reward-chain VDFs
    /// verify against the real `dg_xch_pos` / `dg_xch_vdf` primitives. This exercises the full VDF path
    /// (including the `witness_type=2` n-wesolowski forms whose bqfc serialization once diverged from
    /// chiavdf). HEAVYWEIGHT: it verifies ~1000 real class-group VDFs — run in release
    /// (`cargo test -p dg_xch_weight_proof --release phase4_accepts_real_mainnet_proof`); it is slow in a
    /// debug build (verify with `--release`).
    #[test]
    #[ignore = "heavyweight: ~1000 real class-group VDFs; run in release with --ignored"]
    fn phase4_accepts_real_mainnet_proof() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("real mainnet fixture: {e}"));
        let wp = WeightProof::from_bytes(
            &mut std::io::Cursor::new(bytes.as_slice()),
            ChiaProtocolVersion::default(),
        )
        .expect("deserialize");
        let c = &dg_xch_core::consensus::constants::MAINNET;
        let (summaries, _t, _w) = validate_sub_epoch_summaries(&wp, c).expect("phase 2");
        // Phase 4 accepts: all sampled-segment proofs of space + VDFs verify against the real primitives.
        validate_sub_epoch_segments(&wp, &summaries, c)
            .expect("phase 4 accepts the real mainnet proof");
    }

    #[test]
    fn an_incomplete_validator_fails_closed_never_accepts() {
        // A structurally-plausible proof must NOT be accepted while phases are unported — the whole
        // safety property of a WIP consensus validator. This test must stay green until the port is
        // complete AND a real accept-vector replaces it.
        let wp = WeightProof {
            sub_epochs: vec![],
            sub_epoch_segments: vec![],
            recent_chain_data: vec![],
        };
        // empty → Malformed (never Ok(true))
        assert!(matches!(
            validate_weight_proof(&wp, &dg_xch_core::consensus::constants::MAINNET),
            Err(WeightProofError::Malformed(_))
        ));
    }
}
