use super::*;

pub(in crate::node) fn get_recent_reward_challenges(
    constants: &ConsensusConstants,
    peak: &BlockRecord,
    records: &HashMap<Bytes32, BlockRecord>,
) -> Option<Vec<(Bytes32, u128)>> {
    let limit = 2usize * constants.max_sub_slot_blocks as usize;
    let mut recent_rc: Vec<(Bytes32, u128)> = Vec::new();
    let mut curr = peak;
    while recent_rc.len() < limit {
        if curr.header_hash != peak.header_hash {
            recent_rc.push((curr.reward_infusion_new_challenge, curr.total_iters));
        }
        if curr.first_in_sub_slot() {
            // finished_reward_slot_hashes is Some for a first-in-sub-slot record.
            let hashes = curr.finished_reward_slot_hashes.as_ref()?;
            let mut sub_slot_total_iters = curr.ip_sub_slot_total_iters(constants).ok()?;
            for rc in hashes.iter().rev() {
                if sub_slot_total_iters < u128::from(curr.sub_slot_iters) {
                    break;
                }
                recent_rc.push((*rc, sub_slot_total_iters));
                sub_slot_total_iters -= u128::from(curr.sub_slot_iters);
            }
        }
        if curr.height == 0 {
            break;
        }
        curr = records.get(&curr.prev_hash)?;
    }
    recent_rc.reverse();
    Some(recent_rc)
}

// send_peak_to_timelords. On every new peak the full node hands
// its timelords a NewPeakTimelord so they can begin infusing on top of it — the peak counterpart of
// broadcast_ub_timelord_announcements (timelords connect INBOUND, so this walks the inbound PeerMap
// under a NodeType::Timelord filter). Every field is fully derived: the in-slot
// difficulty, the peak record's deficit/sub_slot_iters, the next sub-epoch summary, the recent reward
// challenges, the last challenge-block-or-EOS total iters, and the passed-ses-height flag. Bails (sends
// nothing) if any derivation cannot be grounded in the loaded record window rather than shipping an
// approximate message. Fire-and-forget: a timelord that misses one gets the next peak.
pub(in crate::node) async fn broadcast_new_peak_timelord<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    inbound_peers: &PeerMap,
    peak_hash: Bytes32,
) {
    // Snapshot inbound timelord peers first (cheap Arc clones); nothing to do without one.
    let mut timelords: Vec<Arc<SocketPeer>> = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Timelord {
            timelords.push(peer.clone());
        }
    }
    if timelords.is_empty() {
        return;
    }
    let Some(new_peak) = build_new_peak_timelord(
        node.store.as_ref(),
        &node.constants,
        &node.record_window,
        &node.sync_metrics,
        peak_hash,
    )
    .await
    else {
        return;
    };
    send_new_peak_timelord(node, &timelords, &new_peak).await;
}

