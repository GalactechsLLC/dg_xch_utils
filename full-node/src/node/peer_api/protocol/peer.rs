use super::*;

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    pub(super) async fn block_by_height(&self, height: u32) -> Option<Box<FullBlock>> {
        let rec = self
            .store
            .get_block_record_by_height(height)
            .await
            .ok()
            .flatten()?;
        self.store
            .get_block(&rec.header_hash)
            .await
            .ok()
            .flatten()
            .map(Box::new)
    }

    // — the RequestBlocks serving cap comes from the network's consensus
    // constants (MAX_BLOCK_COUNT_PER_REQUESTS = 32, override-tunable),
    // exactly like the request_header_blocks cap this server already enforces.
    pub(super) fn max_block_count_per_requests(&self) -> u32 {
        self.constants.max_block_count_per_requests
    }

    // The decode-time list caps resolve from the trusted-peer policy — the same values the
    // handlers enforce post-parse, so decode truncation and handler truncation can never
    // disagree.
    pub(super) fn max_subscriptions(&self, peer: &Bytes32, host: Option<IpAddr>) -> u32 {
        u32::try_from(self.wallet.max_subscriptions(peer, host)).unwrap_or(u32::MAX)
    }
    pub(super) fn max_subscribe_response_items(&self, peer: &Bytes32, host: Option<IpAddr>) -> u32 {
        u32::try_from(self.trust.max_subscribe_response_items(peer, host)).unwrap_or(u32::MAX)
    }

    // An inbound TIMELORD is accepted from localhost or an exempt network only; the trusted-CIDR
    // list defaults empty, so localhost alone. Host-based only — node-id trust does not apply.
    pub(super) fn accept_inbound_timelord(&self, host: Option<IpAddr>) -> bool {
        self.trust.host_trusted(host)
    }

    pub(super) async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        self.known_peers.read().await.clone()
    }

    pub(super) async fn on_new_peak(&self, peer: Bytes32, peak: NewPeak) {
        // `full_node.new_peak`: record this peer's claim FIRST (sync_store.peer_has_block), weight
        // included — WEIGHT is the fork-choice ordering key, and the newest announcement REPLACES the
        // peer's previous claim (which is also how an over-claim is withdrawn). Outbound connections
        // key by the minted per-connection guard (the dispatch peer id there is our own cert hash);
        // the shared inbound api keys by the real inbound peer id.
        let (key, inbound) = match &self.claim_guard {
            Some(guard) => (guard.key(), false),
            None => (peer, true),
        };
        let changed = self.peak_book.record(
            key,
            inbound,
            PeakClaim {
                header_hash: peak.header_hash,
                height: peak.height,
                weight: peak.weight,
            },
        );
        // Event-driven near-tip follow: a change of the
        // heaviest claim wakes the tip_follower to close the gap block-by-block. notify_one stores a
        // permit if the follower is busy, so an advance is never missed.
        if changed {
            self.new_peak_signal.notify_one();
        }
    }

    pub(super) async fn transaction(&self, id: Bytes32) -> Option<SpendBundle> {
        self.mempool.lock().await.spend_bundle(&id)
    }

    pub(super) async fn on_new_transaction(
        &self,
        _peer: Bytes32,
        tx: NewTransaction,
    ) -> TransactionAnnounceAction {
        if !self.synced.load(Ordering::Relaxed) {
            return TransactionAnnounceAction::Ignore;
        }
        // Behavior (b) — "It's not reasonable to advertise a transaction with zero
        // cost." A zero-cost announcement is a protocol violation; ban the peer.
        if tx.cost == 0 {
            warn!(
                "banning peer: zero-cost transaction announcement id={}",
                tx.transaction_id
            );
            return TransactionAnnounceAction::Ban;
        }
        // Pre-filter: an announcement above the block cost ceiling can never be admitted, so it
        // is not worth a round trip. Ignore (not a ban — a mis-costed advert is not the
        // zero-cost violation).
        if tx.cost > self.constants.max_block_cost_clvm {
            return TransactionAnnounceAction::Ignore;
        }
        // Behavior (c) — if we already hold a VALIDATED mempool item for this id,
        // the announced cost/fee must match our own validation. One specific diff is tolerated:
        // pre-2.4.3 peers fold the quote's byte+execution cost into the advertised cost, so
        // `mempool_item.cost + (QUOTE_BYTES * COST_PER_BYTE + QUOTE_EXECUTION_COST)` is also
        // accepted. Any other cost, or any fee mismatch, is a ban.
        let seen = {
            let mempool = self.mempool.lock().await;
            mempool
                .get(&tx.transaction_id)
                .map(|item| (item.cost, item.fee))
        };
        if let Some((item_cost, item_fee)) = seen {
            const QUOTE_BYTES: u64 = 2;
            const QUOTE_EXECUTION_COST: u64 = 20;
            let tolerated = QUOTE_BYTES * self.constants.cost_per_byte + QUOTE_EXECUTION_COST;
            let cost_ok = tx.cost == item_cost || tx.cost == item_cost + tolerated;
            if !cost_ok || tx.fees != item_fee {
                warn!(
                    "banning peer: already-seen tx with mismatched cost/fee id={} advertised_cost={} validation_cost={} advertised_fee={} validation_fee={}",
                    tx.transaction_id, tx.cost, item_cost, tx.fees, item_fee
                );
                return TransactionAnnounceAction::Ban;
            }
            // Already seen and consistent (`return None` after the match check).
            return TransactionAnnounceAction::Ignore;
        }
        // Behavior (d) — `is_fee_enough`: the whole request path is
        // gated on the ADVERTISED fee being able to get in. With room in the pool anything
        // passes; at capacity the fee must clear the nonzero floor (5 fpc) and strictly beat the
        // pool's min fee rate — otherwise the bundle is never fetched (spam CLVM protection).
        if !self.mempool.lock().await.is_fee_enough(tx.fees, tx.cost) {
            return TransactionAnnounceAction::Ignore;
        }
        // New to us. A live (non-expired) entry means a fetch for this id is already in flight
        // from another peer: ignore the duplicate advert.
        // Otherwise record the request instant and pull.
        {
            let mut pending = self.tx_requested.lock().await;
            if pending
                .get(&tx.transaction_id)
                .is_some_and(|t| t.at.elapsed() < REQUEST_TIMEOUT)
            {
                return TransactionAnnounceAction::Ignore;
            }
            // Record the request instant AND this peer's advertised fee/cost — the untrusted
            // tx-queue lane orders on them. A later announcer for this id is deduped above, so the
            // first announcer's advertised values steer the order (not the max fpc across
            // announcers — a documented delta).
            pending.insert(
                tx.transaction_id,
                PendingTx {
                    at: Instant::now(),
                    advertised_fee: tx.fees,
                    advertised_cost: tx.cost,
                },
            );
        }
        TransactionAnnounceAction::Request(RequestTransaction {
            transaction_id: tx.transaction_id,
        })
    }

    pub(super) async fn on_respond_transaction(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        tx: SpendBundle,
    ) {
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        // Behavior (d) — respond_transaction: a body is accepted only if it answers
        // a pull WE issued. `tx_requested` is our `pending_tx_request` set (a RequestTransaction
        // was sent for this id). An unsolicited body — no matching pending entry — is dropped
        // before it can reach the validator. `remove` consumes the entry, so a second copy of the
        // same id is then unsolicited too.
        let Ok(name) = tx.name() else {
            return;
        };
        let Some(pending) = self.tx_requested.lock().await.remove(&name) else {
            debug!(
                "dropping unsolicited transaction body id={} peer={}",
                name, peer
            );
            return;
        };
        // Record the origin (peer id + remote host) BEFORE queueing so the announce drain, which may
        // run concurrently with the validator worker's admission, can exclude the peer this bundle
        // arrived from. The host is what makes an OUTBOUND
        // origin excludable — its dispatch peer id is our shared client-cert hash. A bundle that
        // later fails admission produces no announcement, so its origin
        // is never consumed and simply ages out (bounded — see record_tx_origin).
        record_tx_origin(
            &self.tx_origin,
            name,
            TxOrigin {
                peer_id: peer,
                host,
            },
        )
        .await;
        // The CLVM run does NOT happen here — the websocket read loop must never carry
        // validation work. Bundles queue into the bounded inbox the
        // validator worker drains. A trusted peer's bundle takes the unbounded high-priority lane
        // (`TransactionQueue.put(high_priority=is_trusted(peer))`); an untrusted
        // bundle is admitted to the capped lane only within this peer's share, else dropped, and the
        // lane orders by the advertised fee-per-cost carried on the pending request.
        let high_priority = self.trust.is_trusted(&peer, host);
        self.tx_inbox.lock().await.push(
            peer,
            tx,
            high_priority,
            pending.advertised_fee,
            pending.advertised_cost,
        );
    }

    pub(super) async fn on_new_signage_point_or_eos(
        &self,
        _peer: Bytes32,
        ann: NewSignagePointOrEndOfSubSlot,
    ) -> Option<RequestSignagePointOrEndOfSubSlot> {
        // Gated on sync mode: a syncing node's slot list anchors at its local
        // peak, so tip-context objects can never validate — skip the round trip.
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        // Pull only what the slot state does not hold and is not outdated. (A walk-up-to-30-EOS
        // backwards catch-up for a diverged slot list is a noted follow-up — a missed slot here
        // self-heals at the next confirmed peak.)
        let state = self.slot_state.lock().await;
        if ann.index_from_challenge == 0 {
            if state.get_sub_slot(&ann.challenge_hash).is_some() {
                return None;
            }
        } else if state
            .get_signage_point_by_index(
                &ann.challenge_hash,
                ann.index_from_challenge,
                &ann.last_rc_infusion,
            )
            .is_some()
        {
            return None;
        }
        if state.have_newer_signage_point(
            &ann.challenge_hash,
            ann.index_from_challenge,
            &ann.last_rc_infusion,
        ) {
            return None;
        }
        Some(RequestSignagePointOrEndOfSubSlot {
            challenge_hash: ann.challenge_hash,
            index_from_challenge: ann.index_from_challenge,
            last_rc_infusion: ann.last_rc_infusion,
        })
    }

    pub(super) async fn signage_point_or_eos(
        &self,
        req: RequestSignagePointOrEndOfSubSlot,
    ) -> Option<SignagePointResponse> {
        // request_signage_point_or_end_of_sub_slot: index 0 serves the EOS bundle
        // itself, any other index the cached signage point built on the requested infusion.
        let state = self.slot_state.lock().await;
        if req.index_from_challenge == 0 {
            let (eos, _, _) = state.get_sub_slot(&req.challenge_hash)?;
            return Some(SignagePointResponse::EndOfSubSlot(Box::new(
                RespondEndOfSubSlot {
                    end_of_slot_bundle: eos.clone(),
                },
            )));
        }
        let sp = state.get_signage_point_by_index(
            &req.challenge_hash,
            req.index_from_challenge,
            &req.last_rc_infusion,
        )?;
        // index > 0 (guaranteed by the branch above) always resolves a stored SP with real VDFs;
        // RespondSignagePoint carries non-optional VDFs, so the
        // `?` here only guards a malformed all-None SP, which get_signage_point_by_index never returns.
        Some(SignagePointResponse::SignagePoint(Box::new(
            RespondSignagePoint {
                index_from_challenge: req.index_from_challenge,
                challenge_chain_vdf: sp.cc_vdf?,
                challenge_chain_proof: sp.cc_proof.clone()?,
                reward_chain_vdf: sp.rc_vdf?,
                reward_chain_proof: sp.rc_proof.clone()?,
            },
        )))
    }

    pub(super) async fn on_respond_signage_point(&self, _peer: Bytes32, sp: RespondSignagePoint) {
        let mut inbox = self.sp_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(SpEvent::SignagePoint(Box::new(sp)));
        }
    }

    pub(super) async fn on_respond_end_of_sub_slot(
        &self,
        _peer: Bytes32,
        eos: RespondEndOfSubSlot,
    ) {
        let mut inbox = self.sp_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(SpEvent::EndOfSubSlot(Box::new(eos)));
        }
    }

    pub(super) async fn on_new_unfinished_block(
        &self,
        _peer: Bytes32,
        ann: NewUnfinishedBlock,
    ) -> Option<RequestUnfinishedBlock> {
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        // The v1 announce carries no foliage hash — pull only when we hold and request nothing
        // for this reward hash.
        let mut cache = self.unfinished.lock().await;
        if cache.get_block(&ann.unfinished_reward_hash).is_some() {
            return None;
        }
        let (requesting, count) = cache.is_requesting(&ann.unfinished_reward_hash, None);
        if requesting || count > 0 {
            return None;
        }
        cache.mark_requesting(ann.unfinished_reward_hash, None);
        Some(RequestUnfinishedBlock {
            unfinished_reward_hash: ann.unfinished_reward_hash,
        })
    }

    pub(super) async fn on_new_unfinished_block2(
        &self,
        _peer: Bytes32,
        ann: NewUnfinishedBlock2,
    ) -> Option<RequestUnfinishedBlock2> {
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        // `new_unfinished_block2`'s admission ladder: already held, a better variant held,
        // too many variants held, already fetching, or too many fetches in flight — all ignore.
        let mut cache = self.unfinished.lock().await;
        let (entry, count, have_better) =
            cache.get_block2(&ann.unfinished_reward_hash, ann.foliage_hash.as_ref());
        if entry.is_some() || have_better || count > MAX_DUPLICATE_UNFINISHED_BLOCKS {
            return None;
        }
        let (requesting, count) =
            cache.is_requesting(&ann.unfinished_reward_hash, ann.foliage_hash.as_ref());
        if requesting || count >= MAX_DUPLICATE_UNFINISHED_BLOCKS {
            return None;
        }
        cache.mark_requesting(ann.unfinished_reward_hash, ann.foliage_hash);
        Some(RequestUnfinishedBlock2 {
            unfinished_reward_hash: ann.unfinished_reward_hash,
            foliage_hash: ann.foliage_hash,
        })
    }

    pub(super) async fn unfinished_block(
        &self,
        reward_hash: Bytes32,
    ) -> Option<Box<UnfinishedBlock>> {
        self.unfinished
            .lock()
            .await
            .get_block(&reward_hash)
            .cloned()
            .map(Box::new)
    }

    pub(super) async fn unfinished_block2(
        &self,
        reward_hash: Bytes32,
        foliage_hash: Option<Bytes32>,
    ) -> Option<Box<UnfinishedBlock>> {
        self.unfinished
            .lock()
            .await
            .get_block2(&reward_hash, foliage_hash.as_ref())
            .0
            .cloned()
            .map(Box::new)
    }

    pub(super) async fn on_respond_unfinished_block(&self, block: Box<UnfinishedBlock>) {
        let mut inbox = self.ub_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(*block);
        }
    }

    pub(super) async fn mempool_items(&self, filter: Vec<u8>) -> Vec<NewTransaction> {
        // Decode the peer's BIP158 filter and serve up to `limit` (100) highest-fee items NOT in
        // it, scanning at most `max_checked` (5000). A malformed filter decodes to None and we
        // serve unfiltered — over-announcing is the safe superset (the peer's own dedup absorbs it).
        let decoded = dg_xch_core::consensus::block_filter::decode_chia_block_filter(&filter);
        let mp = self.mempool.lock().await;
        let mut out = Vec::new();
        for (checked, item) in mp.items_by_fee().into_iter().enumerate() {
            if out.len() >= 100 || checked >= 5000 {
                break;
            }
            let name_bytes = SizedBytes::bytes(&item.name);
            if let Some(decoded) = &decoded
                && dg_xch_core::consensus::block_filter::chia_block_filter_match(
                    decoded,
                    &name_bytes,
                )
            {
                continue;
            }
            out.push(NewTransaction {
                transaction_id: item.name,
                cost: item.cost,
                fees: item.fee,
            });
        }
        out
    }

    pub(super) async fn on_request_proof_of_weight(
        &self,
        peer: Bytes32,
        req: RequestProofOfWeight,
        id: Option<u16>,
        peers: PeerMap,
    ) {
        // Queue-only, off the read path: the wp worker builds (single-flight per tip inside the
        // WeightProofServer's lock) and responds. Bounded + deduped — a repeat
        // {peer, tip} would be one build + one response anyway.
        let mut inbox = self.wp_inbox.lock().await;
        if inbox.len() >= WP_INBOX_CAP {
            return;
        }
        if inbox.iter().any(|r| r.peer == peer && r.tip == req.tip) {
            return;
        }
        inbox.push(WpRequest {
            peer,
            peers,
            tip: req.tip,
            id,
        });
    }

    pub(super) async fn compact_vdf(&self, req: RequestCompactVDF) -> Option<RespondCompactVDF> {
        // SERVE (`full_node.request_compact_vdf`): the height's main-chain block whose header
        // hash the requester named; answer only when OUR proof for that field is already compact.
        let block = self.block_by_height(req.height).await?;
        if block.header_hash().ok()? != req.header_hash {
            return None;
        }
        let proof = dg_xch_node::compact_vdf::serve_compact(&block, req.field_vdf, &req.vdf_info)?;
        Some(RespondCompactVDF {
            height: req.height,
            header_hash: req.header_hash,
            field_vdf: req.field_vdf,
            vdf_info: req.vdf_info,
            vdf_proof: proof,
        })
    }

    pub(super) async fn on_new_compact_vdf(
        &self,
        _peer: Bytes32,
        ann: NewCompactVDF,
    ) -> Option<RequestCompactVDF> {
        // `new_compact_vdf`: ignore while syncing (tip-context), ignore blocks within 5 of our
        // peak ("will not compactify recent block"), and pull only when we still hold that exact
        // field/VdfInfo bulky (needs_compact_proof). Otherwise stay silent.
        if !self.synced.load(Ordering::Relaxed) {
            return None;
        }
        let (_, peak_height) = self.store.get_peak().await.ok().flatten()?;
        if peak_height.saturating_sub(ann.height) < 5 {
            return None;
        }
        let block = self.block_by_height(ann.height).await?;
        if block.header_hash().ok()? != ann.header_hash {
            return None;
        }
        if !dg_xch_node::compact_vdf::needs_compact_proof(&block, ann.field_vdf, &ann.vdf_info) {
            return None;
        }
        Some(RequestCompactVDF {
            height: ann.height,
            header_hash: ann.header_hash,
            field_vdf: ann.field_vdf,
            vdf_info: ann.vdf_info,
        })
    }

    pub(super) async fn on_respond_compact_vdf(&self, _peer: Bytes32, resp: RespondCompactVDF) {
        // Queue for the driver: validation (a VDF verify) and the block re-write never run on the
        // websocket read loop. Bounded like every other received-gossip inbox.
        let mut inbox = self.compact_vdf_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(resp);
        }
    }

    pub(super) async fn on_respond_compact_proof_of_time(
        &self,
        _peer: Bytes32,
        resp: RespondCompactProofOfTime,
    ) {
        // A bluebox timelord's answer to our solicitation. It carries the same five fields as a
        // RespondCompactVDF, so map it and feed the identical consume inbox — the driver's
        // process_compact_vdf_inbox validates + swaps under the same-header-hash guard + re-gossips
        // NewCompactVDF. No new validate/replace surface.
        let mapped = RespondCompactVDF {
            height: resp.height,
            header_hash: resp.header_hash,
            field_vdf: resp.field_vdf,
            vdf_info: resp.vdf_info,
            vdf_proof: resp.vdf_proof,
        };
        let mut inbox = self.compact_vdf_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(mapped);
        }
    }
}
