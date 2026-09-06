use super::*;

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    pub(super) async fn on_declare_proof_of_space(
        &self,
        peer: Bytes32,
        declare: DeclareProofOfSpace,
    ) -> Option<RequestSignedValues> {
        // S1 — a declare arrived AT ALL (distinguishes never-received from received-then-dropped).
        self.producer.declare_received();
        info!(
            "declare_proof_of_space received event={} peer={} sp_index={} challenge={} cc_sp={}",
            "producer.declare.received",
            peer,
            declare.signage_point_index,
            declare.challenge_hash,
            declare.challenge_chain_sp
        );
        // `full_node_api.declare_proof_of_space` — declare validation is tip-context; a
        // syncing node has no consistent slot state to check against, so it drops the message.
        if !self.synced.load(Ordering::Relaxed) {
            // Promoted trace!->info! for bring-up: at default level the operator must
            // see this wall.
            self.producer.validated("not_synced");
            info!(
                "declare dropped: node not synced (tip-context validation impossible) event={} peer={}",
                "producer.declare.not_synced", peer
            );
            return None;
        }
        // The plot filter is height-dependent (hard-fork sizing). The farmer proved against the
        // network tip it sees; the highest peer-announced tip is our closest match to that view.
        let height = self.claimed_peak.load(Ordering::Relaxed);
        // The two slot-state lookups run synchronously under one guard — the proof verify itself is
        // CPU-only (no I/O), so holding the slot lock across it is bounded and avoids a TOCTOU on the
        // SP set. This is the read loop, but a single PoSpace verify is cheap (unlike a VDF verify,
        // which we always defer).
        let verdict = {
            let slot = self.slot_state.lock().await;
            validate_declared_proof(
                &self.constants,
                &declare,
                height,
                |cc_sp| slot.get_signage_point(cc_sp),
                |cc| slot.get_sub_slot(cc).is_some(),
            )
        };
        match verdict {
            DeclareVerdict::Accepted(quality_string) => {
                self.producer.validated("accepted");
                // An accepted proof is always held as a candidate proof, independent of
                // whether we can assemble a full block from it yet.
                self.proof_candidates.lock().await.insert(AcceptedProof {
                    declare: declare.clone(),
                    quality_string,
                });
                info!(
                    "accepted proof of space, held as candidate event={} peer={} qs={}",
                    "producer.declare.accepted", peer, quality_string
                );
                // Assemble the candidate unfinished block (placeholder foliage
                // signatures + the SP signatures from THIS declare message), store it keyed by the
                // quality string, and return the RequestSignedValues for the farmer to sign the two
                // foliage hashes. Returns None (no emit) when the candidate cannot yet be assembled
                // from the reachable slot/block state — see try_build_candidate's per-reason drops.
                let request = self.try_build_candidate(&declare, quality_string).await;
                // S4 — a candidate was built and we are about to send RequestSignedValues back.
                if request.is_some() {
                    self.producer.request_signed_values();
                }
                request
            }
            other => {
                let result = other.result_label();
                self.producer.validated(result);
                info!(
                    "declare rejected at validate_declared_proof event={} peer={} result={} sp_index={} challenge={}",
                    "producer.declare.rejected",
                    peer,
                    result,
                    declare.signage_point_index,
                    declare.challenge_hash
                );
                None
            }
        }
    }

    pub(super) async fn on_signed_values(&self, peer: Bytes32, signed: SignedValues) {
        // FARMER→NODE (`full_node_api.signed_values`): the farmer's real foliage signatures for a
        // candidate we asked it to sign. Retrieve the candidate, verify the foliage_block_data
        // signature against the plot key (a mismatch means a plot
        // collision), splice both signatures into the foliage, and propagate the finished block.
        // S5 — the farmer signed back.
        self.producer.signed_values();
        let Some((height, mut candidate)) = self
            .candidates
            .lock()
            .await
            .get(&signed.quality_string)
            .cloned()
        else {
            self.producer
                .candidate_dropped("signed_values_no_candidate");
            warn!(
                "signed_values: no candidate for this quality string (evicted or unknown) event={} reason={} qs={} peer={}",
                "producer.signed.dropped",
                "signed_values_no_candidate",
                signed.quality_string,
                peer
            );
            return;
        };
        // Verify(plot_public_key, foliage_block_data.get_hash(), fbd_signature).
        let plot_pk = candidate.reward_chain_block.proof_of_space.plot_public_key;
        let Ok(fbd_hash) = candidate.foliage.foliage_block_data.hash() else {
            self.producer.candidate_dropped("foliage_hash_fail");
            warn!(
                "signed_values: candidate foliage_block_data failed to hash event={} reason={} qs={}",
                "producer.signed.dropped", "foliage_hash_fail", signed.quality_string
            );
            return;
        };
        if !dg_xch_core::consensus::producer::verify_plot_signature(
            &plot_pk,
            fbd_hash,
            &signed.foliage_block_data_signature,
        ) {
            // Stays warn! — an invalid foliage signature is a plot collision, genuinely alarming.
            self.producer.candidate_dropped("sig_verify_fail");
            warn!(
                "signed_values: foliage_block_data signature invalid (plot collision?); dropping event={} reason={} qs={} peer={}",
                "producer.signed.dropped", "sig_verify_fail", signed.quality_string, peer
            );
            return;
        }
        // Splice the foliage_block_data signature and, for a tx block, the
        // foliage_transaction_block signature.
        splice_farmer_foliage_signatures(
            &mut candidate,
            signed.foliage_block_data_signature,
            signed.foliage_transaction_block_signature,
        );
        // The add-unfinished-block latency guard: drop the block
        // if it would be infused before the current finished head (block.total_iters < peak.total_iters).
        if let Ok(Some((peak_hash, _))) = self.store.get_peak().await
            && let Ok(Some(peak_rec)) = self.store.get_block_record(&peak_hash).await
            && candidate.reward_chain_block.total_iters < peak_rec.total_iters
        {
            self.producer.candidate_dropped("latency_drop_signed");
            warn!(
                "dropping farmed unfinished block: would infuse before the current head (latency) event={} reason={} qs={} sp_index={} block_total_iters={} head_total_iters={}",
                "producer.signed.dropped",
                "latency_drop_signed",
                signed.quality_string,
                candidate.reward_chain_block.signage_point_index,
                candidate.reward_chain_block.total_iters,
                peak_rec.total_iters
            );
            return;
        }
        // S5 success — a finished unfinished block. `partial` is the S6/S7 join key; `header` (the hash
        // of the now-signed foliage) is the FullBlock header hash, recorded so the follow driver counts
        // this block when it confirms (S8). The foliage is fixed from here — infusion never touches it.
        self.producer.ub_assembled();
        let partial = candidate.reward_chain_block.hash().ok();
        if let Ok(bytes) = candidate
            .foliage
            .to_bytes(dg_xch_serialize::ChiaProtocolVersion::default())
        {
            let header = Bytes32::from(dg_xch_core::utils::hash_256(bytes));
            let mut farmed = self.farmed_headers.lock().await;
            farmed.push_back(header);
            while farmed.len() > FARMED_HEADER_CAP {
                farmed.pop_front();
            }
        }
        info!(
            "farmed unfinished block: signatures spliced, propagating event={} height={} qs={} partial={:?}",
            "producer.signed.spliced", height, signed.quality_string, partial
        );
        let mut inbox = self.ub_inbox.lock().await;
        if inbox.len() < SP_INBOX_CAP {
            inbox.push(candidate);
        } else {
            // Was a silent drop on the floor: a spliced, ready-to-infuse block lost with no trace.
            self.producer.candidate_dropped("ub_inbox_full");
            warn!(
                "farmed unfinished block dropped: ub_inbox at cap event={} reason={} qs={}",
                "producer.signed.dropped", "ub_inbox_full", signed.quality_string
            );
        }
    }

    pub(super) async fn on_new_infusion_point_vdf(&self, peer: Bytes32, req: NewInfusionPointVDF) {
        // `full_node_api.new_infusion_point_vdf` — `if sync_store.get_sync_mode(): return None`.
        // A syncing node has no consistent slot/unfinished state to finish a block against.
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        // Queue only — the assembly (unfinished-block lookup + reward-chain backtrack + finished-sub-slot
        // walk + engine add_block/set-peak) runs on the driver, never the websocket read loop.
        let mut inbox = self.ip_inbox.lock().await;
        if inbox.len() < IP_INBOX_CAP {
            inbox.push(req);
        } else {
            warn!("dropping infusion-point VDF: ip_inbox at cap peer={}", peer);
        }
    }

    pub(super) async fn on_new_signage_point_vdf(&self, peer: Bytes32, req: NewSignagePointVDF) {
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        let sp = RespondSignagePoint {
            index_from_challenge: req.index_from_challenge,
            challenge_chain_vdf: req.challenge_chain_sp_vdf,
            challenge_chain_proof: req.challenge_chain_sp_proof,
            reward_chain_vdf: req.reward_chain_sp_vdf,
            reward_chain_proof: req.reward_chain_sp_proof,
        };
        self.on_respond_signage_point(peer, sp).await;
    }

    pub(super) async fn on_new_end_of_sub_slot_vdf(&self, peer: Bytes32, req: NewEndOfSubSlotVDF) {
        if !self.synced.load(Ordering::Relaxed) {
            return;
        }
        let Ok(cc_hash) = req.end_of_sub_slot_bundle.challenge_chain.hash() else {
            return;
        };
        if self
            .slot_state
            .lock()
            .await
            .get_sub_slot(&cc_hash)
            .is_some()
        {
            return;
        }
        self.on_respond_end_of_sub_slot(
            peer,
            RespondEndOfSubSlot {
                end_of_slot_bundle: req.end_of_sub_slot_bundle,
            },
        )
        .await;
    }
}
