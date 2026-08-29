//! Compact-VDF (bluebox) pipeline primitives — the pure field-dispatch, guard, and proof-swap
//! logic behind the full node's `RequestCompactVDF` / `NewCompactVDF` / `RespondCompactVDF`
//! handling.
//!
//! A bluebox timelord normalizes a block's bulky Wesolowski VDF proofs to compact
//! (`normalized_to_identity`, `witness_type == 0`) form and gossips them; peers pull the compact
//! proof and swap it into their stored block, shrinking the on-disk chain. This module holds the
//! store-blind, consensus-blind half of that exchange.

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::protocols::timelord::RequestCompactProofOfTime;
use dg_xch_vdf::{default_classgroup_element, validate_vdf_info};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Which VDF field of a block a compact proof refers to. Wire value is the `field_vdf: u8` of
/// the compact-VDF messages.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CompressibleVdfField {
    CcEosVdf = 1,
    IccEosVdf = 2,
    CcSpVdf = 3,
    CcIpVdf = 4,
}

impl CompressibleVdfField {
    /// Decode the wire `field_vdf` byte; `None` for an unknown discriminant (dropped silently,
    /// never a panic).
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::CcEosVdf),
            2 => Some(Self::IccEosVdf),
            3 => Some(Self::CcSpVdf),
            4 => Some(Self::CcIpVdf),
            _ => None,
        }
    }
}

/// A proof is compact when its witness has been normalized to the identity element and carries
/// no intermediate witnesses.
#[must_use]
pub fn is_compact(proof: &VdfProof) -> bool {
    proof.witness_type == 0 && proof.normalized_to_identity
}

/// The stored proof for `field` whose start-of-slot `VdfInfo` equals `vdf_info`, if any.
fn stored_proof_for<'a>(
    block: &'a FullBlock,
    field: CompressibleVdfField,
    vdf_info: &VdfInfo,
) -> Option<&'a VdfProof> {
    match field {
        CompressibleVdfField::CcEosVdf => block.finished_sub_slots.iter().find_map(|ss| {
            (&ss.challenge_chain.challenge_chain_end_of_slot_vdf == vdf_info)
                .then_some(&ss.proofs.challenge_chain_slot_proof)
        }),
        CompressibleVdfField::IccEosVdf => block.finished_sub_slots.iter().find_map(|ss| {
            ss.infused_challenge_chain.as_ref().and_then(|icc| {
                (&icc.infused_challenge_chain_end_of_slot_vdf == vdf_info)
                    .then_some(())
                    .and(ss.proofs.infused_challenge_chain_slot_proof.as_ref())
            })
        }),
        CompressibleVdfField::CcSpVdf => block
            .reward_chain_block
            .challenge_chain_sp_vdf
            .as_ref()
            .filter(|sp| *sp == vdf_info)
            .and(block.challenge_chain_sp_proof.as_ref()),
        CompressibleVdfField::CcIpVdf => (&block.reward_chain_block.challenge_chain_ip_vdf
            == vdf_info)
            .then_some(&block.challenge_chain_ip_proof),
    }
}

/// Serve arm: return our stored proof for `field` at `vdf_info` only when it is already compact —
/// otherwise stay silent (`None`).
#[must_use]
pub fn serve_compact(block: &FullBlock, field: u8, vdf_info: &VdfInfo) -> Option<VdfProof> {
    let field = CompressibleVdfField::from_u8(field)?;
    let proof = stored_proof_for(block, field, vdf_info)?;
    is_compact(proof).then(|| proof.clone())
}

/// Does this block still carry a bulky proof for `field` at `vdf_info`? True only when the field
/// is present, its `VdfInfo` matches, and its proof is not yet compact — the exact condition
/// under which a compact replacement is wanted.
#[must_use]
pub fn needs_compact_proof(block: &FullBlock, field: u8, vdf_info: &VdfInfo) -> bool {
    let Some(field) = CompressibleVdfField::from_u8(field) else {
        return false;
    };
    stored_proof_for(block, field, vdf_info).is_some_and(|p| !is_compact(p))
}

