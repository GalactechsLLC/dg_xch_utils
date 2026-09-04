use super::*;

pub const PATH: &str = "/get_fee_estimate";
#[portfu::prelude::post("/get_fee_estimate", client_trust = "rpc-clients")]
pub async fn route(
    node: State<Node>,
    connection: ConnectionInfo,
    request: &mut Request,
) -> Result<Response, PortfuError> {
    check_access_policy!(
        connection,
        RpcAccessPolicy::PrivateCa | RpcAccessPolicy::Loopback
    );
    serve_portfu(node, request, |node, body| async move {
        let node = node.as_ref();
        let body = body.as_slice();
        let out = {
            let req: FeeEstimateRequest = parse(body)?;
            let spend_cost = match (req.spend_bundle, req.cost) {
                (Some(_), Some(_)) | (None, None) => {
                    return Err(RpcError::BadRequest(
                        "Request must contain exactly one of ['spend_bundle', 'cost']".to_string(),
                    ));
                }
                (None, Some(cost)) => cost,
                (Some(bundle), None) => {
                    let height = match node.store.get_peak().await? {
                        Some((header_hash, _)) => {
                            node.store
                                .get_block_record(&header_hash)
                                .await?
                                .map_or(0, |record| record.height)
                                + 1
                        }
                        None => 0,
                    };
                    conditions_from_spend_bundle(&bundle, height, &node.constants)
                        .map_err(|error| {
                            RpcError::BadRequest(format!("invalid spend_bundle: {error:?}"))
                        })?
                        .cost
                }
            };
            let mut target_times = req.target_times;
            target_times.sort_unstable();
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let (
                estimates,
                current_fee_rate,
                mempool_size,
                mempool_fees,
                num_spends,
                mempool_max_size,
            ) = {
                let mempool = node.mempool.lock().await;
                let estimator = mempool.fee_estimator();
                let raw = target_times
                    .iter()
                    .map(|target| estimator.estimate_fee_rate(*target) * spend_cost as f64)
                    .collect::<Vec<_>>();
                let estimates = make_monotonically_decreasing(&raw)
                    .into_iter()
                    .map(|estimate| estimate as u64)
                    .collect();
                (
                    estimates,
                    estimator.estimate_fee_rate(1),
                    mempool.total_cost(),
                    mempool.total_fees(),
                    mempool.len() as u64,
                    estimator.mempool_max_size(),
                )
            };
            let full_node_synced = node.synced.load(Ordering::Relaxed);
            let node_time_utc = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs());
            let peak = match node.store.get_peak().await? {
                Some((header_hash, _)) => node.store.get_block_record(&header_hash).await?,
                None => None,
            };
            let (
                peak_height,
                last_peak_timestamp,
                last_block_cost,
                fees_last_block,
                fee_rate_last_block,
                last_tx_block_height,
            ) = match &peak {
                None => (0, 0, 0, 0, 0.0, 0),
                Some(record) => {
                    let mut current = Some(record.clone());
                    let mut last_transaction = None;
                    while let Some(candidate) = current {
                        if candidate.timestamp.is_some() {
                            last_transaction = Some(candidate);
                            break;
                        }
                        current = match candidate.prev_transaction_block_hash {
                            Some(previous) => node.store.get_block_record(&previous).await?,
                            None => None,
                        };
                    }
                    match last_transaction {
                        None => (record.height, 0, 0, 0, 0.0, 0),
                        Some(transaction) => {
                            let timestamp = transaction.timestamp.unwrap_or(0);
                            let fees = transaction.fees.unwrap_or(0);
                            #[allow(clippy::cast_precision_loss)]
                            let (block_cost, fee_rate) =
                                match node.store.get_block(&transaction.header_hash).await? {
                                    Some(block) => match block.transactions_info {
                                        Some(info) if info.cost > 0 => {
                                            (info.cost, info.fees as f64 / info.cost as f64)
                                        }
                                        _ => (0, 0.0),
                                    },
                                    None => (0, 0.0),
                                };
                            (
                                record.height,
                                timestamp,
                                block_cost,
                                fees,
                                fee_rate,
                                transaction.height,
                            )
                        }
                    }
                }
            };
            let resp = FeeEstimateResponse {
                estimates,
                target_times,
                current_fee_rate,
                mempool_size,
                mempool_fees,
                num_spends,
                mempool_max_size,
                full_node_synced,
                peak_height,
                last_peak_timestamp,
                node_time_utc,
                last_block_cost,
                fees_last_block,
                fee_rate_last_block,
                last_tx_block_height,
            };
            // A flat object (no named-key wrapper), then `success` is stamped.
            match to_value(&resp)? {
                Value::Object(m) => m,
                _ => Map::new(),
            }
        };
        Ok(Some(out))
    })
    .await
}
