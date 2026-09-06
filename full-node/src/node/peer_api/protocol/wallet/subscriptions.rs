use super::*;

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    // `register_for_ph_updates`: subscribe the peer to puzzle-hash coin
    // updates in the shared WalletNotifier AND return the initial matching CoinState set. The receiver is
    // handed back only on the peer's FIRST registration; the dispatch layer bridges it to the socket.
    pub(in crate::node::peer_api::protocol) async fn register_for_ph_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RegisterForPhUpdates,
    ) -> PhRegistration {
        // Subscribe FIRST — the returned set is the hashes ACTUALLY subscribed (in-request duplicates,
        // already-subscribed hashes, and the per-peer-cap overflow all filtered out) — and feed ONLY
        // that set to the initial-state read (the query runs on
        // add_puzzle_subscriptions' return, never the raw request list). A registry-capacity failure
        // subscribes nothing, so it also reads nothing.
        let (receiver, subscribed) = self
            .wallet
            .register_for_ph_updates(peer, host, &req.puzzle_hashes)
            .await
            .unwrap_or_else(|_| (None, Vec::new()));
        // The response budget resolves from trust (`max_subscribe_response_items(peer)`).
        let max_items = self.trust.max_subscribe_response_items(&peer, host);
        let coin_states = self
            .ph_initial_states(&subscribed, req.min_height, max_items)
            .await;
        PhRegistration {
            response: RespondToPhUpdates {
                // The reply echoes the REQUESTED hashes, not the subscribed subset — and
                // signals nothing on truncation (log-only).
                puzzle_hashes: req.puzzle_hashes,
                min_height: req.min_height,
                coin_states,
            },
            receiver,
        }
    }

    // `register_for_coin_updates`: subscribe the peer to coin-id updates AND
    // return the initial matching CoinState set. The initial read uses get_coin_states_by_ids — a
    // provided default over point-gets, so it works on every backend without the service tier.
    pub(in crate::node::peer_api::protocol) async fn register_for_coin_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RegisterForCoinUpdates,
    ) -> CoinRegistration {
        // The REQUEST list truncates to max_subscriptions; the SLICED list is subscribed + queried,
        // and echoes the sliced list back. Unlike the ph path, in-request
        // duplicates stay queryable — the query runs on the deduped set, so dedup
        // happens at the query, not the echo.
        let mut coin_ids = req.coin_ids;
        coin_ids.truncate(self.wallet.max_subscriptions(&peer, host));
        let receiver = self
            .wallet
            .register_for_coin_updates(peer, host, &coin_ids)
            .await
            .ok()
            .and_then(|(rx, _added)| rx);
        let mut seen = HashSet::new();
        let query_ids: Vec<Bytes32> = coin_ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect();
        // Bounded by the response budget (get_coin_states_by_ids with
        // max_items = max_subscribe_response_items), resolved per-peer from trust.
        let coin_states = self
            .store
            .get_coin_states_by_ids(
                &query_ids,
                req.min_height,
                true,
                self.trust.max_subscribe_response_items(&peer, host),
            )
            .await
            .unwrap_or_default();
        CoinRegistration {
            response: RespondToCoinUpdates {
                coin_ids,
                min_height: req.min_height,
                coin_states,
            },
            receiver,
        }
    }

    // `request_puzzle_state` (code 98) — the Sage sync loop's paged
    // puzzle-hash read. Truncate + dedup the request list, check the requester's previous peak
    // against our chain (REORG reject on mismatch), check the subscription cap before AND after
    // the store read (the await-race double check), run the paged batch query, resolve the
    // page's (height, header_hash), and subscribe-on-finish. coin-index tier only (the batch
    // query reads the puzzle-hash secondary index); the validator tier keeps the store-blind
    // REORG-reject default.
    #[cfg(feature = "coin-index")]
    pub(in crate::node::peer_api::protocol) async fn puzzle_state(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RequestPuzzleState,
    ) -> PuzzleStateReply {
        // — the list_limits truncation + order-preserving dedup.
        let mut puzzle_hashes = req.puzzle_hashes;
        puzzle_hashes.truncate(dg_xch_stores::traits::MAX_PUZZLE_HASH_BATCH_SIZE);
        let mut seen = HashSet::new();
        puzzle_hashes.retain(|ph| seen.insert(*ph));
        // — previous_height=None compares against the GENESIS_CHALLENGE; an
        // unknown height or a mismatched hash means the requester's chain forked from ours.
        let previous_hash = match req.previous_height {
            Some(h) => self.height_to_hash(h).await,
            None => Some(self.constants.genesis_challenge),
        };
        if previous_hash != Some(req.header_hash) {
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        }
        // — would this subscribe blow the per-peer cap? (trust-resolved)
        let max_subscriptions = self.wallet.max_subscriptions(&peer, host);
        if req.subscribe_when_finished
            && puzzle_hashes.len() + self.wallet.peer_subscription_count(&peer).await
                > max_subscriptions
        {
            return PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        // — the page floor and the paged store read (batch_coin_states_by_
        // puzzle_hashes), bounded by the trust-resolved response budget.
        let max_items = self.trust.max_subscribe_response_items(&peer, host);
        let min_height = req.previous_height.map_or(0, |h| h.saturating_add(1));
        let Ok((coin_states, next_min_height)) = self
            .store
            .batch_coin_states_by_puzzle_hashes(&puzzle_hashes, min_height, &req.filters, max_items)
            .await
        else {
            // A store failure cannot produce a consistent page — the REORG reject is the
            // always-answer posture (an exception would leave the wallet timing out).
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        };
        let is_finished = next_min_height.is_none();
        // — the page's (height, header_hash): the block BEFORE the next page's
        // floor, or the peak when finished; no peak / unresolvable height rejects REORG.
        let Ok(Some((_, peak_height))) = self.store.get_peak().await else {
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        };
        let height = next_min_height.map_or(peak_height, |h| h.saturating_sub(1));
        let Some(header_hash) = self.height_to_hash(height).await else {
            return PuzzleStateReply::Reject(RejectStateReason::REORG);
        };
        // — re-check the cap across the await point.
        if req.subscribe_when_finished
            && puzzle_hashes.len() + self.wallet.peer_subscription_count(&peer).await
                > max_subscriptions
        {
            return PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        let mut receiver = None;
        if is_finished && req.subscribe_when_finished {
            match self
                .wallet
                .register_for_ph_updates(peer, host, &puzzle_hashes)
                .await
            {
                Ok((rx, _added)) => receiver = rx,
                // The registry itself is at capacity (a structural bound): the honest
                // answer is the subscription-limit reject, not a silently unsubscribed respond.
                Err(_) => {
                    return PuzzleStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
                }
            }
        }
        PuzzleStateReply::Respond(
            Box::new(RespondPuzzleState {
                puzzle_hashes,
                height,
                header_hash,
                is_finished,
                coin_states,
            }),
            receiver,
        )
    }

    pub(in crate::node::peer_api::protocol) async fn coin_state(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RequestCoinState,
    ) -> CoinStateReply {
        // — truncate to max_subscribe_response_items (trust-resolved), then dedup.
        let max_items = self.trust.max_subscribe_response_items(&peer, host);
        let mut coin_ids = req.coin_ids;
        coin_ids.truncate(max_items);
        let mut seen = HashSet::new();
        coin_ids.retain(|id| seen.insert(*id));
        // — the previous-peak consistency check.
        let previous_hash = match req.previous_height {
            Some(h) => self.height_to_hash(h).await,
            None => Some(self.constants.genesis_challenge),
        };
        if previous_hash != Some(req.header_hash) {
            return CoinStateReply::Reject(RejectStateReason::REORG);
        }
        // — the pre-read cap check (trust-resolved).
        let max_subscriptions = self.wallet.max_subscriptions(&peer, host);
        if req.subscribe
            && coin_ids.len() + self.wallet.peer_subscription_count(&peer).await > max_subscriptions
        {
            return CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        // — include_spent=True, min_height = previous_height + 1.
        let min_height = req.previous_height.map_or(0, |h| h.saturating_add(1));
        let Ok(coin_states) = self
            .store
            .get_coin_states_by_ids(&coin_ids, min_height, true, max_items)
            .await
        else {
            return CoinStateReply::Reject(RejectStateReason::REORG);
        };
        // — the await-race re-check.
        if req.subscribe
            && coin_ids.len() + self.wallet.peer_subscription_count(&peer).await > max_subscriptions
        {
            return CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
        }
        let mut receiver = None;
        if req.subscribe {
            match self
                .wallet
                .register_for_coin_updates(peer, host, &coin_ids)
                .await
            {
                Ok((rx, _added)) => receiver = rx,
                Err(_) => {
                    return CoinStateReply::Reject(RejectStateReason::ExceededSubscriptionLimit);
                }
            }
        }
        CoinStateReply::Respond(
            Box::new(RespondCoinState {
                coin_ids,
                coin_states,
            }),
            receiver,
        )
    }

    // `request_fee_estimates`: for each requested epoch time,
    // estimate the fee-rate to be confirmed within `max(0, target - now)` seconds, reading the
    // mempool's fee estimator. V2→V1 rounds with `ceil` and always
    // answers one FeeEstimate per requested time (error=None; rate 0 when there is no history).
    pub(in crate::node::peer_api::protocol) async fn fee_estimates(
        &self,
        req: RequestFeeEstimates,
    ) -> FeeEstimateGroup {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let estimates = {
            let mp = self.mempool.lock().await;
            let est = mp.fee_estimator();
            req.time_targets
                .iter()
                .map(|&target| {
                    // deltas = [max(0, req_ts - utc_now)]
                    let delta = target.saturating_sub(now);
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let mojos = est.estimate_fee_rate(delta).ceil() as u64;
                    FeeEstimate {
                        error: None,
                        time_target: target,
                        estimated_fee_rate: FeeRate {
                            mojos_per_clvm_cost: mojos,
                        },
                    }
                })
                .collect()
        };
        FeeEstimateGroup {
            error: None,
            estimates,
        }
    }

    // `request_remove_puzzle_subscriptions`: None = clear all
    // (returning the prior set), Some = remove the listed subset (returning what was removed).
    pub(in crate::node::peer_api::protocol) async fn remove_puzzle_subscriptions(
        &self,
        peer: Bytes32,
        puzzle_hashes: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        self.wallet
            .remove_ph_subscriptions(&peer, puzzle_hashes.as_deref())
            .await
    }

    // `request_remove_coin_subscriptions`.
    pub(in crate::node::peer_api::protocol) async fn remove_coin_subscriptions(
        &self,
        peer: Bytes32,
        coin_ids: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        self.wallet
            .remove_coin_subscriptions(&peer, coin_ids.as_deref())
            .await
    }
}
