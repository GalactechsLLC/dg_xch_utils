use super::*;

// The gossip-transaction validator worker: drains the
// bounded inbox OFF the websocket read loop, runs the bundle→conditions CLVM + aggregate-
// signature checks at next-block height, admits, and queues the re-gossip announcement.
#[allow(clippy::too_many_arguments)] // the worker's seams are the node's shared Arcs, one each
pub(in crate::node) async fn tx_validator<S: BlockStore + CoinStore + Send + Sync + 'static>(
    store: Arc<S>,
    mempool: Arc<Mutex<Mempool>>,
    constants: ConsensusConstants,
    tx_inbox: Arc<Mutex<TxQueue>>,
    tx_announce: Arc<Mutex<Vec<NewTransaction>>>,
    synced: Arc<AtomicBool>,
    run: Arc<AtomicBool>,
) {
    while run.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        // NO_TRANSACTIONS_WHILE_SYNCING —
        // bundles that raced the synced-flag transition into the inbox are dropped without
        // running CLVM. The handler-side gates keep the inbox empty in steady not-synced
        // state; this covers the transition window.
        if !synced.load(Ordering::Relaxed) {
            tx_inbox.lock().await.clear();
            continue;
        }
        // Drain both lanes, high-priority (trusted) first.
        let batch: Vec<(Bytes32, SpendBundle)> = tx_inbox.lock().await.drain_batch();
        if batch.is_empty() {
            continue;
        }
        for (_peer, tx) in batch {
            // The origin was recorded at receipt (`on_respond_transaction` → `note_tx_origin`)
            // with the remote host, so the announce drain can exclude an outbound origin too;
            // the worker only validates + admits here.
            // The shared admission seam (tx_admission.rs, `full_node.add_transaction`):
            // CLVM + aggregate-signature validation at next-block height, `Mempool::admit`,
            // and the NewTransaction announce queued iff newly resident — identical to the
            // push_tx and p2p SendTransaction ingress paths.
            if let Err(e) = crate::tx_admission::admit_spend_bundle(
                store.as_ref(),
                &mempool,
                &constants,
                &tx_announce,
                tx,
            )
            .await
            {
                debug!("gossiped transaction rejected error={}", e);
            }
        }
    }
}

// The weight-proof serving worker (the server arm of request_proof_of_weight): drains the bounded RequestProofOfWeight inbox OFF the websocket read
// loop, builds each requested proof through the crate's WeightProofServer — whose internal lock +
// tip-keyed cache is the single-flight — and responds to the requesting peer with the request
// id. Refusals send nothing: unknown tip and tip below WEIGHT_PROOF_RECENT_BLOCKS are logged and
// dropped.
pub(in crate::node) async fn weight_proof_worker<S: BlockStore + Send + Sync + 'static>(
    store: Arc<S>,
    constants: ConsensusConstants,
    wp_inbox: Arc<Mutex<Vec<WpRequest>>>,
    net: Arc<NetCounters>,
    run: Arc<AtomicBool>,
) {
    let server = dg_xch_weight_proof::serve::WeightProofServer::new(store, constants);
    while run.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let batch: Vec<WpRequest> = wp_inbox.lock().await.drain(..).collect();
        for req in batch {
            match server.get_proof_of_weight(req.tip).await {
                Ok(wp) => respond_weight_proof(&req, &wp, &net).await,
                Err(e) if e.is_refusal() => {
                    info!("refusing weight proof request tip={} error={}", req.tip, e);
                }
                Err(e) => warn!("weight proof build failed tip={} error={}", req.tip, e),
            }
        }
    }
}

