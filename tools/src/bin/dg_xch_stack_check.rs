use clap::Parser;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_clients::rpc::get_client_builder;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::chain_definition::ResolvedChain;
use dg_xch_core::consensus::coinbase::{farmer_parent_id, pool_parent_id};
use dg_xch_keys::decode_puzzle_hash;
use dg_xch_servers::chain_config::read_selection;
use std::collections::HashSet;
use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    about = "Wait for a real development chain and verify cross-node agreement and farmer rewards"
)]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long, value_delimiter = ',', default_value = "cpu,nvidia,amd")]
    nodes: Vec<String>,
    #[arg(long, value_delimiter = ',', default_value = "cpu")]
    require_farmer: Vec<String>,
    #[arg(long, default_value_t = 3)]
    min_height: u32,
    #[arg(long, default_value_t = 1800)]
    timeout_seconds: u64,
    #[arg(long)]
    pooling: bool,
}

async fn client(args: &Args, node: &str) -> Result<FullnodeClient, Error> {
    let address = tokio::net::lookup_host((format!("node-{node}"), 8444))
        .await?
        .find(|address| address.is_ipv4())
        .ok_or_else(|| Error::other("node has no IPv4 address"))?;
    let farmer = args.root.join(format!("farmer-{node}/ssl"));
    let ssl = Some(ClientSSLConfig {
        ssl_crt_path: farmer
            .join("farmer/private_farmer.crt")
            .to_string_lossy()
            .into_owned(),
        ssl_key_path: farmer
            .join("farmer/private_farmer.key")
            .to_string_lossy()
            .into_owned(),
        ssl_ca_crt_path: farmer
            .join("ca/private_ca.crt")
            .to_string_lossy()
            .into_owned(),
    });
    let mut client = FullnodeClient::new_verified("localhost", 8444, 10, ssl.clone(), &None)?;
    client.client = get_client_builder(&ssl, 10)?
        .no_proxy()
        .resolve("localhost", address)
        .build()
        .map_err(Error::other)?;
    Ok(client)
}

