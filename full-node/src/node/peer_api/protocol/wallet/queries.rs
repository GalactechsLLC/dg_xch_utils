use super::*;

impl<S: BlockStore + CoinStore + Send + Sync + 'static> StoreApi<S> {
    // ---- light-wallet query surface (wallet handlers) ---------------------------

    // `request_puzzle_solution`: the (puzzle, solution) of a coin spent at
    // `height`, recovered by re-running that block's generator. Reuses the ONE tested extraction path
    // shared with the HTTP RPC (never a second VM); any miss maps to a RejectPuzzleSolution on the wire.
    pub(in crate::node::peer_api::protocol) async fn puzzle_solution(
        &self,
        coin_name: Bytes32,
        height: u32,
    ) -> Option<PuzzleSolutionResponse> {
        let spend = crate::rpc::puzzle_and_solution_coin_spend(
            self.store.as_ref(),
            &self.constants,
            &coin_name,
            height,
        )
        .await
        .ok()?;
        Some(PuzzleSolutionResponse {
            coin_name,
            height,
            puzzle: spend.puzzle_reveal,
            solution: spend.solution,
        })
    }

    pub(in crate::node::peer_api::protocol) async fn send_transaction(
        &self,
        _peer: Bytes32,
        tx: SendTransaction,
    ) -> TransactionAck {
        let txid = tx.transaction.name().unwrap_or_default();
        if !self.synced.load(Ordering::Relaxed) {
            return TransactionAck {
                txid,
                status: TXStatus::FAILED,
                error: Some("NO_TRANSACTIONS_WHILE_SYNCING".to_string()),
            };
        }
        match crate::tx_admission::admit_spend_bundle(
            self.store.as_ref(),
            &self.mempool,
            &self.constants,
            &self.tx_announce,
            tx.transaction,
        )
        .await
        {
            // SUCCESS acks with error=None.
            Ok(name) => TransactionAck {
                txid: name,
                status: TXStatus::SUCCESS,
                error: None,
            },
            Err(e) => {
                let (status, err_name) = e.ack();
                debug!("send_transaction rejected txid={} error={}", txid, e);
                TransactionAck {
                    txid,
                    status,
                    error: Some(err_name.to_string()),
                }
            }
        }
    }

    // `request_block_header`: the HeaderBlock at a height. An unknown height
    // rejects; a record with no stored body stays silent. The header carries the REAL BIP158
    // transactions_filter (G3 closed — see served_header_block).
    pub(in crate::node::peer_api::protocol) async fn block_header(
        &self,
        height: u32,
    ) -> BlockHeaderReply {
        let record = match self.store.get_block_record_by_height(height).await {
            Ok(Some(r)) => r,
            _ => return BlockHeaderReply::Reject(height),
        };
        match self.store.get_block(&record.header_hash).await {
            Ok(Some(block)) => match self.served_header_block(&block, true).await {
                Some(hb) => BlockHeaderReply::Respond(Box::new(hb)),
                None => BlockHeaderReply::Silent,
            },
            _ => BlockHeaderReply::Silent,
        }
    }

    // `request_header_blocks` (DEPRECATED, code 60): header blocks in
    // [start, end]. A bad/oversized range is silent; an unknown height in range rejects.
    // Headers carry the real filter, built from the coin store per block.
    pub(in crate::node::peer_api::protocol) async fn header_blocks(
        &self,
        start_height: u32,
        end_height: u32,
    ) -> HeaderBlocksReply {
        if end_height < start_height
            || end_height - start_height > self.constants.max_block_count_per_requests
        {
            return HeaderBlocksReply::Silent;
        }
        let reject = || {
            HeaderBlocksReply::Reject(RejectHeaderBlocks {
                start_height,
                end_height,
            })
        };
        let mut header_blocks = Vec::new();
        for h in start_height..=end_height {
            let Ok(Some(record)) = self.store.get_block_record_by_height(h).await else {
                return reject();
            };
            let Ok(Some(block)) = self.store.get_block(&record.header_hash).await else {
                return reject();
            };
            let Some(hb) = self.served_header_block(&block, true).await else {
                return reject();
            };
            header_blocks.push(hb);
        }
        HeaderBlocksReply::Respond(Box::new(RespondHeaderBlocks {
            start_height,
            end_height,
            header_blocks,
        }))
    }

    // `request_block_headers` (the streamed shape, code 86): header blocks in
    // [start, end], capped at 128. A bad range or a missing block rejects. return_filter is honored:
    // false serves the encoded-empty filter b"\x00", true serves the real per-block filter from
    // the coin store.
    pub(in crate::node::peer_api::protocol) async fn block_headers(
        &self,
        start_height: u32,
        end_height: u32,
        return_filter: bool,
    ) -> BlockHeadersReply {
        let reject = || {
            BlockHeadersReply::Reject(RejectBlockHeaders {
                start_height,
                end_height,
            })
        };
        if end_height < start_height || end_height - start_height > 128 {
            return reject();
        }
        let mut header_blocks = Vec::new();
        for h in start_height..=end_height {
            let Ok(Some(record)) = self.store.get_block_record_by_height(h).await else {
                return reject();
            };
            let Ok(Some(block)) = self.store.get_block(&record.header_hash).await else {
                return reject();
            };
            let Some(hb) = self.served_header_block(&block, return_filter).await else {
                return reject();
            };
            header_blocks.push(hb);
        }
        BlockHeadersReply::Respond(Box::new(RespondBlockHeaders {
            start_height,
            end_height,
            header_blocks,
        }))
    }

    // `request_additions`: coins created at a block, grouped by puzzle hash.
    // puzzle_hashes = None → all additions, proofs = None (the trusted-wallet path);
    // puzzle_hashes = Some → per-hash coins plus MerkleSet INCLUSION/EXCLUSION proofs against the
    // foliage additions_root (leaf pairs [puzzle_hash, hash_coin_ids(coin names)]), which an
    // untrusted wallet verifies against the block header.
    // coin-index tier only.
    #[cfg(feature = "coin-index")]
    pub(in crate::node::peer_api::protocol) async fn additions(
        &self,
        req: RequestAdditions,
    ) -> AdditionsReply {
        let reject = |header_hash: Bytes32| {
            AdditionsReply::Reject(RejectAdditionsRequest {
                height: req.height,
                header_hash,
            })
        };
        if req
            .puzzle_hashes
            .as_ref()
            .is_some_and(|p| p.len() > MAX_COIN_HASHES_PER_REQUEST)
        {
            return reject(req.header_hash.unwrap_or_default());
        }
        // Resolve + fork-check the header hash (height_to_hash(height) == header_hash).
        let Ok(Some(confirmed)) = self.store.get_block_record_by_height(req.height).await else {
            return reject(req.header_hash.unwrap_or_default());
        };
        let header_hash = req.header_hash.unwrap_or(confirmed.header_hash);
        if header_hash != confirmed.header_hash {
            return reject(header_hash);
        }
        // Empty proof request: no DB + Merkle work — answer coins=[] with proofs=[]
        // (Some-empty, NOT None).
        if req.puzzle_hashes.as_ref().is_some_and(Vec::is_empty) {
            return AdditionsReply::Respond(Box::new(RespondAdditions {
                height: req.height,
                header_hash,
                coins: Vec::new(),
                proofs: Some(Vec::new()),
            }));
        }
        // The block-delta scan is guarded by wallet_sync_sem (active=2, waiting=20) and REJECTS
        // on overflow — concurrent heavy wallet serves are bounded, never queued without limit.
        let Ok(_permit) = self.wallet_sync_sem.acquire().await else {
            return reject(header_hash);
        };
        let Ok(added) = self.store.get_coins_added_at_height(req.height).await else {
            return reject(header_hash);
        };
        // Reorg guard: the DB read may straddle a reorg — re-check height→hash.
        match self.store.get_block_record_by_height(req.height).await {
            Ok(Some(r)) if r.header_hash == header_hash => {}
            _ => return reject(header_hash),
        }
        // puzzle hash → coins, in additions insertion order (the trusted-path response + the
        // proof leaf pairs both iterate it).
        let mut order: Vec<Bytes32> = Vec::new();
        let mut map: HashMap<Bytes32, Vec<Coin>> = HashMap::new();
        for cr in added {
            let entry = map.entry(cr.coin.puzzle_hash).or_default();
            if entry.is_empty() {
                order.push(cr.coin.puzzle_hash);
            }
            entry.push(cr.coin);
        }
        match req.puzzle_hashes {
            None => {
                // Only the serve-everything map is bounded.
                if map.len() > MAX_COINS_MAP_SIZE {
                    return reject(header_hash);
                }
                let coins: Additions = order
                    .iter()
                    .map(|ph| (*ph, map.remove(ph).unwrap_or_default()))
                    .collect();
                AdditionsReply::Respond(Box::new(RespondAdditions {
                    height: req.height,
                    header_hash,
                    coins,
                    proofs: None,
                }))
            }
            Some(ref puzzle_hashes) => {
                // The addition merkle set: [puzzle_hash, hash_coin_ids(coin names)] leaf pairs —
                // its root IS the foliage additions_root.
                let mut leafs: Vec<[u8; 32]> = Vec::with_capacity(2 * order.len());
                for ph in &order {
                    leafs.push(ph.bytes());
                    let names: Vec<[u8; 32]> = map[ph].iter().map(|c| c.name().bytes()).collect();
                    leafs.push(hash_coin_ids(&names));
                }
                let addition_merkle_set = MerkleSet::from_leafs(&mut leafs);
                let mut coins_map: Additions = Vec::with_capacity(puzzle_hashes.len());
                let mut proofs_map: Vec<(Bytes32, Vec<u8>, Option<Vec<u8>>)> =
                    Vec::with_capacity(puzzle_hashes.len());
                for ph in puzzle_hashes {
                    // INCLUSION if the hash is in the set, EXCLUSION otherwise (
                    // its asserts hold structurally here — the set is built from the same map —
                    // so a mismatch is a corrupt read: reject, never a panic).
                    let Ok((included, proof)) = addition_merkle_set.generate_proof(&ph.bytes())
                    else {
                        return reject(header_hash);
                    };
                    if let Some(coins) = map.get(ph) {
                        let names: Vec<[u8; 32]> = coins.iter().map(|c| c.name().bytes()).collect();
                        let coin_ids_hash = hash_coin_ids(&names);
                        let Ok((included_2, proof_2)) =
                            addition_merkle_set.generate_proof(&coin_ids_hash)
                        else {
                            return reject(header_hash);
                        };
                        if !included || !included_2 {
                            return reject(header_hash);
                        }
                        coins_map.push((*ph, coins.clone()));
                        proofs_map.push((*ph, proof, Some(proof_2)));
                    } else {
                        if included {
                            return reject(header_hash);
                        }
                        coins_map.push((*ph, Vec::new()));
                        proofs_map.push((*ph, proof, None));
                    }
                }
                AdditionsReply::Respond(Box::new(RespondAdditions {
                    height: req.height,
                    header_hash,
                    coins: coins_map,
                    proofs: Some(proofs_map),
                }))
            }
        }
    }

    // `request_removals`: coins spent at a block. coin_names = None (or
    // Some-empty, ) → all removals, proofs = None; coin_names = Some → per-name coins plus
    // MerkleSet INCLUSION/EXCLUSION proofs over the removal names, whose root is asserted equal to
    // the foliage removals_root before serving. coin-index tier only.
    #[cfg(feature = "coin-index")]
    pub(in crate::node::peer_api::protocol) async fn removals(
        &self,
        req: RequestRemovals,
    ) -> RemovalsReply {
        let reject = || {
            RemovalsReply::Reject(RejectRemovalsRequest {
                height: req.height,
                header_hash: req.header_hash,
            })
        };
        if req
            .coin_names
            .as_ref()
            .is_some_and(|n| n.len() > MAX_COIN_HASHES_PER_REQUEST)
        {
            return reject();
        }
        // The whole block-fetch + removal scan is guarded by wallet_sync_sem and REJECTS on
        // overflow.
        let Ok(_permit) = self.wallet_sync_sem.acquire().await else {
            return reject();
        };
        let Ok(Some(block)) = self.store.get_block(&req.header_hash).await else {
            return reject();
        };
        let peak_height = self.store.get_peak().await.ok().flatten().map(|(_, h)| h);
        let confirmed = self
            .store
            .get_block_record_by_height(req.height)
            .await
            .ok()
            .flatten()
            .map(|r| r.header_hash);
        // The four reject conditions: not a tx block, height mismatch, above peak, or a fork.
        if !block.is_transaction_block()
            || block.height() != req.height
            || peak_height.is_some_and(|ph| block.height() > ph)
            || confirmed != Some(req.header_hash)
        {
            return reject();
        }
        // No generator = reward-only tx block: empty removals (proofs None for a None request,
        // Some-empty when coin names were asked).
        if block.transactions_generator.is_none() {
            let proofs = if req.coin_names.is_none() {
                None
            } else {
                Some(Vec::new())
            };
            return RemovalsReply::Respond(Box::new(RespondRemovals {
                height: block.height(),
                header_hash: req.header_hash,
                coins: Vec::new(),
                proofs,
            }));
        }
        let Ok(removed) = self.store.get_coins_removed_at_height(block.height()).await else {
            return reject();
        };
        // Reorg guard: the DB read may straddle a reorg — re-check height→hash.
        match self.store.get_block_record_by_height(block.height()).await {
            Ok(Some(r)) if r.header_hash == req.header_hash => {}
            _ => return reject(),
        }
        match req.coin_names.as_deref() {
            // Trusted path — Some-empty behaves exactly like None.
            None | Some([]) => {
                let coins: Vec<NamedCoin> = removed
                    .into_iter()
                    .map(|cr| (cr.coin.name(), Some(cr.coin)))
                    .collect();
                RemovalsReply::Respond(Box::new(RespondRemovals {
                    height: block.height(),
                    header_hash: req.header_hash,
                    coins,
                    proofs: None,
                }))
            }
            Some(coin_names) => {
                // name → coin in removals order.
                let mut order: Vec<Bytes32> = Vec::with_capacity(removed.len());
                let mut by_name: HashMap<Bytes32, Coin> = HashMap::with_capacity(removed.len());
                for cr in removed {
                    let name = cr.coin.name();
                    if by_name.insert(name, cr.coin).is_none() {
                        order.push(name);
                    }
                }
                // The removal merkle set is the removal names; its root must BE the foliage
                // removals_root — a mismatch means the served delta
                // would not verify against the header: reject, never serve unprovable data.
                let mut leafs: Vec<[u8; 32]> = order.iter().map(|n| n.bytes()).collect();
                let removal_merkle_set = MerkleSet::from_leafs(&mut leafs);
                let removals_root = block
                    .foliage_transaction_block
                    .as_ref()
                    .map(|ftb| ftb.removals_root);
                if removals_root != Some(Bytes32::new(removal_merkle_set.get_root())) {
                    warn!(
                        "request_removals: stored removals do not hash to the foliage removals_root height={}",
                        req.height
                    );
                    return reject();
                }
                let mut coins_map: Vec<NamedCoin> = Vec::with_capacity(coin_names.len());
                let mut proofs_map: Vec<(Bytes32, Vec<u8>)> = Vec::with_capacity(coin_names.len());
                for coin_name in coin_names {
                    let Ok((included, proof)) =
                        removal_merkle_set.generate_proof(&coin_name.bytes())
                    else {
                        return reject();
                    };
                    proofs_map.push((*coin_name, proof));
                    if let Some(coin) = by_name.get(coin_name) {
                        if !included {
                            return reject();
                        }
                        coins_map.push((*coin_name, Some(*coin)));
                    } else {
                        if included {
                            return reject();
                        }
                        coins_map.push((*coin_name, None));
                    }
                }
                RemovalsReply::Respond(Box::new(RespondRemovals {
                    height: block.height(),
                    header_hash: req.header_hash,
                    coins: coins_map,
                    proofs: Some(proofs_map),
                }))
            }
        }
    }

    // `request_children`: coin states of every child (spent + unspent) of a
    // coin, read from the parent secondary index. coin-index tier only.
    #[cfg(feature = "coin-index")]
    pub(in crate::node::peer_api::protocol) async fn children(
        &self,
        coin_name: Bytes32,
    ) -> Vec<CoinState> {
        match self.store.get_coins_by_parent(&coin_name).await {
            Ok(records) => records.iter().map(coin_state_of).collect(),
            Err(_) => Vec::new(),
        }
    }
}