/// Verify an offered compact proof stands on its own: it is compact, and it validates from the
/// default class-group element (the normalized-to-identity entry).
#[must_use]
pub fn validate_compact_proof(
    constants: &ConsensusConstants,
    vdf_info: &VdfInfo,
    proof: &VdfProof,
) -> bool {
    is_compact(proof)
        && validate_vdf_info(
            constants,
            &default_classgroup_element(),
            vdf_info,
            proof,
            None,
        )
}

/// Full admission gate for an offered compact proof:
/// - not too recent (`peak_height - height >= 5` — never compactify a recent block);
/// - the proof is compact and validates from the default element;
/// - we still hold a bulky proof for that exact field/`VdfInfo` (`needs_compact_proof`).
///
/// A whole-block fully-compactified short-circuit is store-side bookkeeping we do not maintain;
/// the per-field `needs_compact_proof` is the decisive guard, so it is omitted deliberately.
#[must_use]
pub fn can_accept_compact_proof(
    constants: &ConsensusConstants,
    block: &FullBlock,
    field: u8,
    vdf_info: &VdfInfo,
    proof: &VdfProof,
    peak_height: u32,
    height: u32,
) -> bool {
    if peak_height.saturating_sub(height) < 5 {
        return false;
    }
    if !validate_compact_proof(constants, vdf_info, proof) {
        return false;
    }
    needs_compact_proof(block, field, vdf_info)
}

/// Return a copy of `block` with the compact `new_proof` swapped in for the single `field` whose
/// `VdfInfo` matches `vdf_info`; `None` when nothing matches. Only the one proof changes, so the
/// block's `header_hash` is unaffected. The caller re-writes the body under the same header hash
/// and re-gossips `NewCompactVDF`.
#[must_use]
pub fn replace_proof(
    block: &FullBlock,
    field: u8,
    vdf_info: &VdfInfo,
    new_proof: &VdfProof,
) -> Option<FullBlock> {
    let field = CompressibleVdfField::from_u8(field)?;
    let mut nb = block.clone();
    match field {
        CompressibleVdfField::CcEosVdf => {
            let ss = nb
                .finished_sub_slots
                .iter_mut()
                .find(|ss| &ss.challenge_chain.challenge_chain_end_of_slot_vdf == vdf_info)?;
            ss.proofs.challenge_chain_slot_proof = new_proof.clone();
        }
        CompressibleVdfField::IccEosVdf => {
            let ss = nb.finished_sub_slots.iter_mut().find(|ss| {
                ss.infused_challenge_chain
                    .as_ref()
                    .is_some_and(|icc| &icc.infused_challenge_chain_end_of_slot_vdf == vdf_info)
            })?;
            ss.proofs.infused_challenge_chain_slot_proof = Some(new_proof.clone());
        }
        CompressibleVdfField::CcSpVdf => {
            if nb
                .reward_chain_block
                .challenge_chain_sp_vdf
                .as_ref()
                .is_none_or(|sp| sp != vdf_info)
            {
                return None;
            }
            nb.challenge_chain_sp_proof = Some(new_proof.clone());
        }
        CompressibleVdfField::CcIpVdf => {
            if &nb.reward_chain_block.challenge_chain_ip_vdf != vdf_info {
                return None;
            }
            nb.challenge_chain_ip_proof = new_proof.clone();
        }
    }
    Some(nb)
}

/// Every VDF field of `block` whose proof is still bulky, as `(field_vdf, start-of-slot VdfInfo)`
/// pairs — the solicitation list handed to blueboxes. Order: sub-slot EOS fields first, then
/// CC_SP, then CC_IP.
#[must_use]
pub fn uncompact_fields(block: &FullBlock) -> Vec<(u8, VdfInfo)> {
    let mut out = Vec::new();
    for ss in &block.finished_sub_slots {
        if !is_compact(&ss.proofs.challenge_chain_slot_proof) {
            out.push((
                CompressibleVdfField::CcEosVdf as u8,
                ss.challenge_chain.challenge_chain_end_of_slot_vdf,
            ));
        }
        if let (Some(icc), Some(proof)) = (
            ss.infused_challenge_chain.as_ref(),
            ss.proofs.infused_challenge_chain_slot_proof.as_ref(),
        ) && !is_compact(proof)
        {
            out.push((
                CompressibleVdfField::IccEosVdf as u8,
                icc.infused_challenge_chain_end_of_slot_vdf,
            ));
        }
    }
    if let (Some(sp_vdf), Some(sp_proof)) = (
        block.reward_chain_block.challenge_chain_sp_vdf.as_ref(),
        block.challenge_chain_sp_proof.as_ref(),
    ) && !is_compact(sp_proof)
    {
        out.push((CompressibleVdfField::CcSpVdf as u8, *sp_vdf));
    }
    if !is_compact(&block.challenge_chain_ip_proof) {
        out.push((
            CompressibleVdfField::CcIpVdf as u8,
            block.reward_chain_block.challenge_chain_ip_vdf,
        ));
    }
    out
}

