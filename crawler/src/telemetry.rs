//! Read-only telemetry from explicitly configured full nodes. No public peer RPCs.
use dg_xch_clients::ClientSSLConfig;
use dg_xch_servers::discovery::{Store, sqlx};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{collections::BTreeMap, io::Error, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub ssl: Option<ClientSSLConfig>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetryConfig {
    pub nodes: Vec<NodeConfig>,
    pub interval_seconds: u64,
    pub initial_lookback: u32,
    pub blocks_per_poll: u32,
}
impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            nodes: vec![],
            interval_seconds: 60,
            initial_lookback: 4608,
            blocks_per_poll: 20,
        }
    }
}
impl TelemetryConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if self.nodes.len() > 3
            || !(30..=3600).contains(&self.interval_seconds)
            || !(1..=100000).contains(&self.initial_lookback)
            || !(1..=40).contains(&self.blocks_per_poll)
        {
            return Err(Error::other("invalid telemetry limits (up to three nodes)"));
        }
        let mut names = std::collections::HashSet::new();
        let mut urls = std::collections::HashSet::new();
        for node in &self.nodes {
            let url =
                reqwest::Url::parse(&node.url).map_err(|_| Error::other("invalid node RPC URL"))?;
            if node.name.is_empty()
                || node.name.len() > 64
                || !names.insert(&node.name)
                || !urls.insert(url.to_string())
                || url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || !matches!(url.path(), "" | "/")
            {
                return Err(Error::other(
                    "node names/URLs must be unique; RPC requires an HTTPS origin without credentials or query",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Node {
    config: NodeConfig,
    client: reqwest::Client,
    counts_cache: std::sync::Arc<tokio::sync::Mutex<Option<(u64, Value)>>>,
}
impl Node {
    pub fn new(config: &NodeConfig) -> Result<Self, Error> {
        Ok(Self {
            config: config.clone(),
            counts_cache: Default::default(),
            client: dg_xch_clients::rpc::get_client(&config.ssl, 8)?,
        })
    }
    async fn rpc(&self, method: &str, body: Value) -> Result<Value, Error> {
        let mut response = self
            .client
            .post(format!(
                "{}/{method}",
                self.config.url.trim_end_matches('/')
            ))
            .json(&body)
            .send()
            .await
            .map_err(|_| Error::other(format!("{method}: connection or TLS failure")))?;
        if !response.status().is_success() {
            return Err(Error::other(format!(
                "{method}: HTTP {}",
                response.status().as_u16()
            )));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::other("RPC body interrupted"))?
        {
            if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
                return Err(Error::other("RPC response exceeds 8 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::other("invalid RPC JSON"))?;
        if value.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(Error::other(format!("{method}: RPC rejected request")));
        }
        Ok(value)
    }
    async fn record(&self, height: u32) -> Result<Value, Error> {
        let v = self
            .rpc("get_block_record_by_height", json!({"height":height}))
            .await?;
        let r = v
            .get("block_record")
            .filter(|r| r.is_object())
            .ok_or_else(|| Error::other("missing block record"))?
            .clone();
        if integer(&r["height"]) != Some(u64::from(height))
            || hash(&r["header_hash"]).is_none()
            || hash(&r["prev_hash"]).is_none()
        {
            return Err(Error::other("inconsistent block record"));
        }
        Ok(r)
    }
    async fn sample(&self, network: &str, now: u64) -> Value {
        let result=async {
            let info=self.rpc("get_network_info",json!({})).await?;
            if info["network_name"].as_str()!=Some(network) { return Err(Error::other("RPC node is on a different network")); }
            let reply=self.rpc("get_blockchain_state",json!({})).await?;
            let state=reply.get("blockchain_state").filter(|s|s.is_object()).ok_or_else(||Error::other("missing blockchain state"))?;
            let mut data=json!({"name":self.config.name,"observed_at":now,"available":true,"synced":state.pointer("/sync/synced").and_then(Value::as_bool)==Some(true) && state.pointer("/sync/sync_mode").and_then(Value::as_bool)!=Some(true),"height":state.pointer("/peak/height"),"peak_hash":state.pointer("/peak/header_hash"),"netspace_bytes":state.get("space").map(|v|v.as_str().map_or_else(||v.to_string(),str::to_owned)),"difficulty":state.get("difficulty"),"sub_slot_iters":state.get("sub_slot_iters"),"mempool_spendbundles":state.get("mempool_size"),"mempool_cost":state.get("mempool_cost"),"mempool_max_cost":state.get("mempool_max_total_cost"),"block_max_cost":state.get("block_max_cost"),"mempool_fees_mojos":state.get("mempool_fees"),"average_block_time":state.get("average_block_time"),"minimum_fee_per_cost":state.pointer("/mempool_min_fees/cost_5000000")});
            if data["synced"] != true { return Ok(data); }
            // Optional endpoints never erase an otherwise valid state sample.
            match self.rpc("get_fee_estimate",json!({"cost":5000000,"target_times":[60,300,900]})).await {
                Ok(v)=>{data["fee_estimates_mojos"]=v["estimates"].clone();data["fee_estimate_cost"]=json!(5000000);},
                Err(_)=>data["fee_estimates_mojos"]=Value::Null,
            }
            let mut cached=self.counts_cache.lock().await;
            if cached.as_ref().is_none_or(|(at,_)|now.saturating_sub(*at)>=3600) {
                let counts=match self.rpc("get_block_count_metrics",json!({})).await {
                    Ok(v)=>json!({"compact":v.pointer("/metrics/compact_blocks"),"uncompact":v.pointer("/metrics/uncompact_blocks"),"hints":v.pointer("/metrics/hint_count")}),
                    Err(_)=>Value::Null,
                };
                *cached=Some((now,counts));
            }
            if let Some((at,counts))=&*cached {data["block_counts"]=counts.clone();data["block_counts_observed_at"]=json!(at);}
            Ok::<_,Error>(data)
        }.await;
        result.unwrap_or_else(|e|json!({"name":self.config.name,"observed_at":now,"available":false,"synced":false,"error":e.to_string()}))
    }
}
pub fn integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
fn hash(value: &Value) -> Option<String> {
    let s = value.as_str()?.trim_start_matches("0x");
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}
/// Different node URLs are configuration identities, not proof of independent operators.
fn quorum(records: &[(usize, Value)]) -> Option<(u32, String, Vec<usize>)> {
    let mut groups = BTreeMap::<(u32, String), Vec<usize>>::new();
    for (index, record) in records {
        if let (Some(h), Some(hash)) = (
            integer(&record["height"]).and_then(|n| u32::try_from(n).ok()),
            hash(&record["header_hash"]),
        ) {
            groups.entry((h, hash)).or_default().push(*index);
        }
    }
    groups
        .into_iter()
        .find(|(_, nodes)| nodes.len() >= 2)
        .map(|((height, hash), nodes)| (height, hash, nodes))
}

pub async fn collect(store: &Store, config: &TelemetryConfig, nodes: &[Node]) -> Result<(), Error> {
    if nodes.is_empty() {
        return Ok(());
    }
    // Transaction-scoped leadership releases even on cancellation or process death.
    let mut tx = store.pool.begin().await.map_err(Error::other)?;
    let leader: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,73922))")
            .bind(&store.network)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::other)?;
    if !leader {
        return Ok(());
    }
    let now = store.clock().await?;
    let previous: Option<i64> =
        sqlx::query_scalar("SELECT updated_at FROM dg_discovery.chain_status WHERE network=$1")
            .bind(&store.network)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::other)?;
    if previous.is_some_and(|at| now.saturating_sub(at as u64) < config.interval_seconds) {
        return Ok(());
    }
    let samples = join_all(nodes.iter().map(|node| node.sample(&store.network, now))).await;
    let mut eligible = Vec::new();
    for (i, sample) in samples.iter().enumerate() {
        sqlx::query("INSERT INTO dg_discovery.node_samples(network,node,bucket,data) VALUES($1,$2,$3,$4) ON CONFLICT(network,node,bucket) DO UPDATE SET data=excluded.data")
            .bind(&store.network).bind(&nodes[i].config.name).bind((now/60) as i64).bind(sample).execute(&mut *tx).await.map_err(Error::other)?;
        if sample["synced"] == true
            && let Some(height) = integer(&sample["height"]).and_then(|n| u32::try_from(n).ok())
        {
            eligible.push((i, height));
        }
    }
    let mut status = json!({"observed_at":now,"nodes":samples,"quorum":false,"checkpoint_height":null,"checkpoint_hash":null,"index_error":null});
    if eligible.len() >= 2 {
        let height = eligible.iter().map(|(_, h)| *h).min().unwrap();
        let records = join_all(
            eligible
                .iter()
                .map(|(i, _)| async { (*i, nodes[*i].record(height).await) }),
        )
        .await
        .into_iter()
        .filter_map(|(i, r)| r.ok().map(|v| (i, v)))
        .collect::<Vec<_>>();
        if let Some((height, checkpoint, agreeing)) = quorum(&records) {
            status["quorum"] = json!(true);
            status["checkpoint_height"] = json!(height);
            status["checkpoint_hash"] = json!(checkpoint);
            status["agreeing_nodes"] = json!(
                agreeing
                    .iter()
                    .map(|i| nodes[*i].config.name.clone())
                    .collect::<Vec<_>>()
            );
            let source = &nodes[agreeing[0]];
            status["source_node"] = json!(source.config.name);
            // A savepoint prevents publishing a partially reconciled canonical chain.
            sqlx::query("SAVEPOINT chain_index")
                .execute(&mut *tx)
                .await
                .map_err(Error::other)?;
            match tokio::time::timeout(
                Duration::from_secs(40),
                index(&mut tx, &store.network, source, height, &checkpoint, config),
            )
            .await
            {
                Ok(Ok(v)) => status["index"] = v,
                result => {
                    let e = match result {
                        Ok(Err(e)) => e,
                        Err(_) => Error::other(
                            "index batch deadline exceeded; source telemetry remains available",
                        ),
                        Ok(Ok(_)) => unreachable!(),
                    };
                    sqlx::query("ROLLBACK TO SAVEPOINT chain_index")
                        .execute(&mut *tx)
                        .await
                        .map_err(Error::other)?;
                    status["index_error"] = json!(e.to_string());
                }
            }
        }
    }
    sqlx::query("INSERT INTO dg_discovery.chain_status(network,updated_at,data) VALUES($1,$2,$3) ON CONFLICT(network) DO UPDATE SET updated_at=excluded.updated_at,data=excluded.data")
        .bind(&store.network).bind(now as i64).bind(status).execute(&mut *tx).await.map_err(Error::other)?;
    sqlx::query("DELETE FROM dg_discovery.node_samples WHERE network=$1 AND (bucket<$2 OR (bucket<$3 AND bucket%60<>0))")
        .bind(&store.network).bind((now.saturating_sub(90*86400)/60) as i64).bind((now.saturating_sub(86400)/60) as i64).execute(&mut *tx).await.map_err(Error::other)?;
    tx.commit().await.map_err(Error::other)
}

/// Matching hashes form a prefix of two chains. Binary search bounds a full
/// 128-block reconciliation to ten RPCs, leaving time for indexing in the batch.
async fn common_ancestor(
    node: &Node,
    local: &[(u32, String)],
) -> Result<Option<(u32, String)>, Error> {
    let Some(last) = local.last() else {
        return Ok(None);
    };
    let matches = |entry: &(u32, String), remote: &Value| {
        hash(&remote["header_hash"]).as_ref() == Some(&entry.1)
    };
    if matches(last, &node.record(last.0).await?) {
        return Ok(Some(last.clone()));
    }
    if local.len() == 1 || !matches(&local[0], &node.record(local[0].0).await?) {
        return Ok(None);
    }
    let mut lower = 0;
    let mut upper = local.len() - 1;
    while upper - lower > 1 {
        let middle = lower + (upper - lower) / 2;
        if matches(&local[middle], &node.record(local[middle].0).await?) {
            lower = middle;
        } else {
            upper = middle;
        }
    }
    Ok(Some(local[lower].clone()))
}

async fn index(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    network: &str,
    node: &Node,
    tip: u32,
    checkpoint: &str,
    c: &TelemetryConfig,
) -> Result<Value, Error> {
    let last=sqlx::query("SELECT height,hash FROM dg_discovery.chain_blocks WHERE network=$1 ORDER BY height DESC LIMIT 1")
        .bind(network).fetch_optional(&mut **tx).await.map_err(Error::other)?;
    let mut next = tip.saturating_sub(c.initial_lookback - 1);
    let mut parent = None;
    let mut removed = 0;
    if let Some(last) = last {
        let old = last.get::<i64, _>("height") as u32;
        let candidate = old.min(tip);
        let floor = candidate.saturating_sub(128);
        let rows = sqlx::query(
            "SELECT height,hash FROM dg_discovery.chain_blocks WHERE network=$1 AND height BETWEEN $2 AND $3 ORDER BY height DESC",
        )
        .bind(network)
        .bind(i64::from(floor))
        .bind(i64::from(candidate))
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::other)?;
        // Search only the contiguous retained suffix, never across a missing block.
        let mut local = Vec::with_capacity(rows.len());
        for row in rows {
            let height = row.get::<i64, _>("height") as u32;
            if candidate.checked_sub(local.len() as u32) != Some(height) {
                break;
            }
            local.push((height, row.get::<String, _>("hash")));
        }
        local.reverse();
        let (ancestor, ancestor_hash) = common_ancestor(node, &local).await?.ok_or_else(|| {
            Error::other("reorg exceeds retained reconciliation window; indexing paused")
        })?;
        removed =
            sqlx::query("DELETE FROM dg_discovery.chain_blocks WHERE network=$1 AND height>$2")
                .bind(network)
                .bind(i64::from(ancestor))
                .execute(&mut **tx)
                .await
                .map_err(Error::other)?
                .rows_affected();
        next = ancestor.saturating_add(1);
        parent = Some(ancestor_hash);
    }
    let end = tip.min(next.saturating_add(c.blocks_per_poll - 1));
    let mut added = 0;
    if next <= tip {
        for height in next..=end {
            let record = node.record(height).await?;
            let block_hash = hash(&record["header_hash"]).unwrap();
            let prev = hash(&record["prev_hash"]).unwrap();
            if parent.as_ref().is_some_and(|p| p != &prev) {
                return Err(Error::other(
                    "chain changed during indexing; retrying next poll",
                ));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            let response = node
                .rpc(
                    "get_block",
                    json!({"header_hash":format!("0x{block_hash}")}),
                )
                .await?;
            let block = response
                .get("block")
                .ok_or_else(|| Error::other("missing full block"))?;
            if integer(&block["reward_chain_block"]["height"]) != Some(u64::from(height))
                || hash(&block["foliage"]["prev_block_hash"]).as_deref() != Some(&prev)
            {
                return Err(Error::other("block/record mismatch"));
            }
            let mut data = block_metrics(&record, block)?;
            if integer(&record["timestamp"]).is_some() {
                tokio::time::sleep(Duration::from_millis(250)).await;
                // Coin deltas count coin spends, not user transactions. Reward coins are separate.
                match node
                    .rpc(
                        "get_additions_and_removals",
                        json!({"header_hash":format!("0x{block_hash}")}),
                    )
                    .await
                {
                    Ok(delta) => {
                        data["coin_spends"] = delta["removals"]
                            .as_array()
                            .map(|v| json!(v.len()))
                            .unwrap_or(Value::Null);
                        data["created_coins"] = delta["additions"]
                            .as_array()
                            .map(|v| {
                                json!(v.iter().filter(|coin| coin["coinbase"] != true).count())
                            })
                            .unwrap_or(Value::Null);
                    }
                    Err(_) => {
                        data["coin_spends"] = Value::Null;
                        data["created_coins"] = Value::Null;
                    }
                }
            }
            sqlx::query("INSERT INTO dg_discovery.chain_blocks(network,height,hash,prev_hash,timestamp,data) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(network).bind(i64::from(height)).bind(&block_hash).bind(prev).bind(integer(&record["timestamp"]).map(|n|n as i64)).bind(data).execute(&mut **tx).await.map_err(Error::other)?;
            parent = Some(block_hash);
            added += 1;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    // The agreed checkpoint must still name the same chain after all reads.
    if hash(&node.record(tip).await?["header_hash"]).as_deref() != Some(checkpoint) {
        return Err(Error::other("checkpoint changed during indexing"));
    }
    Ok(
        json!({"added_blocks":added,"reorg_removed_blocks":removed,"indexed_height":if next<=tip {end} else {tip},"target_height":tip}),
    )
}
fn block_metrics(record: &Value, block: &Value) -> Result<Value, Error> {
    let tx = integer(&record["timestamp"]).is_some();
    let cost = integer(&block["transactions_info"]["cost"]);
    let fees = integer(&record["fees"]);
    if tx && (cost.is_none() || fees.is_none()) {
        return Err(Error::other("transaction block missing cost or fees"));
    }
    Ok(
        json!({"height":record["height"],"timestamp":record["timestamp"],"transaction_block":tx,"cost":cost,"fees_mojos":fees.map(|n|n.to_string()),"farmer_puzzle_hash":record["farmer_puzzle_hash"],"pool_puzzle_hash":record["pool_puzzle_hash"],"plot_size":block.pointer("/reward_chain_block/proof_of_space/size"),"plot_version":block.pointer("/reward_chain_block/proof_of_space/version"),"coin_spends":if tx {Value::Null} else {json!(0)},"created_coins":if tx {Value::Null} else {json!(0)}}),
    )
}

pub async fn dashboard(store: &Store, interval: u64) -> Result<Value, Error> {
    let now = store.clock().await?;
    let data: Option<Value> =
        sqlx::query_scalar("SELECT data FROM dg_discovery.chain_status WHERE network=$1")
            .bind(&store.network)
            .fetch_optional(&store.pool)
            .await
            .map_err(Error::other)?;
    let mut status = data.unwrap_or_else(|| json!({"nodes":[],"quorum":false,"observed_at":null}));
    let fresh =
        integer(&status["observed_at"]).is_some_and(|at| now.saturating_sub(at) <= interval * 3);
    status["fresh"] = json!(fresh);
    if !fresh {
        status["quorum"] = json!(false);
    }
    let r=sqlx::query("SELECT min(height) AS first,max(height) AS last,count(*) AS blocks,count(*) FILTER(WHERE timestamp IS NOT NULL) AS tx_blocks,min(timestamp) AS start,max(timestamp) AS end,min(height) FILTER(WHERE timestamp IS NOT NULL) AS first_tx_height,max(height) FILTER(WHERE timestamp IS NOT NULL) AS last_tx_height,sum((data->>'fees_mojos')::numeric)::text AS fees,sum((data->>'cost')::numeric)::text AS cost,sum((data->>'coin_spends')::bigint)::text AS spends,count(*) FILTER(WHERE timestamp IS NOT NULL AND data->>'coin_spends' IS NULL) AS unknown_spends FROM dg_discovery.chain_blocks WHERE network=$1 AND height>=(SELECT max(height)-4607 FROM dg_discovery.chain_blocks WHERE network=$1)")
        .bind(&store.network).fetch_one(&store.pool).await.map_err(Error::other)?;
    let farmers=sqlx::query("SELECT data->>'farmer_puzzle_hash' AS address,count(*) AS blocks FROM dg_discovery.chain_blocks WHERE network=$1 AND height>=(SELECT max(height)-4607 FROM dg_discovery.chain_blocks WHERE network=$1) GROUP BY address ORDER BY blocks DESC,address")
        .bind(&store.network).fetch_all(&store.pool).await.map_err(Error::other)?;
    let sizes=sqlx::query("SELECT data->>'plot_size' AS size,data->>'plot_version' AS version,count(*) AS blocks FROM dg_discovery.chain_blocks WHERE network=$1 AND height>=(SELECT max(height)-4607 FROM dg_discovery.chain_blocks WHERE network=$1) GROUP BY size,version ORDER BY blocks DESC")
        .bind(&store.network).fetch_all(&store.pool).await.map_err(Error::other)?;
    let count = r.get::<i64, _>("blocks");
    let mut cumulative = 0;
    let mut concentration = None;
    for (i, farmer) in farmers.iter().enumerate() {
        cumulative += farmer.get::<i64, _>("blocks");
        if cumulative * 2 > count {
            concentration = Some(i + 1);
            break;
        }
    }
    let last = r.get::<Option<i64>, _>("last");
    let checkpoint = integer(&status["checkpoint_height"]).map(|n| n as i64);
    status["indexed"] = json!({"first_height":r.get::<Option<i64>,_>("first"),"last_height":last,"lag_blocks":checkpoint.zip(last).map(|(tip,last)|(tip-last).max(0)),"blocks":count,"transaction_blocks":r.get::<i64,_>("tx_blocks"),"first_tx_timestamp":r.get::<Option<i64>,_>("start"),"last_tx_timestamp":r.get::<Option<i64>,_>("end"),"fees_mojos":r.get::<Option<String>,_>("fees"),"cost":r.get::<Option<String>,_>("cost"),"coin_spends":r.get::<Option<String>,_>("spends"),"blocks_with_unknown_spends":r.get::<i64,_>("unknown_spends"),"reward_addresses_over_half":concentration,"top_reward_addresses":farmers.iter().take(20).map(|r|json!({"address":r.get::<Option<String>,_>("address"),"blocks":r.get::<i64,_>("blocks")})).collect::<Vec<_>>(),"plot_sizes":sizes.iter().map(|r|json!({"size":r.get::<Option<String>,_>("size"),"version":r.get::<Option<String>,_>("version"),"blocks":r.get::<i64,_>("blocks")})).collect::<Vec<_>>()});
    let duration = r
        .get::<Option<i64>, _>("end")
        .zip(r.get::<Option<i64>, _>("start"))
        .map(|(end, start)| end - start)
        .filter(|n| *n > 0);
    let height_span = r
        .get::<Option<i64>, _>("last_tx_height")
        .zip(r.get::<Option<i64>, _>("first_tx_height"))
        .map(|(last, first)| last - first)
        .filter(|n| *n > 0);
    status["indexed"]["seconds_per_block"] = json!(
        duration
            .zip(height_span)
            .map(|(seconds, blocks)| seconds as f64 / blocks as f64)
    );
    let transactions = r.get::<i64, _>("tx_blocks");
    status["indexed"]["seconds_per_transaction_block"] = json!(
        duration
            .filter(|_| transactions > 1)
            .map(|seconds| seconds as f64 / (transactions - 1) as f64)
    );
    let cost = r
        .get::<Option<String>, _>("cost")
        .and_then(|n| n.parse::<f64>().ok());
    let fees = r
        .get::<Option<String>, _>("fees")
        .and_then(|n| n.parse::<f64>().ok());
    status["indexed"]["fee_per_cost"] = json!(
        fees.zip(cost)
            .filter(|(_, cost)| *cost > 0.)
            .map(|(fees, cost)| fees / cost)
    );
    let source = status["nodes"].as_array().and_then(|nodes| {
        nodes
            .iter()
            .find(|node| node["name"] == status["source_node"])
    });
    let max_cost = source.and_then(|node| integer(&node["block_max_cost"]));
    status["indexed"]["average_block_fullness"] = json!(
        cost.zip(max_cost)
            .filter(|(_, max)| *max > 0 && transactions > 0)
            .map(|(cost, max)| cost / (max as f64 * transactions as f64))
    );
    let recent=sqlx::query("SELECT data FROM dg_discovery.chain_blocks WHERE network=$1 ORDER BY height DESC LIMIT 200")
        .bind(&store.network).fetch_all(&store.pool).await.map_err(Error::other)?;
    status["recent_blocks"] = json!(
        recent
            .into_iter()
            .rev()
            .map(|r| r.get::<Value, _>("data"))
            .collect::<Vec<_>>()
    );
    let history=sqlx::query("SELECT node,bucket,data FROM dg_discovery.node_samples WHERE network=$1 AND bucket>=$2 ORDER BY bucket DESC,node LIMIT 4320")
        .bind(&store.network).bind((now.saturating_sub(86400)/60) as i64).fetch_all(&store.pool).await.map_err(Error::other)?;
    status["history"] = json!(
        history
            .into_iter()
            .map(|r| r.get::<Value, _>("data"))
            .collect::<Vec<_>>()
    );
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quorum_requires_matching_hash_and_height() {
        let record =
            |height, byte| json!({"height":height,"header_hash":format!("{byte:02x}").repeat(32)});
        assert!(quorum(&[(0, record(10, 1)), (1, record(10, 2)), (2, record(11, 1))]).is_none());
        assert_eq!(
            quorum(&[(0, record(10, 1)), (1, record(10, 1)), (2, record(10, 2))])
                .unwrap()
                .2,
            vec![0, 1]
        );
    }
    #[test]
    fn rpc_configuration_rejects_duplicate_or_insecure_sources() {
        let node = NodeConfig {
            name: "one".into(),
            url: "https://one:8555".into(),
            ssl: None,
        };
        let mut c = TelemetryConfig {
            nodes: vec![node.clone()],
            ..Default::default()
        };
        assert!(c.validate().is_ok());
        c.nodes.push(node);
        assert!(c.validate().is_err());
        c.nodes.pop();
        c.nodes[0].url = "http://one:8555".into();
        assert!(c.validate().is_err());
    }
    #[test]
    fn metrics_preserve_exact_fees_and_unknown_coin_counts() {
        let r = json!({"timestamp":100,"fees":9007199254740993u64});
        let b = json!({"transactions_info":{"cost":42}});
        let m = block_metrics(&r, &b).unwrap();
        assert_eq!(m["fees_mojos"], "9007199254740993");
        assert!(m["coin_spends"].is_null());
        assert!(block_metrics(&r, &json!({})).is_err());
    }
}

#[cfg(test)]
#[path = "telemetry/tests.rs"]
mod integration_tests;
