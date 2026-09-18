//! Farmer candidate construction and its bounded chain walks.

use super::peer_api::StoreApi;
use super::*;

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    pub(in crate::node) async fn try_build_candidate(
        &self,
        declare: &DeclareProofOfSpace,
        quality_string: Bytes32,
    ) -> Option<RequestSignedValues> {
        debug!(
            "producer.build start qs={} sp_index={}",
            quality_string, declare.signage_point_index
        );
        self.try_build_candidate_inner(declare, quality_string)
            .await
    }

    pub(in crate::node) async fn try_build_candidate_inner(
        &self,
        declare: &DeclareProofOfSpace,
        // Keys the candidate in `self.candidates` and rides in the `RequestSignedValues`
        // (`add_candidate_block(quality_string, ...)`).
        quality_string: Bytes32,
    ) -> Option<RequestSignedValues> {
        let is_genesis_challenge = declare.challenge_hash == self.constants.genesis_challenge;

        // ---- Phase A: reachable slot inputs (one slot lock; a single PoSpace/hash pass, no I/O) ----
        let (sp, cc_challenge_hash, total_iters_pos_slot, rc_challenge, pos_eos) = {
            let slot = self.slot_state.lock().await;
            // the SP we accepted. At index 0 it is the all-None sub-slot-start
            // form; index > 0 carries real VDFs. Was a bare `?` (the #1 silent hole): an accepted proof
            // that no longer resolves here vanished with no trace. Now categorical.
            let Some(sp) = slot.get_signage_point(&declare.challenge_chain_sp) else {
                self.producer.candidate_dropped("sp_not_found_in_slotstate");
                info!(
                    "candidate: accepted SP no longer resolvable in slot state; dropping event={} reason={} cc_sp={}",
                    "producer.build.dropped",
                    "sp_not_found_in_slotstate",
                    declare.challenge_chain_sp
                );
                return None;
            };
            // At index 0 the SP is the sub-slot start (all-None); the cc challenge
            // is the SP hash itself. At index > 0 the SP MUST carry a cc VDF (guaranteed by admission); a
            // missing one is a malformed SP that slipped the accept ladder — was a bare `?`.
            let cc_challenge_hash = if declare.signage_point_index == 0 {
                declare.challenge_chain_sp
            } else {
                match sp.cc_vdf.as_ref() {
                    Some(v) => v.challenge,
                    None => {
                        self.producer.candidate_dropped("sp_cc_vdf_missing");
                        info!(
                            "candidate: index>0 SP has no challenge-chain VDF (malformed); dropping event={} reason={}",
                            "producer.build.dropped", "sp_cc_vdf_missing"
                        );
                        return None;
                    }
                }
            };
            // the pos sub-slot (start total-iters + its reward chain for the
            // backtrack); genesis has no stored sub-slot (total_iters_pos_slot = 0). Was a bare `?`
            // (the #2 silent hole).
            let (total_iters_pos_slot, pos_rc_end_challenge, pos_eos) = if is_genesis_challenge {
                (0u128, None, None)
            } else {
                let Some((eos, _, start)) = slot.get_sub_slot(&cc_challenge_hash) else {
                    self.producer.candidate_dropped("pos_sub_slot_not_found");
                    info!(
                        "candidate: pos sub-slot absent in slot state; dropping event={} reason={} cc_challenge_hash={}",
                        "producer.build.dropped", "pos_sub_slot_not_found", cc_challenge_hash
                    );
                    return None;
                };
                (
                    start,
                    Some(eos.reward_chain.end_of_slot_vdf.challenge),
                    Some(eos.clone()),
                )
            };
            // the reward-chain challenge the prev block must carry: index 0 uses
            // the pos sub-slot's reward chain, index > 0 the SP's rc-vdf challenge; then backtrack it
            // through the empty sub-slots we hold.
            let rc_challenge = if declare.signage_point_index == 0 {
                pos_rc_end_challenge
            } else {
                sp.rc_vdf.as_ref().map(|v| v.challenge)
            }
            .map(|rc| slot.backtrack_rc_challenge(rc));
            (
                sp,
                cc_challenge_hash,
                total_iters_pos_slot,
                rc_challenge,
                pos_eos,
            )
        };

        // the resolved cc challenge must equal the farmer's declared challenge.
        if cc_challenge_hash != declare.challenge_hash {
            self.producer.candidate_dropped("cc_challenge_mismatch");
            warn!(
                "candidate: resolved cc-challenge != declared challenge_hash; dropping event={} reason={} cc_challenge_hash={} declared={}",
                "producer.build.dropped",
                "cc_challenge_mismatch",
                cc_challenge_hash,
                declare.challenge_hash
            );
            return None;
        }

        // ---- Phase B: peak + prev-block linkage (store) ----
        let peak_rec = match self.store.get_peak().await {
            Ok(Some((hash, _))) => self.store.get_block_record(&hash).await.ok().flatten(),
            _ => None,
        };
        // prev_b starts at the peak; the reward-chain backtrack finds the true
        // previous block. No peak ⇒ genesis: prev_b = None, height 0.
        let prev_b: Option<BlockRecord> = if let Some(peak) = peak_rec.clone() {
            let Some(rc) = rc_challenge else {
                self.producer.candidate_dropped("no_rc_challenge");
                warn!(
                    "candidate: non-genesis declare with no reward-chain challenge resolved; dropping event={} reason={}",
                    "producer.build.dropped", "no_rc_challenge"
                );
                return None;
            };
            match backtrack_prev_block(self.store.as_ref(), peak, rc).await {
                Some(pb) => pb,
                None => {
                    self.producer.candidate_dropped("prev_block_not_found");
                    warn!(
                        "candidate: no previous block with the correct reward chain hash; dropping event={} reason={}",
                        "producer.build.dropped", "prev_block_not_found"
                    );
                    return None;
                }
            }
        } else {
            None
        };
        let height = match &prev_b {
            Some(pb) => pb.height + 1,
            None => 0,
        };

        // ---- Finished sub-slots (slot lock) + the pos-sub-slot guard ----
        // challenge_in_chain is block-store-derived (GENESIS if no prev block,
        // else prev_b's first-in-sub-slot ancestor's last finished challenge slot hash).
        let chain_challenge = match &prev_b {
            None => self.constants.genesis_challenge,
            Some(pb) => match challenge_in_chain(self.store.as_ref(), pb).await {
                Some(c) => c,
                None => {
                    self.producer
                        .candidate_dropped("challenge_in_chain_unresolved");
                    warn!(
                        "candidate: could not resolve challenge_in_chain from prev block; dropping event={} reason={}",
                        "producer.build.dropped", "challenge_in_chain_unresolved"
                    );
                    return None;
                }
            },
        };
        let finished_sub_slots = {
            let slot = self.slot_state.lock().await;
            slot.get_finished_sub_slots(chain_challenge, cc_challenge_hash)
        };
        let Some(finished_sub_slots) = finished_sub_slots else {
            self.producer
                .candidate_dropped("finished_sub_slots_disconnected");
            warn!(
                "candidate: finished sub-slots not connected; dropping event={} reason={} challenge_in_chain={} cc_challenge_hash={}",
                "producer.build.dropped",
                "finished_sub_slots_disconnected",
                chain_challenge,
                cc_challenge_hash
            );
            return None;
        };
        // the last finished sub-slot we would farm on must be the pos sub-slot.
        if let (Some(pos_eos), Some(last)) = (pos_eos.as_ref(), finished_sub_slots.last())
            && last != pos_eos
        {
            self.producer.candidate_dropped("wrong_sub_slots_to_farm");
            warn!(
                "candidate: have different sub-slots than required to farm this block; dropping event={} reason={}",
                "producer.build.dropped", "wrong_sub_slots_to_farm"
            );
            return None;
        }

        // ---- Phase D: pool/farmer targets, difficulty/ssi ----
        let (pool_target, farmer_ph) = match &prev_b {
            // Genesis pays the pre-farm puzzle hashes.
            None => (
                PoolTarget {
                    puzzle_hash: self.constants.genesis_pre_farm_pool_puzzle_hash,
                    max_height: 0,
                },
                self.constants.genesis_pre_farm_farmer_puzzle_hash,
            ),
            Some(_) => {
                // pool-contract plots pin the pool puzzle hash; OG plots carry
                // the farmer's pool_target.
                let pt = if let Some(ph) = declare.proof_of_space.pool_contract_puzzle_hash {
                    PoolTarget {
                        puzzle_hash: ph,
                        max_height: 0,
                    }
                } else if let Some(pt) = declare.pool_target {
                    pt
                } else {
                    self.producer.candidate_dropped("missing_pool_target");
                    warn!(
                        "candidate: OG-plot declare missing pool_target; dropping event={} reason={}",
                        "producer.build.dropped", "missing_pool_target"
                    );
                    return None;
                };
                (pt, declare.farmer_puzzle_hash)
            }
        };
        let peak_pair = match &peak_rec {
            Some(peak) => {
                let prev_weight = self
                    .store
                    .get_block_record(&peak.prev_hash)
                    .await
                    .ok()
                    .flatten()
                    .map_or(0, |r| r.weight);
                Some((peak, prev_weight))
            }
            None => None,
        };
        let (difficulty, sub_slot_iters) =
            candidate_difficulty_and_ssi(&self.constants, peak_pair, &finished_sub_slots);

        // ---- Phase E: iters + latency/empty-block guards ----
        let Some(iters) = resolve_candidate_iters(
            &self.constants,
            quality_string,
            &declare.proof_of_space,
            difficulty,
            sub_slot_iters,
            declare.signage_point_index,
            declare.challenge_chain_sp,
            total_iters_pos_slot,
        ) else {
            self.producer
                .candidate_dropped("required_iters_out_of_range");
            warn!(
                "candidate: proof failed the iters filter (required_iters out of range); dropping event={} reason={} sp_index={}",
                "producer.build.dropped",
                "required_iters_out_of_range",
                declare.signage_point_index
            );
            return None;
        };
        // a candidate that would infuse before the head is too late (latency).
        if let Some(peak) = &peak_rec
            && iters.infusion_point_total_iters < peak.total_iters
        {
            self.producer.candidate_dropped("latency_drop_candidate");
            warn!(
                "candidate: infusion point behind the current head (latency); dropping event={} reason={} sp_index={} infusion_point_total_iters={} head_total_iters={}",
                "producer.build.dropped",
                "latency_drop_candidate",
                declare.signage_point_index,
                iters.infusion_point_total_iters,
                peak.total_iters
            );
            return None;
        }
        // Empty-block coercion: if the candidate's signage point
        // sits at/before the transaction peak's window, the last transaction block prevents a new
        // one — coerce the block generator to None. tx-peak resolves via the O(1)
        // prev_transaction_block_hash link (the tx-peak link — NOT a peak backwalk).
        let mut coerce_empty = false;
        if let Some(peak) = &peak_rec {
            let tx_peak = if peak.is_transaction_block() {
                Some(peak.clone())
            } else if let Some(pth) = peak.prev_transaction_block_hash {
                self.store.get_block_record(&pth).await.ok().flatten()
            } else {
                None
            };
            if let Some(tx_peak) = tx_peak
                && iters.candidate_sp_total_iters <= tx_peak.total_iters
            {
                debug!("candidate: sp at/before the tx-peak window -> empty block");
                coerce_empty = true;
            }
        }

        // ---- Phase F: prev linkage (is_tx + reward claims), timestamp, assemble, store ----
        let total_iters_sp = total_iters_pos_slot + u128::from(iters.sp_iters);
        let prev = match &prev_b {
            // Genesis is a transaction block with no reward claims.
            None => CandidatePrev {
                is_transaction_block: true,
                prev_block_hash: self.constants.genesis_challenge,
                prev_transaction_block_hash: self.constants.genesis_challenge,
                prev_transaction_block_height: 0,
                reward_claims: Vec::new(),
            },
            Some(pb) => {
                match resolve_prev_linkage(self.store.as_ref(), &self.constants, pb, total_iters_sp)
                    .await
                {
                    Some(p) => p,
                    None => {
                        self.producer.candidate_dropped("prev_linkage_store_gap");
                        warn!(
                            "candidate: prev-block linkage/reward-claim walk failed (store gap); dropping event={} reason={}",
                            "producer.build.dropped", "prev_linkage_store_gap"
                        );
                        return None;
                    }
                }
            }
        };
        // timestamp strictly after the previous transaction block.
        let timestamp = match &prev_b {
            None => now_secs(),
            Some(pb) => candidate_timestamp(self.store.as_ref(), pb).await,
        };

        let transactions = {
            let mp = self.mempool.lock().await;
            if may_build_transactions(
                prev.is_transaction_block,
                coerce_empty,
                mp.peak().map(|(h, _)| h),
                prev.prev_transaction_block_height,
            ) {
                mp.create_block_generator(&self.constants, height, BLOCK_CREATION_TIMEOUT)
            } else {
                debug!(
                    "candidate: no mempool payload (non-tx, coerced, or frame mismatch); empty block is_tx={} coerce_empty={} mempool_peak={:?} prev_tx_height={}",
                    prev.is_transaction_block,
                    coerce_empty,
                    mp.peak(),
                    prev.prev_transaction_block_height
                );
                None
            }
        };

        // index 0 passes SignagePoint(None, ...) (the sub-slot start has no
        // signage VDFs); index > 0 passes the real SP. `sp` at index 0 is already the all-None form, so
        // either the explicit None or Some(&sp) would null the VDFs; None keeps the contract unambiguous.
        let sp_for_block = if declare.signage_point_index == 0 {
            None
        } else {
            Some(&sp)
        };
        let Some((candidate, request)) = assemble_candidate(
            &self.constants,
            declare,
            quality_string,
            sp_for_block,
            finished_sub_slots,
            &iters,
            height,
            &prev,
            transactions.as_ref(),
            pool_target,
            farmer_ph,
            timestamp,
            cc_challenge_hash,
        ) else {
            self.producer.candidate_dropped("assembly_hash_fail");
            warn!(
                "candidate: assembly failed to hash foliage/reward block; dropping event={} reason={}",
                "producer.build.dropped", "assembly_hash_fail"
            );
            return None;
        };

        // S3 success — the candidate exists. `partial` (the reward-chain-block hash) is the S6/S7 join
        // key; logging it here bridges the qs-keyed build events to the partial-keyed driver events.
        let partial = candidate.reward_chain_block.hash().ok();
        // `full_node_store.add_candidate_block`(quality_string, height, unfinished_block).
        self.candidates
            .lock()
            .await
            .insert(quality_string, height, candidate);
        self.producer.candidate_built();
        info!(
            "assembled candidate unfinished block; requesting farmer signatures event={} height={} sp_index={} qs={} partial={:?} tx_generator={}",
            "producer.build.assembled",
            height,
            declare.signage_point_index,
            quality_string,
            partial,
            transactions.is_some()
        );
        Some(request)
    }
}

