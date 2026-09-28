use super::*;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    /// A validated weight proof for the current claimed tip: the cached one when present (validate AT
    /// MOST ONCE per landing), otherwise fetch from live peers and run full verification.
    ///
    /// # Errors
    /// Returns an I/O error if every peer fails the fetch or the proof fails validation.
    pub(in crate::node) async fn validated_proof(
        &self,
        peers: &[Arc<OutboundPeer>],
    ) -> Result<Option<ValidatedTip>, Error> {
        let Some(target) = self.sync_target().await else {
            return Ok(None);
        };
        let (tip, tip_height, tip_weight) = (target.header_hash, target.height, target.weight);
        if peers.is_empty() {
            return Ok(None);
        }
        if let Some(v) = self.validated_tip.read().await.clone() {
            info!("reusing validated weight proof cached_tip={}", v.tip);
            return Ok(Some(v));
        }
        // Race the weight-proof request across EVERY live peer and take the first that answers. A single
        // peer that is slow, swamped, or unwilling to serve a multi-MB proof must not stall the sync (one
        // laggy peer would otherwise eat the whole timeout every tick). The first valid proof wins; the
        // rest are aborted. The serving peer travels with the proof so a proof that fails the claim
        // cross-check or validation can evict exactly that peer (the failed-proof eviction).
        info!(
            "fast-sync: fetching weight proof (racing all peers) tip_height={} peers={}",
            tip_height,
            peers.len()
        );
        let mut fetches = tokio::task::JoinSet::new();
        for peer in peers {
            let peer = peer.clone();
            fetches.spawn(async move {
                let fetched =
                    request_weight_proof(&peer, tip, tip_height, WEIGHT_PROOF_TIMEOUT).await;
                (peer, fetched)
            });
        }
        let mut fetched = None;
        let mut failures = 0usize;
        while let Some(joined) = fetches.join_next().await {
            match joined {
                Ok((peer, Ok(proof))) => {
                    fetched = Some((peer, proof));
                    break;
                }
                Ok((_, Err(e))) => {
                    failures += 1;
                    warn!(
                        "weight-proof fetch from a peer failed, awaiting others error={}",
                        e
                    );
                }
                Err(e) => {
                    failures += 1;
                    warn!("weight-proof fetch task join error error={}", e);
                }
            }
        }
        fetches.abort_all();
        let Some((wp_peer, proof)) = fetched else {
            // NO peer will serve a proof for this claimed tip: retract the claim (every claimant) so a
            // phantom peak cannot be re-selected tick after tick. Honest claimants re-announce within a
            // block cadence and repopulate the book — the soft analog of closing the peer that
            // failed to serve the proof (request_validate_wp → peer.close).
            self.peak_book.retract_hash(&tip);
            return Err(Error::other(format!(
                "weight-proof fetch failed from all {failures} peers; retracted claims on tip {tip}"
            )));
        };
        // `request_validate_wp`: the proof must attest EXACTLY the claimed tip — its recent chain's
        // last block carries the claimed height AND weight. A mismatch quarantines the claimed
        // peak (never re-selected) and evicts the
        // serving peer.
        let attested = proof
            .recent_chain_data
            .last()
            .map(|h| (h.height(), h.weight()));
        if attested != Some((tip_height, tip_weight)) {
            self.peak_book.quarantine(tip, tip_height);
            wp_peer.stop();
            return Err(Error::other(format!(
                "weight proof attests {attested:?}, claim was ({tip_height}, {tip_weight}); peak {tip} quarantined"
            )));
        }
        // Refuse a proof whose recent chain rides through ANY quarantined peak
        // (an extension of a poisoned chain re-offered under a fresh tip hash).
        for header in &proof.recent_chain_data {
            if let Ok(hash) = header.header_hash()
                && self.peak_book.is_quarantined(&hash)
            {
                return Err(Error::other(format!(
                    "weight proof rides through quarantined peak {hash}"
                )));
            }
        }
        // Debug: dump the raw proof bytes so a real mainnet weight proof can be captured as an offline
        // validation fixture (validating a fetched-fresh proof live is minutes; a fixture makes it
        // deterministic and profilable). Off unless --dump-weight-proof-dir is set.
        if let Some(dir) = &self.config.capture_dir {
            match proof.to_bytes(ChiaProtocolVersion::default()) {
                Ok(bytes) => {
                    let path = dir.join(format!("weight_proof_{tip_height}.bin"));
                    match std::fs::write(&path, &bytes) {
                        Ok(()) => {
                            info!(
                                "dumped weight-proof fixture path={} bytes={}",
                                path.display(),
                                bytes.len()
                            )
                        }
                        Err(e) => warn!("failed to write weight-proof dump error={}", e),
                    }
                }
                Err(e) => warn!("failed to serialize weight proof for dump error={:?}", e),
            }
        }
        let wp = Arc::new(proof);
        // Verify off the async runtime (blocking pool) AND off the chaser lock: the verify is CPU-bound
        // for minutes and must not stall the tip-follow driver or hold the chaser mutex meanwhile.
        let constants = self.constants;
        let wp_for_verify = wp.clone();
        info!(
            "fast-sync: validating weight proof (spawn_blocking) tip_height={}",
            tip_height
        );
        let verified = tokio::task::spawn_blocking(move || {
            dg_xch_weight_proof::validate_weight_proof(&wp_for_verify, &constants)
        })
        .await
        .map_err(|e| Error::other(format!("weight-proof verify task: {e}")))?;
        let summaries = match verified {
            Ok((true, summaries)) => summaries,
            // The proof does NOT prove the claimed peak: quarantine it (
            // a poisoned peak is never re-selected) and evict the peer that served the bad proof
            // (a peer eviction would be the harder posture).
            Ok((false, _)) => {
                self.peak_book.quarantine(tip, tip_height);
                wp_peer.stop();
                return Err(Error::other(format!(
                    "weight proof did not validate; peak {tip} quarantined"
                )));
            }
            Err(e) => {
                self.peak_book.quarantine(tip, tip_height);
                wp_peer.stop();
                return Err(Error::other(format!(
                    "weight proof: {e:?}; peak {tip} quarantined"
                )));
            }
        };
        let v = ValidatedTip {
            tip,
            wp,
            summaries: Arc::new(summaries),
        };
        *self.validated_tip.write().await = Some(v.clone());
        Ok(Some(v))
    }
}
