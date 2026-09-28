use super::*;

// The remote identity a tx-origin exclusion keys on. An INBOUND link carries the peer's true
// cert-hash id (`peer_id` is exact); every OUTBOUND dial shares OUR client cert hash
// (clients/src/websocket/mod.rs `peer_id = hash_256(certs[0])` over our OWN cert), so an outbound
// origin is only distinguishable by its dialed remote `host`.
#[derive(Clone, Copy)]
pub(in crate::node) struct TxOrigin {
    pub(in crate::node) peer_id: Bytes32,
    pub(in crate::node) host: Option<IpAddr>,
}

// Bounded insert into the tx-origin map: prune
// entries older than 60s and cap the map at 4096, so an unconsumed entry (a bundle that fails
// admission, hence is never announced/consumed) cannot grow it without bound.
pub(in crate::node) async fn record_tx_origin(
    origins: &Mutex<HashMap<Bytes32, (TxOrigin, Instant)>>,
    txid: Bytes32,
    origin: TxOrigin,
) {
    let mut o = origins.lock().await;
    o.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
    if o.len() < 4096 {
        o.insert(txid, (origin, Instant::now()));
    }
}

// Whether a FULL_NODE peer is the origin of a re-broadcast tx and must be skipped from the
// NewTransaction send. Callers pass the dispatch id for INBOUND peers (exact cert-hash match)
// and the remote host
// for OUTBOUND peers (their shared cert hash cannot identify them); the peer is the origin when
// EITHER the id or the host matches what was recorded at receipt.
pub(in crate::node) fn is_tx_rebroadcast_origin(
    origin: Option<&TxOrigin>,
    peer_id: Option<&Bytes32>,
    peer_host: Option<IpAddr>,
) -> bool {
    let Some(o) = origin else {
        return false;
    };
    if let Some(id) = peer_id
        && *id == o.peer_id
    {
        return true;
    }
    matches!((o.host, peer_host), (Some(a), Some(b)) if a == b)
}

// A tx's origin peer is excluded from the NewTransaction re-broadcast. Inbound origins are
// excluded by their exact cert-hash id; the residual is the OUTBOUND origin — every outbound
// dial shares our own client-cert hash as its dispatch id, so it cannot be identified that way.
// The exclusion records the origin's
// remote HOST too and excludes an outbound peer whose dialed host matches.
#[cfg(test)]
#[path = "../../../../tests/unit/node/workers/announcements.rs"]
mod tx_origin_exclusion_tests;

pub(in crate::node) async fn broadcast_transactions<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
) {
    let announcements: Vec<NewTransaction> = node.tx_announce.lock().await.drain(..).collect();
    if announcements.is_empty() {
        return;
    }
    // Resolve and CONSUME each announcement's origin (peer id + remote host).
    let origin_of: HashMap<Bytes32, TxOrigin> = {
        let mut origins = node.tx_origin.lock().await;
        announcements
            .iter()
            .filter_map(|tx| {
                origins
                    .remove(&tx.transaction_id)
                    .map(|(origin, _)| (tx.transaction_id, origin))
            })
            .collect()
    };
    let version = ChiaProtocolVersion::default();
    let outbound = registry.live_peers().await;
    let inbound: Vec<(Bytes32, Arc<SocketPeer>)> = node
        .inbound_peers
        .read()
        .await
        .iter()
        .map(|(id, peer)| (*id, peer.clone()))
        .collect();
    for tx in announcements {
        let origin = origin_of.get(&tx.transaction_id);
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewTransaction,
            version,
            &tx,
            None,
        ) else {
            continue;
        };
        for peer in &outbound {
            // Exclude the origin outbound peer by its dialed remote host — an outbound dial's
            // dispatch id is our own shared cert hash, so host is its only distinct identity
            // (the origin exclusion).
            if is_tx_rebroadcast_origin(origin, None, peer.endpoint.0.parse::<IpAddr>().ok()) {
                continue;
            }
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewTransaction,
                msg.data.as_slice().len(),
            );
            let _ = peer.client.send(msg.clone()).await;
        }
        for (peer_id, peer) in &inbound {
            // Inbound peers carry their true cert-hash id — exact origin match (unchanged).
            if is_tx_rebroadcast_origin(origin, Some(peer_id), None) {
                continue;
            }
            if *peer.node_type.read().await != NodeType::FullNode {
                continue;
            }
            node.net.count_out(
                dg_xch_core::protocols::ProtocolMessageTypes::NewTransaction,
                msg.data.as_slice().len(),
            );
            let _ = peer.send(msg.clone()).await;
        }
    }
}

// Send NewPeak for the just-confirmed tip to every live outbound peer. Fire-and-forget: a
// send failure only means that peer misses one announcement (the next step re-announces).
// The message carries the UNFINISHED reward-chain-block hash of the peak — peers key
// their unfinished-block caches on it — derived from
// `reward_chain_block.get_unfinished()`.
pub(in crate::node) async fn broadcast_new_peak<
    S: BlockStore + CoinStore + Send + Sync + 'static,
>(
    node: &Arc<FullNode<S>>,
    registry: &Arc<dyn OutboundPeers>,
    header_hash: Bytes32,
    height: u32,
) {
    let Ok(Some(block)) = node.store.get_block(&header_hash).await else {
        return;
    };
    let version = ChiaProtocolVersion::default();
    let unfinished = block.reward_chain_block.get_unfinished();
    let Ok(unfinished_bytes) = unfinished.to_bytes(version) else {
        return;
    };
    let peak = NewPeak {
        header_hash,
        height,
        weight: block.reward_chain_block.weight,
        fork_point_with_previous_peak: height.saturating_sub(1),
        unfinished_reward_block_hash: dg_xch_core::utils::hash_256(&unfinished_bytes).into(),
    };
    let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
        dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
        version,
        &peak,
        None,
    ) else {
        return;
    };
    for peer in registry.live_peers().await {
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            msg.data.as_slice().len(),
        );
        let _ = peer.client.send(msg.clone()).await;
    }
    // Inbound full-node peers (peers that dialed US) hear peaks too. Without this they got one
    // on-connect greeting and then silence: their book's claim for us aged past its stale TTL,
    // and a fetch frontier clamped to a servable tip we never refreshed wedged their sync while
    // their weight-heaviest target kept riding fresh gossip.
    let inbound: Vec<Arc<SocketPeer>> = {
        let mut list = Vec::new();
        for peer in node.inbound_peers.read().await.values() {
            if *peer.node_type.read().await == NodeType::FullNode {
                list.push(peer.clone());
            }
        }
        list
    };
    for peer in inbound {
        let peer_version = *peer.protocol_version.read().await;
        let Ok(msg) = dg_xch_core::protocols::ChiaMessage::new(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            peer_version,
            &peak,
            None,
        ) else {
            continue;
        };
        node.net.count_out(
            dg_xch_core::protocols::ProtocolMessageTypes::NewPeak,
            msg.data.as_slice().len(),
        );
        let _ = peer.send(msg).await;
    }
}