/// Dedup key for one solicited compact-proof-of-time request: the block's header hash, the
/// compressible field byte, and the field's start-of-slot VDF challenge + iteration count. The
/// challenge is what distinguishes multiple CC_EOS/ICC_EOS sub-slots of the SAME block (they
/// share a field byte but each finished sub slot has a distinct challenge), so the key does not
/// collapse them into one — a block with two bulky EOS slots solicits both.
type SolicitKey = (Bytes32, u8, Bytes32, u64);

fn solicit_key(req: &RequestCompactProofOfTime) -> SolicitKey {
    (
        req.header_hash,
        req.field_vdf,
        req.new_proof_of_time.challenge,
        req.new_proof_of_time.number_of_iterations,
    )
}

/// Bounded time-to-live + capacity dedup of already-solicited compact-proof requests. The scan
/// sweeps a fixed recent window every tick; without a memory it would re-solicit the same bulky
/// fields every interval. After `ttl` a key is re-solicitable (a timelord may have dropped the
/// request). Past `capacity` distinct keys the oldest is evicted — an evicted stale key simply
/// becomes eligible to solicit again.
pub struct SolicitLedger {
    /// Last-solicited instant per distinct request key.
    seen: HashMap<SolicitKey, Instant>,
    /// Insertion order of distinct keys, for capacity eviction (one entry per live key).
    order: VecDeque<SolicitKey>,
    capacity: usize,
    ttl: Duration,
}

impl SolicitLedger {
    /// `capacity` distinct keys retained (>=1 enforced); a key is re-solicitable after `ttl`.
    #[must_use]
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            seen: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
            ttl,
        }
    }

    /// Should `req` be solicited at `now`? Returns `true` and records the send when the field has
    /// not been solicited within `ttl` (first sight, or the ttl window has expired); `false` when a
    /// recent solicitation is still outstanding. A `true` result mutates the ledger (records the
    /// send + evicts if over capacity); a `false` result leaves it untouched.
    pub fn admit(&mut self, req: &RequestCompactProofOfTime, now: Instant) -> bool {
        let key = solicit_key(req);
        if let Some(&last) = self.seen.get(&key)
            && now.duration_since(last) < self.ttl
        {
            return false;
        }
        let is_new = self.seen.insert(key, now).is_none();
        if is_new {
            self.order.push_back(key);
        }
        while self.seen.len() > self.capacity {
            let Some(evict) = self.order.pop_front() else {
                break;
            };
            self.seen.remove(&evict);
        }
        true
    }

    /// Distinct keys currently retained (bounded by `capacity`). For tests/observability.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether the ledger holds no solicited keys.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Build the deduped solicitation list for one stored `block`: every still-bulky VDF field of the
/// block (see [`uncompact_fields`]) turned into a [`RequestCompactProofOfTime`], minus any the
/// `ledger` already solicited within its ttl.
#[must_use]
pub fn plan_block_solicitations(
    block: &FullBlock,
    header_hash: Bytes32,
    height: u32,
    ledger: &mut SolicitLedger,
    now: Instant,
) -> Vec<RequestCompactProofOfTime> {
    uncompact_fields(block)
        .into_iter()
        .map(|(field_vdf, vdf_info)| RequestCompactProofOfTime {
            new_proof_of_time: vdf_info,
            header_hash,
            height,
            field_vdf,
        })
        .filter(|req| ledger.admit(req, now))
        .collect()
}