async fn check(
    args: &Args,
    chain: &ResolvedChain,
    required: &HashSet<Bytes32>,
) -> Result<serde_json::Value, Error> {
    let mut clients = Vec::new();
    let mut shared_height = u32::MAX;
    for name in &args.nodes {
        let client = client(args, name).await?;
        let network = client.get_network_info().await?;
        if network.network_name != chain.network_id {
            return Err(Error::other(format!("node-{name} is on a different chain")));
        }
        let state = client.get_blockchain_state().await?;
        let peak = state.peak.ok_or_else(|| {
            Error::other(format!("node-{name} has not produced or received genesis"))
        })?;
        if peak.height < args.min_height {
            return Err(Error::other(format!(
                "node-{name} height {} is below {}",
                peak.height, args.min_height
            )));
        }
        shared_height = shared_height.min(peak.height);
        clients.push(client);
    }
    shared_height = shared_height.min(10_000);
    let first = clients
        .first()
        .ok_or_else(|| Error::other("no nodes selected"))?;
    let common = first.get_block_record_by_height(shared_height).await?;
    let mut summaries = Vec::new();
    for (name, client) in args.nodes.iter().zip(&clients) {
        let record = client.get_block_record_by_height(shared_height).await?;
        let genesis = client.get_block_record_by_height(0).await?;
        if record.header_hash != common.header_hash
            || genesis.prev_hash != chain.constants.genesis_challenge
        {
            return Err(Error::other(
                "nodes have not converged on the expected chain",
            ));
        }
        let genesis_block = client.get_block(&genesis.header_hash).await?;
        if genesis_block.reward_chain_block.proof_of_space.version != 1 {
            return Err(Error::other("genesis was not farmed with PoS2"));
        }
        let rewards = client
            .get_coin_records_by_parent_ids(
                &[
                    pool_parent_id(0, chain.constants.genesis_challenge),
                    farmer_parent_id(0, chain.constants.genesis_challenge),
                ],
                Some(true),
                None,
                None,
            )
            .await?;
        if rewards.iter().any(|reward| reward.coin.amount != 0) {
            return Err(Error::other("nonzero genesis reward was minted"));
        }
        summaries.push(serde_json::json!({ "node": name, "height": shared_height, "header_hash": record.header_hash }));
    }
    let mut paid = HashSet::new();
    for height in 1..=shared_height {
        let record = first.get_block_record_by_height(height).await?;
        paid.insert(record.farmer_puzzle_hash);
    }
    if !required.is_subset(&paid) {
        return Err(Error::other(
            "waiting for an accepted block from each required farmer",
        ));
    }
    let pooling = if args.pooling {
        let config: dg_xch_farmer::farmer::config::Config<()> = serde_yaml::from_slice(
            &std::fs::read(args.root.join("farmer-cpu/pool-config.yaml"))?,
        )
        .map_err(Error::other)?;
        let pool = config
            .pool_info
            .first()
            .ok_or_else(|| Error::other("pool configuration is missing"))?;
        let ca = std::fs::read(args.root.join("farmer-cpu/pool-ca.crt"))?;
        let client = dg_xch_clients::api::pool::DefaultPoolClient::with_ca_certificates(&[ca])?;
        let response = client
            .client
            .get(format!("{}/pool_stats", pool.pool_url))
            .send()
            .await
            .map_err(Error::other)?
            .error_for_status()
            .map_err(Error::other)?;
        let bytes = dg_xch_clients::http::bounded_body(response, 64 * 1024).await?;
        let statistics: serde_json::Value = serde_json::from_slice(&bytes).map_err(Error::other)?;
        if statistics["farmers"].as_u64() != Some(3)
            || statistics["accepted_partials"].as_u64().unwrap_or(0) == 0
            || statistics["confirmed_payouts"].as_u64().unwrap_or(0) == 0
        {
            return Err(Error::other(
                "waiting for three pool registrations, accepted partials, and a confirmed payout",
            ));
        }
        for payout_hash in required {
            let coins = first
                .get_coin_records_by_puzzle_hash(payout_hash, Some(true), None, None)
                .await?;
            let mut paid_by_pool = false;
            for coin in coins {
                if coin.coinbase || coin.coin.amount == 0 {
                    continue;
                }
                if let Some(parent) = first
                    .get_coin_record_by_name(&coin.coin.parent_coin_info)
                    .await?
                    && parent.spent
                    && parent.coin.puzzle_hash == pool.target_puzzle_hash
                {
                    paid_by_pool = true;
                    break;
                }
            }
            if !paid_by_pool {
                return Err(Error::other(format!(
                    "waiting for an on-chain pool payout to {payout_hash}"
                )));
            }
        }
        Some(statistics)
    } else {
        None
    };
    Ok(
        serde_json::json!({ "success": true, "nodes": summaries, "required_farmers": args.require_farmer, "genesis_prefarm": 0, "pooling": pooling }),
    )
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    if args.nodes.is_empty()
        || args.min_height == 0
        || args.min_height > 10_000
        || !(1..=86_400).contains(&args.timeout_seconds)
        || args
            .nodes
            .iter()
            .chain(&args.require_farmer)
            .any(|name| !matches!(name.as_str(), "cpu" | "nvidia" | "amd"))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid stack-check nodes, height or timeout",
        ));
    }
    let chain = read_selection(&args.root.join("common/chain.json"))?
        .resolve()
        .map_err(Error::other)?;
    if !chain.allows_bootstrap
        || chain.constants.rewards.genesis_pool != 0
        || chain.constants.rewards.genesis_farmer != 0
    {
        return Err(Error::other(
            "stack check requires a custom no-prefarm chain",
        ));
    }
    let mut required = HashSet::new();
    for farmer in &args.require_farmer {
        let keys: serde_json::Value = serde_json::from_slice(&std::fs::read(
            args.root.join(format!("farmer-{farmer}/plot-keys.json")),
        )?)
        .map_err(Error::other)?;
        let address = keys
            .get("payout_address")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::other("farmer payout address is missing"))?;
        required.insert(decode_puzzle_hash(address)?);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout_seconds);
    loop {
        match tokio::time::timeout_at(deadline, check(&args, &chain, &required)).await {
            Ok(Ok(report)) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(Error::other)?
                );
                return Ok(());
            }
            Ok(Err(error)) => eprintln!("Stack not ready: {error}"),
            Err(_) => {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "stack acceptance check timed out",
                ));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::new(
                ErrorKind::TimedOut,
                "stack acceptance check timed out",
            ));
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_secs(5)).min(deadline),
        )
        .await;
    }
}
