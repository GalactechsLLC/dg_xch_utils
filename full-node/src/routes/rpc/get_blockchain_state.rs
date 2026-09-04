use super::*;

const BLOCKCHAIN_STATE_LOOKBACK: u32 = 4608;
const MAX_TX_BLOCK_WALK: u32 = 128;

pub const PATH: &str = "/get_blockchain_state";
#[portfu::prelude::post("/get_blockchain_state")]
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
        let node = node.as_ref();
        let out = {
            let synced = node.synced.load(Ordering::Relaxed);
            let node_id = node
                .live
                .get()
                .map_or_else(Bytes32::default, |live| live.node_id);
            let peak = match node.store.get_peak().await? {
                Some((header_hash, _)) => Some(
                    node.store
                        .get_block_record(&header_hash)
                        .await?
                        .ok_or_else(|| {
                            RpcError::Corrupt(format!("peak record {header_hash} missing"))
                        })?,
                ),
                None => None,
            };
            let (difficulty, sub_slot_iters) = match &peak {
                Some(record) if record.height > 0 => {
                    let previous = node.store.get_block_record(&record.prev_hash).await?;
                    let difficulty = previous.map_or(record.weight, |previous| {
                        record.weight.saturating_sub(previous.weight)
                    });
                    (
                        u64::try_from(difficulty).unwrap_or(u64::MAX),
                        record.sub_slot_iters,
                    )
                }
                _ => (
                    node.constants.difficulty_starting,
                    node.constants.sub_slot_iters_starting,
                ),
            };
            let (space, average_block_time) = match &peak {
                Some(record) if record.height > 1 => {
                    let older_height = record
                        .height
                        .saturating_sub(BLOCKCHAIN_STATE_LOOKBACK)
                        .max(1);
                    let space = match node.store.get_block_record_by_height(older_height).await? {
                        Some(older) => {
                            network_space_between(&node.constants, &older, record).unwrap_or(0)
                        }
                        None => 0,
                    };
                    (
                        space,
                        average_block_time(node, record.height, older_height).await?,
                    )
                }
                _ => (0, None),
            };
            let (mempool_size, mempool_cost, mempool_fees, mempool_max_total_cost, minimum_fee) = {
                let mempool = node.mempool.lock().await;
                (
                    mempool.len() as u64,
                    mempool.total_cost(),
                    mempool.total_fees(),
                    mempool.max_total_cost(),
                    mempool.get_min_fee_rate(5_000_000).unwrap_or(0.0),
                )
            };
            let claimed_peak = node
                .live
                .get()
                .map_or(0, |live| live.claimed_peak.load(Ordering::Relaxed));
            let peak_height = peak.as_ref().map_or(0, |record| record.height);
            let sync_mode = !synced;
            let sync = SyncStatus {
                sync_mode,
                synced,
                sync_tip_height: if sync_mode && claimed_peak == 0 {
                    peak_height
                } else if sync_mode {
                    claimed_peak
                } else {
                    0
                },
                sync_progress_height: if sync_mode { peak_height } else { 0 },
            };
            let mut state = BlockchainState {
                peak,
                genesis_challenge_initialized: true,
                sync,
                difficulty,
                sub_slot_iters,
                space,
                mempool_size,
                mempool_cost,
                mempool_min_fees: MinMempoolFees {
                    cost_5000000: minimum_fee,
                },
                mempool_max_total_cost,
                block_max_cost: node.constants.max_block_cost_clvm,
                node_id,
            };
            // serde_json holds no >u64 integers: serialize with space zeroed, then inject
            // the real value through json_u128 (see that fn's divergence note).
            let space = state.space;
            state.space = 0;
            let mut state = to_value(&state)?;
            if let Value::Object(obj) = &mut state {
                obj.insert("space".to_string(), json_u128(space));
                obj.insert(
                    "average_block_time".to_string(),
                    average_block_time.map_or(Value::Null, Value::from),
                );
                obj.insert("mempool_fees".to_string(), Value::from(mempool_fees));
            }
            obj_with("blockchain_state", state)
        };
        Ok(Some(out))
    })
    .await
}

async fn average_block_time(
    node: &Node,
    newer_height: u32,
    older_height: u32,
) -> Result<Option<u64>, RpcError> {
    let Some(newer) = nearest_transaction_block(node, newer_height).await? else {
        return Ok(None);
    };
    let Some(older) = nearest_transaction_block(node, older_height).await? else {
        return Ok(None);
    };
    let (Some(newer_time), Some(older_time)) = (newer.timestamp, older.timestamp) else {
        return Ok(None);
    };
    if newer.height <= older.height || newer_time <= older_time {
        return Ok(None);
    }
    Ok(Some(
        (newer_time - older_time) / u64::from(newer.height - older.height),
    ))
}

async fn nearest_transaction_block(
    node: &Node,
    height: u32,
) -> Result<Option<BlockRecord>, RpcError> {
    let stop = height.saturating_sub(MAX_TX_BLOCK_WALK);
    let mut height = height;
    loop {
        if let Some(record) = node.store.get_block_record_by_height(height).await?
            && record.is_transaction_block()
        {
            return Ok(Some(record));
        }
        if height == 0 || height == stop {
            return Ok(None);
        }
        height -= 1;
    }
}
