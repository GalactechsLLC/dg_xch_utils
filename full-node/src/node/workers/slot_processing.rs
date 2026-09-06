use super::*;

// Ancestor window sized for get_next_sub_slot_iters_and_difficulty — and the SES machinery that
// shares its walks — anchored at `anchor` (the UB's parent, or the peak), depth per
// difficulty_record_depth (513..=896 mid-epoch, 5,121..=5,503 across an epoch turn and its first
// sub-epoch). Served from the node's in-memory record window (record_window.rs) with store
// fallback per miss — never a per-call DB walk, which would re-fetch the whole window from the
// backend on every peak (a throughput trough on the Postgres legs).
pub(in crate::node) async fn difficulty_records_map<S: BlockStore + Send + Sync>(
    node: &FullNode<S>,
    anchor: &BlockRecord,
) -> HashMap<Bytes32, BlockRecord> {
    crate::record_window::windowed_records_map(
        &node.record_window,
        node.store.as_ref(),
        &node.sync_metrics,
        &node.constants,
        anchor,
    )
    .await
}

// `blockchain.get_sp_and_ip_sub_slots`: the EOS bundles ending the slots the peak's signage
// point and infusion sit in. Walks records back to the last first-in-sub-slot block and reads its
// stored finished sub-slots; the overflow case additionally needs the previous slot's bundle.
pub(in crate::node) async fn sp_and_ip_sub_slots<S: BlockStore + Send + Sync>(
    store: &S,
    peak_hash: Bytes32,
) -> Option<(Option<EndOfSubSlotBundle>, Option<EndOfSubSlotBundle>)> {
    let peak = store.get_block_record(&peak_hash).await.ok().flatten()?;
    let is_overflow = peak.overflow;
    let mut curr_br = peak;
    while !curr_br.first_in_sub_slot() && curr_br.height > 0 {
        curr_br = store
            .get_block_record(&curr_br.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    let curr = store.get_block(&curr_br.header_hash).await.ok().flatten()?;
    if curr.finished_sub_slots.is_empty() {
        // Reached genesis with no sub-slots yet.
        return Some((None, None));
    }
    let ip_sub_slot = curr.finished_sub_slots.last()?.clone();
    if !is_overflow {
        // The PoS sub-slot is the infusion sub-slot.
        return Some((None, Some(ip_sub_slot)));
    }
    if curr.finished_sub_slots.len() > 1 {
        let sp = curr.finished_sub_slots[curr.finished_sub_slots.len() - 2].clone();
        return Some((Some(sp), Some(ip_sub_slot)));
    }
    // Overflow with a single finished slot: the SP slot ended at the PREVIOUS first-in-sub-slot
    // block's last bundle.
    let mut prev_br = match store.get_block_record(&curr.prev_header_hash()).await {
        Ok(Some(rec)) => rec,
        _ => return Some((None, Some(ip_sub_slot))), // curr is genesis
    };
    while prev_br.height > 0 && !prev_br.first_in_sub_slot() {
        prev_br = store
            .get_block_record(&prev_br.prev_hash)
            .await
            .ok()
            .flatten()?;
    }
    let prev_curr = store.get_block(&prev_br.header_hash).await.ok().flatten()?;
    match prev_curr.finished_sub_slots.last() {
        Some(bundle) => Some((Some(bundle.clone()), Some(ip_sub_slot))),
        None => Some((None, Some(ip_sub_slot))),
    }
}

// The relay announcement for an accepted signage point. Only
// index > 0 SPs reach this path (they are appended by `new_signage_point`, which rejects index 0), so
// the VDFs are present; `None` guards a malformed all-None SP and simply skips the announce.
pub(in crate::node) fn announce_for_sp(
    state: &SlotState,
    index: u8,
    sp: &SignagePoint,
) -> Option<NewSignagePointOrEndOfSubSlot> {
    let cc_vdf = sp.cc_vdf.as_ref()?;
    let rc_vdf = sp.rc_vdf.as_ref()?;
    let prev_challenge = state.get_sub_slot(&cc_vdf.challenge).map(|(eos, _, _)| {
        eos.challenge_chain
            .challenge_chain_end_of_slot_vdf
            .challenge
    });
    Some(NewSignagePointOrEndOfSubSlot {
        prev_challenge_hash: prev_challenge,
        challenge_hash: cc_vdf.challenge,
        index_from_challenge: index,
        last_rc_infusion: rc_vdf.challenge,
    })
}

// The farmer-form signage point for an accepted SP (farmer_protocol.NewSignagePoint).
// The challenge hash is the SP's sub-slot cc challenge, same as the gossip
// announce; difficulty/SSI come from the accept site's next-SSI context so a farmer can size the
// plot filter. `None` only if the SP's VDF outputs fail to hash.
pub(in crate::node) fn farmer_announce_for_sp(
    sp: &SignagePoint,
    index: u8,
    difficulty: u64,
    sub_slot_iters: u64,
    peak_height: u32,
    last_tx_height: u32,
) -> Option<NewSignagePoint> {
    new_signage_point_for_farmers(
        sp,
        sp.cc_vdf.as_ref()?.challenge,
        difficulty,
        sub_slot_iters,
        index,
        peak_height,
        last_tx_height,
    )
}

// (peak_height, last_transaction_block_height) for the farmer signage-point context: a tx-block
// peak is its own last-tx height; otherwise the peak carries the previous one. No peak (pre-genesis)
// is (0, 0).
pub(in crate::node) fn farmer_heights(peak: Option<&BlockRecord>) -> (u32, u32) {
    match peak {
        Some(rec) if rec.is_transaction_block() => (rec.height, rec.height),
        Some(rec) => (rec.height, rec.prev_transaction_block_height),
        None => (0, 0),
    }
}

// The relay announcement for an accepted end-of-sub-slot (index 0 by protocol convention).
pub(in crate::node) fn announce_for_eos(
    eos: &EndOfSubSlotBundle,
) -> Option<NewSignagePointOrEndOfSubSlot> {
    Some(NewSignagePointOrEndOfSubSlot {
        prev_challenge_hash: Some(
            eos.challenge_chain
                .challenge_chain_end_of_slot_vdf
                .challenge,
        ),
        challenge_hash: eos.challenge_chain.hash().ok()?,
        index_from_challenge: 0,
        last_rc_infusion: eos.reward_chain.end_of_slot_vdf.challenge,
    })
}

// The farmer-form index-0 signage point for a newly-finished sub-slot.
// A sub-slot start has no cc/rc SP VDF, so sp_source_data carries the challenge/reward SUB-SLOTS
// (sub_slot_data), not vdf_data; the SP hashes ARE the sub-slot hashes. The farmer counterpart of
// announce_for_eos (which serves the full-node NewSignagePointOrEndOfSubSlot). None only on hash fail.
pub(in crate::node) fn farmer_announce_for_eos(
    eos: &EndOfSubSlotBundle,
    difficulty: u64,
    sub_slot_iters: u64,
    peak_height: u32,
    last_tx_height: u32,
) -> Option<NewSignagePoint> {
    let cc_hash = eos.challenge_chain.hash().ok()?;
    let rc_hash = eos.reward_chain.hash().ok()?;
    Some(NewSignagePoint {
        challenge_hash: cc_hash,
        challenge_chain_sp: cc_hash,
        reward_chain_sp: rc_hash,
        difficulty,
        sub_slot_iters,
        signage_point_index: 0,
        peak_height,
        last_tx_height,
        sp_source_data: Some(SignagePointSourceData {
            sub_slot_data: Some(SPSubSlotSourceData {
                cc_sub_slot: eos.challenge_chain,
                rc_sub_slot: eos.reward_chain,
            }),
            vdf_data: None,
        }),
    })
}

// Reset the slot state around a just-confirmed peak and queue relay announcements for anything
// the future caches released.
pub(in crate::node) async fn update_slot_state_on_peak<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    peak_hash: Bytes32,
) {
    let Ok(Some(rec)) = node.store.get_block_record(&peak_hash).await else {
        return;
    };
    let Some((sp_ss, ip_ss)) = sp_and_ip_sub_slots(node.store.as_ref(), peak_hash).await else {
        return;
    };
    let blocks = difficulty_records_map(node, &rec).await;
    let (next_ssi, next_diff) = match get_next_sub_slot_iters_and_difficulty(
        &node.constants,
        true,
        Some(&rec),
        &blocks,
    ) {
        Ok(v) => v,
        Err(e) => {
            // This computation cannot fail for a connected peak; a failure here means the
            // record walk broke mid-chain. Skipping this peak's slot-state reset is strictly
            // safer than a fallback,
            // which fed difficulty 0 into the slot state and the farmer announcements.
            warn!(
                "slot-state peak update skipped: next SSI/difficulty computation failed event={} peak={} error={}",
                "slot_state.ssi_difficulty_fail", peak_hash, e
            );
            return;
        }
    };
    let mut state = node.slot_state.lock().await;
    let (new_eos, new_sps) = state.new_peak(
        &rec,
        PeakSlotContext {
            sp_sub_slot: sp_ss.as_ref(),
            ip_sub_slot: ip_ss.as_ref(),
            fork_block: None,
        },
        &blocks,
        next_ssi,
        next_diff,
        false,
    );
    // The advancing peak also obsoletes cached unfinished blocks at or below it.
    node.unfinished.lock().await.prune_below(rec.height);
    let (peak_height, last_tx_height) = farmer_heights(Some(&rec));
    let mut announces = node.sp_announce.lock().await;
    let mut farmer_announces = node.sp_farmer_announce.lock().await;
    if let Some(eos) = new_eos.as_ref() {
        if let Some(a) = announce_for_eos(eos) {
            announces.push(a);
        }
        if let Some(fa) =
            farmer_announce_for_eos(eos, next_diff, next_ssi, peak_height, last_tx_height)
        {
            farmer_announces.push(fa);
        }
    }
    for (index, sp) in &new_sps {
        if let Some(a) = announce_for_sp(&state, *index, sp) {
            announces.push(a);
        }
        if let Some(fa) =
            farmer_announce_for_sp(sp, *index, next_diff, next_ssi, peak_height, last_tx_height)
        {
            farmer_announces.push(fa);
        }
    }
}

// Validate every received slot-gossip payload into the state machine and queue relays for what
// was accepted — the driver half of respond_signage_point / respond_end_of_sub_slot.
pub(in crate::node) async fn process_sp_inbox<S: BlockStore + CoinStore + Send + Sync + 'static>(
    node: &Arc<FullNode<S>>,
) {
    let events: Vec<SpEvent> = node.sp_inbox.lock().await.drain(..).collect();
    if events.is_empty() {
        return;
    }
    let peak = match node.store.get_peak().await {
        Ok(Some((hash, _))) => node.store.get_block_record(&hash).await.ok().flatten(),
        _ => None,
    };
    let blocks = match &peak {
        Some(rec) => difficulty_records_map(node, rec).await,
        None => HashMap::new(),
    };
    // A None peak yields the starting SSI/difficulty (never an Err). A connected peak cannot fail
    // either — so an Err here means the record walk broke mid-chain: drop the drained batch
    // (SP gossip is redundant across peers and ticks) rather than process it under difficulty 0,
    // a poisoning fallback.
    let (next_ssi, next_diff) = match get_next_sub_slot_iters_and_difficulty(
        &node.constants,
        true,
        peak.as_ref(),
        &blocks,
    ) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "signage-point batch dropped: next SSI/difficulty computation failed event={} error={}",
                "sp.ssi_difficulty_fail", e
            );
            return;
        }
    };
    let (peak_height, last_tx_height) = farmer_heights(peak.as_ref());
    let mut state = node.slot_state.lock().await;
    let mut announces = node.sp_announce.lock().await;
    let mut farmer_announces = node.sp_farmer_announce.lock().await;
    for event in events {
        match event {
            SpEvent::SignagePoint(sp) => {
                // A pulled RespondSignagePoint always carries real VDFs (index > 0); wrap them in the
                // now-optional SignagePoint fields (the stored SP form).
                let point = SignagePoint {
                    cc_vdf: Some(sp.challenge_chain_vdf),
                    cc_proof: Some(sp.challenge_chain_proof.clone()),
                    rc_vdf: Some(sp.reward_chain_vdf),
                    rc_proof: Some(sp.reward_chain_proof.clone()),
                };
                if state.new_signage_point(
                    sp.index_from_challenge,
                    &blocks,
                    peak.as_ref(),
                    next_ssi,
                    &point,
                    false,
                ) {
                    node.sp_current_index
                        .store(u32::from(sp.index_from_challenge), Ordering::Relaxed);
                    node.signage_points_total
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if let (Ok(cc), Ok(rc)) = (
                        sp.challenge_chain_vdf.output.hash(),
                        sp.reward_chain_vdf.output.hash(),
                    ) {
                        info!(
                            "finished signage point index={} cc={} rc={}",
                            sp.index_from_challenge, cc, rc
                        );
                    }
                    if let Some(a) = announce_for_sp(&state, sp.index_from_challenge, &point) {
                        announces.push(a);
                    }
                    if let Some(fa) = farmer_announce_for_sp(
                        &point,
                        sp.index_from_challenge,
                        next_diff,
                        next_ssi,
                        peak_height,
                        last_tx_height,
                    ) {
                        farmer_announces.push(fa);
                    }
                }
            }
            SpEvent::EndOfSubSlot(eos) => {
                if state
                    .new_finished_sub_slot(
                        &eos.end_of_slot_bundle,
                        &blocks,
                        peak.as_ref(),
                        next_ssi,
                        next_diff,
                        false,
                    )
                    .is_some()
                {
                    // The "finished sub slot" INFO line, keyed by the challenge-chain hash.
                    if let Ok(cc) = eos.end_of_slot_bundle.challenge_chain.hash() {
                        info!("finished sub slot cc={}", cc);
                    }
                    if let Some(a) = announce_for_eos(&eos.end_of_slot_bundle) {
                        announces.push(a);
                    }
                    if let Some(fa) = farmer_announce_for_eos(
                        &eos.end_of_slot_bundle,
                        next_diff,
                        next_ssi,
                        peak_height,
                        last_tx_height,
                    ) {
                        farmer_announces.push(fa);
                    }
                }
            }
        }
    }
}
