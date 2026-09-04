use super::*;

pub(in crate::node) async fn process_ub_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
) {
    let blocks: Vec<UnfinishedBlock> = node.ub_inbox.lock().await.drain(..).collect();
    if blocks.is_empty() {
        return;
    }
    for block in blocks {
        let Ok(partial_hash) = block.reward_chain_block.hash() else {
            // Was a silent continue: a ready UB dies here with nothing logged.
            node.producer.candidate_dropped("ub_reward_hash_fail");
            warn!(
                "unfinished block dropped: reward_chain_block failed to hash event={} reason={}",
                "producer.ub.dropped", "ub_reward_hash_fail"
            );
            continue;
        };
        let foliage_hash = block.foliage.foliage_transaction_block_hash;
        // Capture the parent hash before the match — the store-error arm moves `block` back onto the
        // inbox, so it can no longer be read through `block` after that point.
        let prev_hash = block.foliage.prev_block_hash;
        // Three OUTCOMES, not two. The old `let Ok(Some(prev)) = .. else { drop }` collapsed a store
        // ERROR into the same "we are behind" placeholder-drop as a genuine miss — and that drop also
        // `remove_requesting`s, forfeiting the re-fetch. For an OWN-farmed winning candidate whose prev
        // IS the current committed peak (read from THIS store during declare→assemble), a transient
        // backend hiccup on this one lookup would silently lose the block. A candidate must never be lost
        // to a DB read: add_unfinished_block resolves prev against the in-memory Blockchain, not the DB.
        //
        // Note the store never lags the validated chain here (engine commits a record BEFORE inserting
        // it into its in-memory cache, and block records are never deleted — reorg only flips
        // in_main_chain, which get_block_record-by-hash ignores). So an `Ok(None)` really is "we do not
        // have this parent yet" (we are behind), and an OWN candidate's committed-peak parent can only
        // fail to resolve via an `Err` — which we now retry instead of dropping.
        let prev = match node.store.get_block_record(&prev_hash).await {
            Ok(Some(prev)) => prev,
            Ok(None) => {
                // Parent genuinely absent — we have not validated it yet. Park the placeholder and drop
                // the pending request so a re-announce after the peak catches up re-fetches it (a UB
                // whose prev is not in the chain cannot validate). This is the expected
                // steady-state outcome while syncing (the bulk of this counter).
                node.producer.candidate_dropped("ub_prev_unknown");
                info!(
                    "unfinished block parked: parent block not in store (we are behind) event={} reason={} partial={} prev={}",
                    "producer.ub.dropped", "ub_prev_unknown", partial_hash, prev_hash
                );
                node.unfinished
                    .lock()
                    .await
                    .remove_requesting(&partial_hash, foliage_hash.as_ref());
                continue;
            }
            Err(e) => {
                // A STORE ERROR is NOT "we are behind": the parent may well be present. Re-queue for the
                // next drain (bounded by SP_INBOX_CAP) and DO NOT remove_requesting, so the lookup retries
                // against a recovered backend and the candidate — possibly our own winning block — survives.
                let mut inbox = node.ub_inbox.lock().await;
                if inbox.len() < SP_INBOX_CAP {
                    inbox.push(block);
                    drop(inbox);
                    node.producer.candidate_requeued("ub_prev_store_error");
                    warn!(
                        "unfinished block re-queued: store error resolving parent (retryable, candidate preserved) event={} reason={} partial={} prev={} error={}",
                        "producer.ub.requeued", "ub_prev_store_error", partial_hash, prev_hash, e
                    );
                } else {
                    drop(inbox);
                    // Only with the inbox saturated ON TOP of the store error is the candidate lost.
                    node.producer.candidate_dropped("ub_inbox_full");
                    warn!(
                        "unfinished block dropped: store error resolving parent AND ub_inbox at cap event={} reason={} partial={} error={}",
                        "producer.ub.dropped", "ub_inbox_full", partial_hash, e
                    );
                }
                continue;
            }
        };
        // The dedup ladder, AFTER the disconnected-parent check (a disconnected block is not
        // seen-marked, so a parked "we are behind" block is
        // re-processable once we catch up) and BEFORE any validation: the seen set keyed on the
        // EXACT unfinished block hash (many foliages can share one trunk — the seen set is the
        // DoS bound), then the per-(reward, foliage) cache
        // check. Together they bound a burst of duplicate announces to ONE header validation and
        // ONE generator run.
        match block.hash() {
            Ok(ub_hash) => {
                if node.unfinished.lock().await.seen(ub_hash) {
                    node.producer.candidate_dropped("ub_duplicate");
                    debug!(
                        "unfinished block dropped: exact duplicate already processed event={} reason={} partial={}",
                        "producer.ub.dropped", "ub_duplicate", partial_hash
                    );
                    continue;
                }
            }
            Err(e) => {
                node.producer.candidate_dropped("ub_hash_fail");
                warn!(
                    "unfinished block dropped: unfinished block failed to hash event={} reason={} partial={} error={}",
                    "producer.ub.dropped", "ub_hash_fail", partial_hash, e
                );
                continue;
            }
        }
        {
            // get_unfinished_block2: already held at this (reward, foliage), or a BETTER
            // (smaller-foliage) variant held — ignore. Placeholder (requested-not-received)
            // entries do not count as held.
            let cache = node.unfinished.lock().await;
            let (existing, _, has_better) = cache.get_block2(&partial_hash, foliage_hash.as_ref());
            if existing.is_some() || has_better {
                drop(cache);
                node.producer.candidate_dropped("ub_already_cached");
                debug!(
                    "unfinished block dropped: already cached (or a better variant is) event={} reason={} partial={}",
                    "producer.ub.dropped", "ub_already_cached", partial_hash
                );
                continue;
            }
        }
        let records = difficulty_records_map(node, &prev).await;
        let is_first_in_sub_slot = !block.finished_sub_slots.is_empty();
        // With the window sized by difficulty_record_depth this cannot fail for a parent whose
        // ancestry is in the store. A failure means the record walk broke mid-chain — a real
        // invariant break worth the WARN.
        let (ssi, difficulty) = match get_next_sub_slot_iters_and_difficulty(
            &node.constants,
            is_first_in_sub_slot,
            Some(&prev),
            &records,
        ) {
            Ok(v) => v,
            Err(e) => {
                node.producer.candidate_dropped("ub_ssi_difficulty_fail");
                warn!(
                    "unfinished block dropped: next SSI/difficulty computation failed event={} reason={} partial={} error={}",
                    "producer.ub.dropped", "ub_ssi_difficulty_fail", partial_hash, e
                );
                continue;
            }
        };
        let header = UnfinishedHeaderBlock {
            finished_sub_slots: block.finished_sub_slots.clone(),
            reward_chain_block: block.reward_chain_block.clone(),
            challenge_chain_sp_proof: block.challenge_chain_sp_proof.clone(),
            reward_chain_sp_proof: block.reward_chain_sp_proof.clone(),
            foliage: block.foliage,
            foliage_transaction_block: block.foliage_transaction_block,
            transactions_filter: dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::default(),
        };
        match validate_unfinished_header_block(
            &node.constants,
            &dg_xch_node::header::PrimitiveVerifier(&dg_xch_node::NativePrimitives),
            &records,
            &header,
            ValidationState { ssi, difficulty },
            true,
        ) {
            Ok(required_iters) => {
                // The transactions generator RUNS — and the cost and
                // aggregate-signature rules hold — BEFORE the block may enter the served cache or
                // the relay queue. Peers ban the sender of an invalid unfinished block; a node that
                // relayed without running the generator serves the poisoned block to honest
                // peers and eats that ban itself. Own-farmed candidates take this same path.
                //
                // Ban-posture delta: peers ban the SENDER of the invalid unfinished block for
                // 600s. Our p2p layer has no timed ban list, and the
                // RespondUnfinishedBlock inbox does not carry the sender's peer id — the
                // enforceable action today is the drop + no-relay below, which closes the harm
                // vector (nothing invalid is served or announced). Sender punishment lands with
                // the p2p ban list (same posture as the tx path, p2p/src/handlers.rs
                // TransactionAnnounceAction::Ban).
                if let Err((reason, e)) =
                    validate_ub_body(node, &block, prev.height.saturating_add(1), &prev).await
                {
                    node.producer.candidate_dropped(reason);
                    info!(
                        "unfinished block dropped: transactions generator/body validation failed event={} reason={} partial={} error={}",
                        "producer.ub.dropped", reason, partial_hash, e
                    );
                    node.unfinished
                        .lock()
                        .await
                        .remove_requesting(&partial_hash, foliage_hash.as_ref());
                    continue;
                }
                // The "added unfinished block" INFO line.
                info!(
                    "added unfinished block event={} partial={}",
                    "producer.ub.added", partial_hash
                );
                // Build the NewUnfinishedBlockTimelord BEFORE the block is moved into the cache.
                // sub_slot_iters/difficulty are the same context the header validation used; ses
                // is the summary the NEXT block would include (None on a near-genesis chain);
                // rc_prev is the last reward-chain infusion before this SP (the index-0 vs
                // index>0 split).
                let timelord_request = {
                    let ses = next_sub_epoch_summary(
                        &node.constants,
                        &records,
                        required_iters,
                        &block,
                        true,
                    )
                    .unwrap_or(None);
                    let rcb = &block.reward_chain_block;
                    // Resolve the pos sub-slot's reward-chain hash under the slot lock (index-0 path only),
                    // then let the pure helper apply the index-0/index>0 rc_prev split.
                    let pos_sub_slot_rc_hash = if rcb.signage_point_index == 0 {
                        let slot = node.slot_state.lock().await;
                        slot.get_sub_slot(&rcb.pos_ss_cc_challenge_hash)
                            .and_then(|(eos, _, _)| eos.reward_chain.hash().ok())
                    } else {
                        None
                    };
                    let rc_prev = dg_xch_node::farmer::timelord_rc_prev(
                        node.constants.genesis_challenge,
                        rcb.signage_point_index,
                        rcb.pos_ss_cc_challenge_hash,
                        rcb.reward_chain_sp_vdf.as_ref(),
                        pos_sub_slot_rc_hash,
                    );
                    rc_prev.map(|rc_prev| NewUnfinishedBlockTimelord {
                        reward_chain_block: block.reward_chain_block.clone(),
                        difficulty,
                        sub_slot_iters: ssi,
                        foliage: block.foliage,
                        sub_epoch_summary: ses,
                        rc_prev,
                    })
                };
                node.unfinished.lock().await.add_block(
                    partial_hash,
                    prev.height.saturating_add(1),
                    block,
                    required_iters,
                );
                node.ub_announce.lock().await.push(NewUnfinishedBlock2 {
                    unfinished_reward_hash: partial_hash,
                    foliage_hash,
                });
                match timelord_request {
                    Some(req) => node.ub_timelord_announce.lock().await.push(req),
                    None => {
                        node.producer
                            .candidate_dropped("timelord_rc_prev_unresolved");
                        warn!(
                            "timelord broadcast: could not resolve rc_prev; skipping NewUnfinishedBlockTimelord event={} reason={} partial={}",
                            "producer.ub.dropped", "timelord_rc_prev_unresolved", partial_hash
                        );
                    }
                }
            }
            // Promoted debug!->info!: a validation failure of a block we may have
            // farmed is a wall the operator must see (read %e for the specific consensus reason).
            Err(e) => {
                node.producer.candidate_dropped("ub_validation_fail");
                info!(
                    "unfinished block failed pre-validation event={} reason={} partial={} error={}",
                    "producer.ub.dropped", "ub_validation_fail", partial_hash, e
                );
                node.unfinished
                    .lock()
                    .await
                    .remove_requesting(&partial_hash, foliage_hash.as_ref());
            }
        }
    }
}