// The construction half of `send_peak_to_timelords` — shared by the peak broadcast above and the
// on-connect TIMELORD greeting ([`FullNodeApi::timelord_peak`]).
pub(in crate::node) async fn build_new_peak_timelord<S: BlockStore + Send + Sync>(
    store: &S,
    constants: &ConsensusConstants,
    record_window: &Mutex<BlockRecordCache>,
    sync_metrics: &SyncMetrics,
    peak_hash: Bytes32,
) -> Option<Box<NewPeakTimelord>> {
    let peak_block = store.get_block(&peak_hash).await.ok().flatten()?;
    let peak = store.get_block_record(&peak_hash).await.ok().flatten()?;
    // Depth must cover the deepest of the walks here: the difficulty computation's
    // can_finish_sub_and_full_epoch scan (up to 383 back mid-epoch, ~5120 at an epoch turn — a
    // fixed 512 fails both late-sub-epoch and epoch-turn peaks), passes_ses
    // (< sub_epoch_blocks = 384) and get_recent_reward_challenges (< 2*max_sub_slot_blocks = 256).
    // difficulty_record_depth's floor of 513 dominates the latter two.
    let records = crate::record_window::windowed_records_map(
        record_window,
        store,
        sync_metrics,
        constants,
        &peak,
    )
    .await;

    // difficulty: get_next_sub_slot_iters_and_difficulty(peak, False)[1], including the
    // height<=2 short-circuit to the starting difficulty.
    let difficulty = if peak.height <= 2 {
        constants.difficulty_starting
    } else {
        let Ok((_ssi, diff)) =
            get_next_sub_slot_iters_and_difficulty(constants, false, Some(&peak), &records)
        else {
            return None;
        };
        diff
    };

    // sub_epoch_summary: next_sub_epoch_summary takes an UnfinishedBlock; reconstruct one from
    // the peak FullBlock — it reads only signage_point_index, the prev-block hash, finished_sub_slots
    // and total_iters, all preserved by RewardChainBlock::get_unfinished and the shared foliage.
    let unfinished_peak = UnfinishedBlock {
        finished_sub_slots: peak_block.finished_sub_slots.clone(),
        reward_chain_block: peak_block.reward_chain_block.get_unfinished(),
        challenge_chain_sp_proof: peak_block.challenge_chain_sp_proof.clone(),
        reward_chain_sp_proof: peak_block.reward_chain_sp_proof.clone(),
        foliage: peak_block.foliage,
        foliage_transaction_block: peak_block.foliage_transaction_block,
        transactions_info: peak_block.transactions_info.clone(),
        transactions_generator: peak_block.transactions_generator.clone(),
        transactions_generator_ref_list: peak_block.transactions_generator_ref_list.clone(),
    };
    let sub_epoch_summary = next_sub_epoch_summary(
        constants,
        &records,
        peak.required_iters,
        &unfinished_peak,
        true,
    )
    .unwrap_or(None);

    // previous_reward_challenges: `blockchain.get_recent_reward_challenges`().
    let previous_reward_challenges = get_recent_reward_challenges(constants, &peak, &records)?;

    // last_challenge_sb_or_eos_total_iters: walk back to the last
    // challenge-block or first-in-sub-slot record; take its total_iters (challenge block) or the total
    // iters at the start of its infusion sub-slot (end-of-sub-slot case).
    let mut curr = &peak;
    while !curr.is_challenge_block(constants.min_blocks_per_challenge_block)
        && !curr.first_in_sub_slot()
    {
        curr = records.get(&curr.prev_hash)?;
    }
    let last_challenge_sb_or_eos_total_iters =
        if curr.is_challenge_block(constants.min_blocks_per_challenge_block) {
            curr.total_iters
        } else {
            curr.ip_sub_slot_total_iters(constants).ok()?
        };

    // passes_ses_height_but_not_yet_included: true unless a sub-epoch summary
    // was already included at or after the last sub-epoch-block height boundary.
    let mut curr = &peak;
    let mut passes_ses_height_but_not_yet_included = true;
    while curr.height % constants.sub_epoch_blocks != 0 {
        if curr.sub_epoch_summary_included.is_some() {
            passes_ses_height_but_not_yet_included = false;
        }
        curr = records.get(&curr.prev_hash)?;
    }
    if curr.sub_epoch_summary_included.is_some() || curr.height == 0 {
        passes_ses_height_but_not_yet_included = false;
    }

    Some(Box::new(NewPeakTimelord {
        reward_chain_block: peak_block.reward_chain_block.clone(),
        difficulty,
        deficit: peak.deficit,
        sub_slot_iters: peak.sub_slot_iters,
        sub_epoch_summary,
        previous_reward_challenges,
        last_challenge_sb_or_eos_total_iters,
        passes_ses_height_but_not_yet_included,
    }))
}

// The delivery half: NewPeakTimelord to every snapshotted timelord peer. Fire-and-forget: a
// timelord that misses one gets the next peak.
pub(in crate::node) async fn send_new_peak_timelord<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    timelords: &[Arc<SocketPeer>],
    new_peak: &NewPeakTimelord,
) {
    for peer in timelords {
        let version = *peer.protocol_version.read().await;
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakTimelord,
            version,
            new_peak,
            None,
        ) else {
            continue;
        };
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakTimelord,
            msg.data.as_slice().len(),
        );
        let _ = peer.send(msg).await;
    }
    info!(
        "new peak announced to timelord peers event={} height={} timelords={}",
        "producer.peak.timelord_broadcast",
        new_peak.reward_chain_block.height,
        timelords.len()
    );
}

// Drain the tx-announce queue into NewTransaction broadcasts to EVERY connected full-node peer —
// outbound AND inbound — excluding each transaction's origin peer.
// Fire-and-forget like the peak announcement: a peer that misses one can still pull the bundle
// after any other node re-announces it.
//
// Origin-exclusion id space: inbound peers are keyed by their client-cert hash, so an
// inbound-sourced transaction never echoes to its origin. OUTBOUND connections all share one
// local dispatch id (OUR client-cert hash — clients/src/websocket/mod.rs:205), so an
// outbound-sourced transaction may still echo to its origin: a benign redundancy (the origin
// holds the item, the cost/fee consistency check passes, the announce is ignored) pending
// per-connection identity plumbing on the dial path.
