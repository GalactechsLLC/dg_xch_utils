use super::*;

pub const PATH: &str = "/get_node_details";

#[portfu::prelude::post("/get_node_details", client_trust = "rpc-clients")]
pub async fn route(
    node: State<Node>,
    connection: ConnectionInfo,
    request: &mut Request,
) -> Result<Response, PortfuError> {
    check_access_policy!(
        connection,
        RpcAccessPolicy::PrivateCa | RpcAccessPolicy::Loopback
    );
    serve_portfu(node, request, |node, _body| async move {
        let mut details = Map::new();
        details.insert(
            "synced".into(),
            Value::from(node.synced.load(Ordering::Relaxed)),
        );
        let mut constants = node.constants;
        let factor = constants.difficulty_constant_factor;
        constants.difficulty_constant_factor = 0;
        let mut consensus = to_value(&constants)?;
        if let Value::Object(fields) = &mut consensus {
            fields.insert(
                "difficulty_constant_factor".into(),
                Value::from(factor.to_string()),
            );
        }
        details.insert("consensus".into(), consensus);
        details.insert(
            "transaction_announcements_queued".into(),
            Value::from(node.tx_announce.lock().await.len()),
        );
        {
            let mempool = node.mempool.lock().await;
            details.insert("mempool_items".into(), Value::from(mempool.len()));
            details.insert("mempool_cost".into(), Value::from(mempool.total_cost()));
            details.insert("mempool_fees".into(), Value::from(mempool.total_fees()));
            details.insert(
                "conflicting_transactions_cached".into(),
                Value::from(mempool.conflict_cache_len()),
            );
            details.insert(
                "conflict_cache_cost".into(),
                Value::from(mempool.conflict_cache_cost()),
            );
        }
        if let Some(live) = node.live.get() {
            details.insert("network_id".into(), Value::from(live.network_id.clone()));
            details.insert(
                "claimed_peer_peak".into(),
                Value::from(live.claimed_peak.load(Ordering::Relaxed)),
            );
            details.insert(
                "inbound_peer_count".into(),
                Value::from(live.inbound_peers.read().await.len()),
            );
            {
                let slots = live.slot_state.lock().await;
                details.insert("cached_sub_slots".into(), Value::from(slots.slot_count()));
                details.insert("slot_peak_hash".into(), to_value(&slots.peak_hash())?);
            }
            let (received, requesting, seen) = live.unfinished.lock().await.diagnostic_counts();
            details.insert("unfinished_blocks_received".into(), Value::from(received));
            details.insert(
                "unfinished_blocks_requested".into(),
                Value::from(requesting),
            );
            details.insert("unfinished_hashes_seen".into(), Value::from(seen));
        }
        Ok(Some(obj_with("node_details", Value::Object(details))))
    })
    .await
}