pub(in crate::node) async fn validate_ub_body<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
    block: &UnfinishedBlock,
    height: u32,
    prev: &BlockRecord,
) -> Result<(), (&'static str, NodeError)> {
    let mut refs: Vec<GeneratorReference> =
        Vec::with_capacity(block.transactions_generator_ref_list.len());
    for (index, &ref_height) in block.transactions_generator_ref_list.iter().enumerate() {
        // Live only for pre-SF9-regime chains — past SF9 the pure validator below bans a
        // non-empty ref list before the refs are even read.
        let generator = match node.store.get_generator_at_height(ref_height).await {
            Ok(Some(g)) => g,
            Ok(None) => {
                return Err((
                    "ub_generator_ref_missing",
                    ChiaError::GeneratorRefHasNoGenerator.into(),
                ));
            }
            Err(e) => return Err(("ub_generator_ref_missing", e.into())),
        };
        refs.push(GeneratorReference {
            height: ref_height,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            generator,
        });
    }
    // The SF9 body rules key on the previous TRANSACTION block's height (the CLVM flag ladder
    // keys on the block's own). Two regimes, two keys, as in the engine.
    let prev_tx_height = if prev.is_transaction_block() {
        prev.height
    } else {
        prev.prev_transaction_block_height
    };
    validate_unfinished_block_body(
        &NativePrimitives,
        &node.constants,
        block,
        &refs,
        height,
        prev_tx_height,
    )
    .map(|_| ())
    .map_err(classify_ub_body_error)
}