pub(crate) fn may_build_transactions(
    is_transaction_block: bool,
    coerce_empty: bool,
    mempool_peak_height: Option<u32>,
    prev_transaction_block_height: u32,
) -> bool {
    is_transaction_block
        && !coerce_empty
        && mempool_peak_height == Some(prev_transaction_block_height)
}

// Wall-clock seconds since the Unix epoch (`uint64(time.time())`), 0 on a pre-epoch clock.
pub(super) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// A hard cap on any single candidate-assembly block-store walk. A well-formed chain reaches a
// sub-slot/transaction boundary within `MAX_SUB_SLOT_BLOCKS`; the cap turns a store gap or a corrupt
// ancestry into a bounded bail instead of an unbounded loop on the produce path.
pub(super) const CANDIDATE_WALK_CAP: usize = 512;

/// The reward-chain backtrack that finds the candidate's true previous block —
/// `declare_proof_of_space:1005-1022`. `rc_challenge` is already backtracked through empty sub-slots
/// (`SlotState::backtrack_rc_challenge`). Returns `None` to BAIL (the did-not-find return), or
/// `Some(prev_b)` where `prev_b` may itself be `None` when the finished-reward-slot match steps back to
/// the genesis boundary. Bounded to 10 attempts.
pub(crate) async fn backtrack_prev_block<S: BlockStore + Send + Sync>(
    store: &S,
    peak: BlockRecord,
    rc_challenge: Bytes32,
) -> Option<Option<BlockRecord>> {
    let mut prev_b = Some(peak);
    for _ in 0..10 {
        let Some(cur) = prev_b.clone() else { break };
        if cur.reward_infusion_new_challenge == rc_challenge {
            return Some(Some(cur));
        }
        if let Some(hashes) = &cur.finished_reward_slot_hashes
            && hashes.last() == Some(&rc_challenge)
        {
            // This block includes the sub-slot our SP vdf starts in; go back one more for the prev block.
            return Some(store.get_block_record(&cur.prev_hash).await.ok().flatten());
        }
        prev_b = store.get_block_record(&cur.prev_hash).await.ok().flatten();
    }
    None
}

