use super::*;

// Which unfinished-block announce a peer at `version` must receive. The broadcast branches on
// the negotiated protocol version, NOT on a Capability enum variant: newer clients get
// NewUnfinishedBlock2 and older clients get NewUnfinishedBlock, split at version 0.0.35.
// True = send v2.
#[must_use]
pub(in crate::node) fn announce_v2_for(version: ChiaProtocolVersion) -> bool {
    version > ChiaProtocolVersion::Chia0_0_35
}

// The negotiated protocol version an outbound peer reported in its handshake reply (captured by
// WsClient::perform_handshake); default when we somehow hold no handshake for it.
pub(in crate::node) fn outbound_peer_version(peer: &OutboundPeer) -> ChiaProtocolVersion {
    peer.client
        .handshake
        .as_ref()
        .map(|h| {
            ChiaProtocolVersion::from_str(&h.protocol_version)
                .expect("ChiaProtocolVersion::from_str is Infallible")
        })
        .unwrap_or_default()
}

// Drain the unfinished-block relay queue, branching per peer on the negotiated protocol version:
// NewUnfinishedBlock2 to peers > 0.0.35, NewUnfinishedBlock (reward hash only) to older peers —
// (a validated partial relays onward so the timelord's input propagates ahead of infusion).
// Each message is encoded at that peer's own version.
pub(in crate::node) async fn broadcast_ub_announcements<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announces: Vec<NewUnfinishedBlock2> = node.ub_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    let peers = registry.live_peers().await;
    if peers.is_empty() {
        info!(
            "unfinished block(s) validated but no full-node peer to announce to event={} pending={}",
            "producer.ub.no_full_node_peer",
            announces.len()
        );
        return;
    }
    for ann in announces {
        let v1 = NewUnfinishedBlock {
            unfinished_reward_hash: ann.unfinished_reward_hash,
        };
        for peer in &peers {
            let version = outbound_peer_version(peer);
            let (msg_type, msg) = if announce_v2_for(version) {
                (
                    dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock2,
                    dg_xch_core::protocols::ChiaMessage::new(
                        dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock2,
                        version,
                        &ann,
                        None,
                    ),
                )
            } else {
                (
                    dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock,
                    dg_xch_core::protocols::ChiaMessage::new(
                        dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlock,
                        version,
                        &v1,
                        None,
                    ),
                )
            };
            let Ok(msg) = msg else { continue };
            node.net.count_out(msg_type, msg.data.as_slice().len());
            let _ = peer.client.send(msg).await;
        }
        // S7 — one broadcast (to all full-node peers) per validated partial.
        node.producer.ub_broadcast("full_node");
        info!(
            "unfinished block announced to full-node peers event={} partial={} peer_type={} peers={}",
            "producer.ub.broadcast",
            ann.unfinished_reward_hash,
            "full_node",
            peers.len()
        );
    }
}