// Fold a body-validation failure into its producer-metrics drop reason: the cost rules
// (`ub_cost_mismatch`), the structural bindings (`ub_body_fail`), and everything the generator
// RUN itself surfaces — deserialize failure, CLVM raise, bad aggregate signature — under
// `ub_generator_fail` (the GENERATOR_RUNTIME_ERROR family, the live-ban vector).
pub(in crate::node) fn classify_ub_body_error(e: NodeError) -> (&'static str, NodeError) {
    let reason = match &e {
        NodeError::Consensus(
            ChiaError::InvalidBlockCost
            | ChiaError::BlockCostExceedsMax
            | ChiaError::InvalidCostResult,
        ) => "ub_cost_mismatch",
        NodeError::Consensus(
            ChiaError::InvalidTransactionsGeneratorHash
            | ChiaError::InvalidTransactionsInfoHash
            | ChiaError::TooManyGeneratorRefs
            | ChiaError::FutureGeneratorRefs
            | ChiaError::GeneratorRefHasNoGenerator
            | ChiaError::ComplexGeneratorReceived
            | ChiaError::TooManySpends,
        ) => "ub_body_fail",
        _ => "ub_generator_fail",
    };
    (reason, e)
}

/// Assemble the infused `FullBlock` for one `NewInfusionPointVDF` — the assembly half of
/// `new_infusion_point_vdf`, split out so it is unit-testable
/// against a seeded unfinished cache + `SlotState` without the block store's full validation engine.
/// Returns `None` on any bail (unknown unfinished block, prev block not reachable, disconnected
/// finished sub-slots, missing pos sub-slot, an iters failure, or an invalid pool signature) — the
/// caller then drops the infusion point (the timelord re-sends on the next `NewPeakTimelord`).
pub(in crate::node) async fn assemble_infusion_block<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    req: &NewInfusionPointVDF,
) -> Option<FullBlock> {
    // 1. the unfinished block this infusion point finishes.
    let unfinished = node
        .unfinished
        .lock()
        .await
        .get_block(&req.unfinished_reward_hash)
        .cloned();
    let Some(unfinished) = unfinished else {
        warn!(
            "infusion point: no cached unfinished reward block, cannot finish unfinished_reward_hash={}",
            req.unfinished_reward_hash
        );
        return None;
    };

    // 2. backtrack the rc challenge through empty sub-slots, then find prev_b.
    let last_slot_cc_hash = req.challenge_chain_ip_vdf.challenge;
    let target_rc_hash = node
        .slot_state
        .lock()
        .await
        .backtrack_rc_challenge(req.reward_chain_ip_vdf.challenge);
    let prev_b: Option<BlockRecord> = if target_rc_hash == node.constants.genesis_challenge {
        None
    } else {
        let Ok(Some((peak_hash, _))) = node.store.get_peak().await else {
            debug!(
                "infusion point: no peak to backtrack prev block from target_rc_hash={}",
                target_rc_hash
            );
            return None;
        };
        let Ok(Some(peak_rec)) = node.store.get_block_record(&peak_hash).await else {
            return None;
        };
        match backtrack_prev_block(node.store.as_ref(), peak_rec, target_rc_hash).await {
            Some(pb) => pb,
            None => {
                // add_to_future_ip + return: the prev block is not reachable yet. We do
                // not model the future-ip cache; the timelord re-sends on the next NewPeakTimelord.
                warn!(
                    "infusion point: previous block not found (parked; timelord will re-send) target_rc_hash={} infusion={}",
                    target_rc_hash, req.reward_chain_ip_vdf.challenge
                );
                return None;
            }
        }
    };

    // 3. the finished sub-slots from challenge_in_chain to last_slot_cc_hash.
    let challenge_in_chain = match &prev_b {
        None => node.constants.genesis_challenge,
        Some(pb) => match challenge_in_chain(node.store.as_ref(), pb).await {
            Some(c) => c,
            None => {
                debug!("infusion point: challenge_in_chain walk hit a store gap");
                return None;
            }
        },
    };
    let finished_sub_slots = node
        .slot_state
        .lock()
        .await
        .get_finished_sub_slots(challenge_in_chain, last_slot_cc_hash);
    let Some(finished_sub_slots) = finished_sub_slots else {
        debug!("infusion point: finished sub-slots not connected");
        return None;
    };

    // 4. next SSI/difficulty, then SP total-iters from the pos sub-slot start.
    let records = match &prev_b {
        Some(pb) => difficulty_records_map(node, pb).await,
        None => HashMap::new(),
    };
    let (sub_slot_iters, difficulty) = match get_next_sub_slot_iters_and_difficulty(
        &node.constants,
        !finished_sub_slots.is_empty(),
        prev_b.as_ref(),
        &records,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "infusion point: next SSI/difficulty computation failed error={}",
                e
            );
            return None;
        }
    };
    let pos_ss_cc = unfinished.reward_chain_block.pos_ss_cc_challenge_hash;
    let sub_slot_start_iters: u128 = if pos_ss_cc == node.constants.genesis_challenge {
        0
    } else {
        match node.slot_state.lock().await.get_sub_slot(&pos_ss_cc) {
            Some((_, _, start_iters)) => start_iters,
            None => {
                warn!(
                    "infusion point: do not have pos sub-slot, cannot finish pos_ss_cc={}",
                    pos_ss_cc
                );
                return None;
            }
        }
    };
    let sp_iters = match dg_xch_core::consensus::pot_iterations::calculate_sp_iters(
        &node.constants,
        sub_slot_iters,
        unfinished.reward_chain_block.signage_point_index,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!("infusion point: sp_iters computation failed error={}", e);
            return None;
        }
    };
    let sp_total_iters = sub_slot_start_iters + u128::from(sp_iters);

    // get_prev_transaction_block's first return, computed against the store (core holds none).
    let is_transaction_block = match &prev_b {
        None => true,
        Some(pb) => {
            match resolve_prev_linkage(node.store.as_ref(), &node.constants, pb, sp_total_iters)
                .await
            {
                Some(linkage) => linkage.is_transaction_block,
                None => {
                    debug!("infusion point: prev-linkage walk hit a store gap");
                    return None;
                }
            }
        }
    };

    // 5. assemble the FullBlock from the unfinished block + infusion-point VDFs.
    let block = match unfinished_block_to_full_block(
        &unfinished,
        req.challenge_chain_ip_vdf,
        req.challenge_chain_ip_proof.clone(),
        req.reward_chain_ip_vdf,
        req.reward_chain_ip_proof.clone(),
        req.infused_challenge_chain_ip_vdf,
        req.infused_challenge_chain_ip_proof.clone(),
        finished_sub_slots,
        prev_b.as_ref(),
        is_transaction_block,
        difficulty,
    ) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "infusion point: FullBlock assembly failed to hash reward block error={:?}",
                e
            );
            return None;
        }
    };
    // — refuse a pre-farm block whose height is not 0 (invalid pool signature).
    if !has_valid_pool_sig(&node.constants, &block) {
        warn!("infusion point: block has an invalid pool signature; dropping");
        return None;
    }
    Some(block)
}