// Compact-VDF solicitation scan cadence + window when `--uncompact` is on (off by default).
// The window is a bounded slice of confirmed blocks ending 5 below the peak (a block within 5 of
// the peak is never compactified).
const UNCOMPACT_INTERVAL: Duration = Duration::from_secs(300);
const UNCOMPACT_WINDOW: u32 = 256;
// The broadcast list chunks into `target_uncompact_proofs`-sized chunks (100) round-robined one
// chunk per connected timelord so blueboxes share the work rather than each grinding the whole
// list. Our fixed window rarely fills a chunk.
const UNCOMPACT_TARGET_PROOFS: usize = 100;
// Re-solicit suppression (see SolicitLedger): do not re-send a field's request for one hour (12
// scan ticks) — long enough that a connected bluebox is not spammed with duplicates while it grinds,
// short enough that a field left bulky (timelord gone / dropped the request) is retried, not
// abandoned. Capacity caps memory at a few window-fulls of distinct fields.
const UNCOMPACT_RESOLICIT_TTL: Duration = Duration::from_secs(3600);
const UNCOMPACT_LEDGER_CAP: usize = 8192;

// A minimal async seam over a solicitation target so the timelord-solicit fan-out is unit-testable
// with a recording mock: a live `SocketPeer` wraps a websocket sink that cannot be forged offline
// (WebsocketMsgStream has only TCP/TLS + hyper-upgrade variants), so the send path is proven against
// a mock here and against a real bluebox live. The production impl is `Arc<SocketPeer>`.
#[async_trait]
pub(in crate::node) trait SolicitTarget: Send + Sync {
    async fn is_timelord(&self) -> bool;
    async fn negotiated_version(&self) -> ChiaProtocolVersion;
    async fn deliver(&self, msg: dg_xch_core::protocols::ChiaMessage) -> Result<(), Error>;
}

#[async_trait]
impl SolicitTarget for Arc<SocketPeer> {
    async fn is_timelord(&self) -> bool {
        *self.node_type.read().await == NodeType::Timelord
    }
    async fn negotiated_version(&self) -> ChiaProtocolVersion {
        *self.protocol_version.read().await
    }
    async fn deliver(&self, msg: dg_xch_core::protocols::ChiaMessage) -> Result<(), Error> {
        self.send(msg).await
    }
}

// SOLICIT: hand `reqs` to connected bluebox TIMELORDS.
// Filters `peers` to timelords, chunks the list into UNCOMPACT_TARGET_PROOFS-sized chunks, and
// round-robins one chunk per timelord (each bluebox gets a different slice),
// sending each field as a RequestCompactProofOfTime. Returns the number of request messages sent.
// An empty timelord set (the network-infused case: we run without a bluebox, the network compacts
// for us) sends nothing and returns 0.
pub(in crate::node) async fn solicit_uncompact_from_timelords<T: SolicitTarget>(
    reqs: &[RequestCompactProofOfTime],
    peers: &[T],
    net: &NetCounters,
) -> usize {
    if reqs.is_empty() {
        return 0;
    }
    // Snapshot the timelord targets (async node_type read done once, not per chunk).
    let mut timelords: Vec<&T> = Vec::new();
    for p in peers {
        if p.is_timelord().await {
            timelords.push(p);
        }
    }
    if timelords.is_empty() {
        return 0;
    }
    let chunks: Vec<&[RequestCompactProofOfTime]> = reqs.chunks(UNCOMPACT_TARGET_PROOFS).collect();
    let mut sent = 0usize;
    for (i, peer) in timelords.into_iter().enumerate() {
        let chunk = chunks[i % chunks.len()];
        let version = peer.negotiated_version().await;
        for req in chunk {
            let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
                dg_xch_core::protocols::ProtocolMessageTypes::RequestCompactProofOfTime,
                version,
                req,
                None,
            ) else {
                continue;
            };
            net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::RequestCompactProofOfTime,
                msg.data.as_slice().len(),
            );
            if peer.deliver(msg).await.is_ok() {
                sent += 1;
            }
        }
    }
    sent
}