// CONSUME driver pass (`full_node.add_compact_vdf`): validate each pulled compact proof off the
// read path, swap it into the stored block, and queue a NewCompactVDF re-gossip. The block re-write
// reuses the store's INSERT-OR-REPLACE body write-through under the SAME header hash (only a witness
// changes — the block identity is unchanged), so no store surface is added.
//
// NOTE — the accept-and-replace happy path is exercised only by a genuine normalized-to-identity
// proof from a live bluebox timelord; it cannot be forged offline. The guard/reject branches are
// unit-proven (full-node/tests/compact_vdf.rs); this end-to-end acceptance is gated live.
pub(in crate::node) async fn process_compact_vdf_inbox<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
) {
    let queued: Vec<RespondCompactVDF> = node.compact_vdf_inbox.lock().await.drain(..).collect();
    if queued.is_empty() {
        return;
    }
    let Ok(Some((_, peak_height))) = node.store.get_peak().await else {
        return;
    };
    for resp in queued {
        let Ok(Some(block)) = node.store.get_block(&resp.header_hash).await else {
            continue;
        };
        if block.header_hash().ok() != Some(resp.header_hash) {
            continue;
        }
        if !dg_xch_node::compact_vdf::can_accept_compact_proof(
            &node.constants,
            &block,
            resp.field_vdf,
            &resp.vdf_info,
            &resp.vdf_proof,
            peak_height,
            resp.height,
        ) {
            debug!(
                "rejected compact vdf proof height={} field={}",
                resp.height, resp.field_vdf
            );
            continue;
        }
        let Some(new_block) = dg_xch_node::compact_vdf::replace_proof(
            &block,
            resp.field_vdf,
            &resp.vdf_info,
            &resp.vdf_proof,
        ) else {
            continue;
        };
        // Defense-in-depth: swapping a VDF *proof* (witness) must not change the block's identity —
        // the header hash commits to VdfInfo/foliage, not the proofs. `new_block` is stored under
        // `resp.header_hash`, so this guard rejects a replacement that altered a committed field
        // rather than writing content that mis-hashes its key.
        if new_block.header_hash().ok() != Some(resp.header_hash) {
            warn!(
                "compact vdf replace changed the header hash — refusing store re-write height={}",
                resp.height
            );
            continue;
        }
        // Re-write the body under the same header hash (INSERT OR REPLACE). One block, one commit.
        let rewrite = async {
            let mut batch = node.store.begin().await?;
            node.store.append_many(&mut batch, &[new_block]).await?;
            node.store.commit(batch).await
        };
        if let Err(e) = rewrite.await {
            warn!(
                "compact vdf block re-write failed height={} error={}",
                resp.height, e
            );
            continue;
        }
        info!(
            "replaced compact vdf proof height={} field={}",
            resp.height, resp.field_vdf
        );
        node.compact_vdf_announce.lock().await.push(NewCompactVDF {
            height: resp.height,
            header_hash: resp.header_hash,
            field_vdf: resp.field_vdf,
            vdf_info: resp.vdf_info,
        });
    }
}

// Drain the compact-VDF re-gossip queue as NewCompactVDF broadcasts to every live outbound peer
// (the origin peer would normally be excluded — our broadcast helpers fan out to all outbound
// peers and rely on the peers' own request-dedup, harmless).
pub(in crate::node) async fn broadcast_compact_vdf_announcements<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announces: Vec<NewCompactVDF> = node.compact_vdf_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    let version = ChiaProtocolVersion::default();
    let peers = registry.live_peers().await;
    for ann in announces {
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewCompactVdf,
            version,
            &ann,
            None,
        ) else {
            continue;
        };
        for peer in &peers {
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewCompactVdf,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg.clone()).await;
        }
    }
}

// Drain the slot-gossip announce queue to every live outbound peer (same shape as the
// transaction re-gossip).
pub(in crate::node) async fn broadcast_sp_announcements<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announces: Vec<NewSignagePointOrEndOfSubSlot> =
        node.sp_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    let version = ChiaProtocolVersion::default();
    let peers = registry.live_peers().await;
    for ann in announces {
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
            version,
            &ann,
            None,
        ) else {
            continue;
        };
        for peer in &peers {
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg.clone()).await;
        }
    }
}

// Snapshot the inbound peers that handshook as wallets (cheap Arc clones — the send loop holds no
// map lock), the NodeType::Wallet analog of the farmer/timelord snapshots below.
pub(in crate::node) async fn wallet_peers(inbound_peers: &PeerMap) -> Vec<Arc<SocketPeer>> {
    let mut wallets = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Wallet {
            wallets.push(peer.clone());
        }
    }
    wallets
}

// Push a confirmed peak to the wallet peers as NewPeakWallet. Fire-and-forget:
// a wallet that misses one re-anchors on the next peak (and can always re-page via
// RequestPuzzleState).
pub(in crate::node) async fn broadcast_new_peak_wallet(
    net: &NetCounters,
    wallets: &[Arc<SocketPeer>],
    announce: &NewPeakWallet,
) {
    for peer in wallets {
        let version = *peer.protocol_version.read().await;
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakWallet,
            version,
            announce,
            None,
        ) else {
            continue;
        };
        net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeakWallet,
            msg.data.as_slice().len(),
        );
        let _ = peer.send(msg).await;
    }
}