pub(in crate::node) async fn process_ip_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
    inbound_peers: &PeerMap,
) {
    let reqs: Vec<NewInfusionPointVDF> = node.ip_inbox.lock().await.drain(..).collect();
    for req in reqs {
        // Steps 1-5: look up + assemble + pool-signature check. `None` = any bail.
        let Some(block) = assemble_infusion_block(node, &req).await else {
            continue;
        };

        // — add_block: validate, persist, set peak (raise_on_disconnected). We route
        // through the same follow path a peer's block takes; it fires the S8 farmed-header match (this
        // block IS one we farmed) and returns the new peak.
        let height = block.reward_chain_block.height;
        let partial = req.unfinished_reward_hash;
        match node.follow_step_blocks(std::slice::from_ref(&block)).await {
            Ok(Some((hash, new_height))) => {
                info!(
                    "infused our unfinished block into a FullBlock and set it as the new peak event={} height={} header={} partial={}",
                    "producer.infusion.peak", new_height, hash, partial
                );
                broadcast_new_peak(node, registry, hash, new_height).await;
                update_slot_state_on_peak(node, hash).await;
                broadcast_new_peak_timelord(node, inbound_peers, hash).await;
            }
            Ok(None) => {
                // Validated but did not become the peak (a competing/heavier chain already leads, or we
                // already hold it); no NewPeak in that case.
                info!(
                    "infusion point: assembled block confirmed but did not advance the peak height={} partial={}",
                    height, partial
                );
            }
            Err(e) => {
                // Consensus error validating the block; log and move on (the driver's per-tick
                // NewPeakTimelord broadcast covers the timelord resync).
                warn!(
                    "infusion point: assembled block failed consensus validation error={} height={} partial={}",
                    e, height, partial
                );
            }
        }
    }
}