// SOLICITATION. Flag-gated OFF by default.
// Scans a bounded recent window of confirmed blocks for still-bulky VDF proofs and SENDS a
// RequestCompactProofOfTime to every connected bluebox TIMELORD (NodeType::Timelord) so it computes
// + returns the compact proof (RespondCompactProofOfTime → on_respond_compact_proof_of_time → the
// same consume/validate/replace/re-gossip path as full-node compact-VDF gossip). Re-solicit
// suppression (SolicitLedger) keeps a fixed-window scan from re-requesting the same field every
// tick. On a network-infused node with no timelord peer the scan runs and sends nothing (no target)
// — the code path exists and fires the moment a bluebox connects.
pub(in crate::node) async fn uncompact_scanner<S: BlockStore + Send + Sync + 'static>(
    store: Arc<S>,
    inbound_peers: PeerMap,
    net: Arc<NetCounters>,
    run: Arc<AtomicBool>,
) {
    let mut ledger =
        dg_xch_node::compact_vdf::SolicitLedger::new(UNCOMPACT_LEDGER_CAP, UNCOMPACT_RESOLICIT_TTL);
    while run.load(Ordering::Relaxed) {
        tokio::time::sleep(UNCOMPACT_INTERVAL).await;
        let Ok(Some((_, peak_height))) = store.get_peak().await else {
            continue;
        };
        let top = peak_height.saturating_sub(5);
        let bottom = top.saturating_sub(UNCOMPACT_WINDOW);
        let now = std::time::Instant::now();
        let mut reqs: Vec<RequestCompactProofOfTime> = Vec::new();
        for h in bottom..=top {
            let Ok(Some(rec)) = store.get_block_record_by_height(h).await else {
                continue;
            };
            let Ok(Some(block)) = store.get_block(&rec.header_hash).await else {
                continue;
            };
            reqs.extend(dg_xch_node::compact_vdf::plan_block_solicitations(
                &block,
                rec.header_hash,
                h,
                &mut ledger,
                now,
            ));
        }
        // Snapshot the inbound peers (cheap Arc clones) so the send holds no map lock; the
        // timelord filter runs inside solicit_uncompact_from_timelords.
        let peers: Vec<Arc<SocketPeer>> = inbound_peers.read().await.values().cloned().collect();
        let sent = solicit_uncompact_from_timelords(&reqs, &peers, &net).await;
        info!(
            "uncompact scan: bulky VDF proofs solicited from bluebox timelords bottom={} top={} solicited={} sent={} ledger={}",
            bottom,
            top,
            reqs.len(),
            sent,
            ledger.len()
        );
    }
}

// Send one built proof back to its requester over the link map the request arrived on, encoded at
// the peer's negotiated protocol version and carrying the request id (the requester's oneshot on
// RespondProofOfWeight matches by type + id). A peer that disconnected while we built is dropped.
pub(in crate::node) async fn respond_weight_proof(
    req: &WpRequest,
    wp: &WeightProof,
    net: &NetCounters,
) {
    let Some(peer) = req.peers.read().await.get(&req.peer).cloned() else {
        debug!(
            "weight proof requester disconnected before response peer={}",
            req.peer
        );
        return;
    };
    let version = *peer.protocol_version.read().await;
    let resp = RespondProofOfWeight {
        wp: wp.clone(),
        tip: req.tip,
    };
    let msg = match dg_xch_core::protocols::ChiaMessage::new(
        dg_xch_core::protocols::ProtocolMessageTypes::RespondProofOfWeight,
        version,
        &resp,
        req.id,
    ) {
        Ok(msg) => msg,
        Err(e) => {
            warn!("failed to serialize RespondProofOfWeight error={}", e);
            return;
        }
    };
    net.count_out(
        dg_xch_core::protocols::ProtocolMessageTypes::RespondProofOfWeight,
        msg.data.as_slice().len(),
    );
    if let Err(e) = peer.send(msg).await {
        warn!(
            "failed to send RespondProofOfWeight peer={} error={}",
            req.peer, e
        );
    }
}