// Push queued farmer-form signage points to inbound peers that handshook as farmers
// (farmer_protocol.NewSignagePoint). Farmers connect INBOUND to our peer
// server, so this walks the inbound PeerMap under a NodeType::Farmer filter — distinct from the
// outbound full-node gossip relay in broadcast_sp_announcements. Fire-and-forget: a farmer that
// misses one gets the next signage point (there are 64 per slot).
pub(in crate::node) async fn broadcast_farmer_signage_points<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    inbound_peers: &PeerMap,
) {
    let announces: Vec<NewSignagePoint> = node.sp_farmer_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    // Snapshot the inbound farmer peers (cheap Arc clones) so the send loop holds no map lock.
    let mut farmers: Vec<Arc<SocketPeer>> = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Farmer {
            farmers.push(peer.clone());
        }
    }
    if farmers.is_empty() {
        return;
    }
    for ann in &announces {
        for peer in &farmers {
            let version = *peer.protocol_version.read().await;
            let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
                dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePoint,
                version,
                ann,
                None,
            ) else {
                continue;
            };
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewSignagePoint,
                msg.data.as_slice().len(),
            );
            let _ = peer.send(msg).await;
        }
    }
}

// Push queued NewUnfinishedBlockTimelord messages to inbound peers that handshook as
// timelords (`full_node.add_unfinished_block` → `send_to_all([timelord_msg], NodeType.TIMELORD)`).
// Timelords connect INBOUND to our peer server, so this walks the inbound PeerMap under a
// NodeType::Timelord filter — the timelord counterpart of broadcast_farmer_signage_points. Without
// this, a farmed UnfinishedBlock never reaches a timelord and the block never completes into a
// FullBlock. Fire-and-forget: a timelord that misses one gets the next; the partial is also relayed
// to full nodes via broadcast_ub_announcements (origin exclusion is moot — we originate here).
pub(in crate::node) async fn broadcast_ub_timelord_announcements<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    inbound_peers: &PeerMap,
) {
    let announces: Vec<NewUnfinishedBlockTimelord> =
        node.ub_timelord_announce.lock().await.drain(..).collect();
    if announces.is_empty() {
        return;
    }
    // Snapshot the inbound timelord peers (cheap Arc clones) so the send loop holds no map lock.
    let mut timelords: Vec<Arc<SocketPeer>> = Vec::new();
    for peer in inbound_peers.read().await.values() {
        if *peer.node_type.read().await == NodeType::Timelord {
            timelords.push(peer.clone());
        }
    }
    if timelords.is_empty() {
        // THE expected first-block wall: a UB is ready to infuse but no timelord is connected, so it
        // never completes into a FullBlock. Count each
        // stranded UB so the /metrics funnel names this stage exactly — the METRIC fires per UB.
        for _ in &announces {
            node.producer.candidate_dropped("no_timelord_peer");
        }
        // The LOG is debounced: on a node that intentionally runs without a timelord peer (the
        // network infuses for it) this state is EXPECTED and fired 284×/window as WARN — log noise
        // that buried real warnings. One INFO per NO_TIMELORD_LOG_SECS carries the stranded count
        // since the last line; the per-UB evidence lives in the counter, queryable any time.
        static LAST_LOG_UNIX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        static STRANDED_SINCE_LOG: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        const NO_TIMELORD_LOG_SECS: u64 = 600;
        STRANDED_SINCE_LOG.fetch_add(announces.len() as u64, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let last = LAST_LOG_UNIX.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= NO_TIMELORD_LOG_SECS
            && LAST_LOG_UNIX
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let n = STRANDED_SINCE_LOG.swap(0, Ordering::Relaxed);
            info!(
                "unfinished blocks ready but no timelord peer connected (expected on a \
                 network-infused node; see fullnode_producer_candidates_dropped_total) event={} stranded_since_last_log={}",
                "producer.ub.no_timelord_peer", n
            );
        }
        return;
    }
    for ann in &announces {
        for peer in &timelords {
            let version = *peer.protocol_version.read().await;
            let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
                dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlockTimelord,
                version,
                ann,
                None,
            ) else {
                continue;
            };
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewUnfinishedBlockTimelord,
                msg.data.as_slice().len(),
            );
            let _ = peer.send(msg).await;
        }
        // S7t — one broadcast (to all timelord peers) per ready partial.
        node.producer.ub_broadcast("timelord");
        info!(
            "unfinished block announced to timelord peers event={} partial={:?} peer_type={} timelords={}",
            "producer.ub.broadcast",
            ann.reward_chain_block.hash().ok(),
            "timelord",
            timelords.len()
        );
    }
}