/// The `challenge_in_chain` for `get_finished_sub_slots` —
/// `curr = prev_b; while not curr.first_in_sub_slot: curr = block_record(curr.prev_hash);
/// curr.finished_challenge_slot_hashes[-1]` (`full_node_store.get_finished_sub_slots`). Bounded.
pub(crate) async fn challenge_in_chain<S: BlockStore + Send + Sync>(
    store: &S,
    prev_b: &BlockRecord,
) -> Option<Bytes32> {
    let mut cur = prev_b.clone();
    for _ in 0..CANDIDATE_WALK_CAP {
        if cur.first_in_sub_slot() {
            return cur.finished_challenge_slot_hashes?.last().copied();
        }
        cur = store
            .get_block_record(&cur.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    None
}

/// `is_transaction_block` + the reward-claim walk + `prev_transaction_block_hash` for a non-genesis
/// candidate — `get_prev_transaction_block` +
/// `create_foliage`'s reward-claim walk. `total_iters_sp`
/// is the candidate's signage-point total iters (`total_iters_pos_slot + sp_iters`). Returns `None` on a
/// store gap. Walks are bounded by [`CANDIDATE_WALK_CAP`].
pub(crate) async fn resolve_prev_linkage<S: BlockStore + Send + Sync>(
    store: &S,
    constants: &ConsensusConstants,
    prev_b: &BlockRecord,
    total_iters_sp: u128,
) -> Option<CandidatePrev> {
    // get_prev_transaction_block: walk prev_b back to the first transaction block.
    let mut cur = prev_b.clone();
    for _ in 0..CANDIDATE_WALK_CAP {
        if cur.is_transaction_block() {
            break;
        }
        cur = store
            .get_block_record(&cur.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    if !cur.is_transaction_block() {
        return None; // walk cap hit without a transaction block — store gap.
    }
    let prev_transaction_block = cur;
    // is_transaction_block = total_iters_sp > prev_transaction_block.total_iters.
    let is_transaction_block = total_iters_sp > prev_transaction_block.total_iters;
    // height > 0 here (prev_b exists), so prev_block_hash is prev_b.header_hash .
    let prev_block_hash = prev_b.header_hash;
    if !is_transaction_block {
        // Non-tx candidate: create_foliage builds no foliage_transaction_block; the tx-hash/claims are
        // unused (prev_transaction_block_hash is a harmless placeholder) and the server never builds
        // a mempool payload for it (the height still records the true prev tx block).
        return Some(CandidatePrev {
            is_transaction_block: false,
            prev_block_hash,
            prev_transaction_block_hash: constants.genesis_challenge,
            prev_transaction_block_height: prev_transaction_block.height,
            reward_claims: Vec::new(),
        });
    }
    // The reward-claim walk (height > 0): the prev transaction block WITH its
    // fees, then every non-transaction block between it and the transaction block before it (fees = 0).
    let prev_transaction_block_hash = prev_transaction_block.header_hash;
    let mut reward_claims = vec![RewardBlockClaim {
        height: prev_transaction_block.height,
        pool_puzzle_hash: prev_transaction_block.pool_puzzle_hash,
        farmer_puzzle_hash: prev_transaction_block.farmer_puzzle_hash,
        fees: prev_transaction_block.fees.unwrap_or(0),
    }];
    if prev_transaction_block.height > 0 {
        let mut curr = store
            .get_block_record(&prev_transaction_block.prev_hash)
            .await
            .ok()
            .flatten()?;
        for _ in 0..CANDIDATE_WALK_CAP {
            if curr.is_transaction_block() {
                break;
            }
            reward_claims.push(RewardBlockClaim {
                height: curr.height,
                pool_puzzle_hash: curr.pool_puzzle_hash,
                farmer_puzzle_hash: curr.farmer_puzzle_hash,
                fees: 0,
            });
            curr = store
                .get_block_record(&curr.prev_hash)
                .await
                .ok()
                .flatten()?;
        }
    }
    Some(CandidatePrev {
        is_transaction_block: true,
        prev_block_hash,
        prev_transaction_block_hash,
        prev_transaction_block_height: prev_transaction_block.height,
        reward_claims,
    })
}

/// The candidate timestamp — `declare_proof_of_space:1113-1121`: `max(now, prev_tx_block.timestamp
/// + 1)`, walking `prev_b` back to the first transaction block (or genesis). Falls back to `now` on a
/// store gap. Bounded by [`CANDIDATE_WALK_CAP`].
pub(crate) async fn candidate_timestamp<S: BlockStore + Send + Sync>(
    store: &S,
    prev_b: &BlockRecord,
) -> u64 {
    let now = now_secs();
    let mut curr = prev_b.clone();
    for _ in 0..CANDIDATE_WALK_CAP {
        if curr.is_transaction_block() || curr.height == 0 {
            break;
        }
        match store.get_block_record(&curr.prev_hash).await.ok().flatten() {
            Some(next) => curr = next,
            None => return now,
        }
    }
    match curr.timestamp {
        Some(ts) if now <= ts => ts + 1,
        _ => now,
    }
}
