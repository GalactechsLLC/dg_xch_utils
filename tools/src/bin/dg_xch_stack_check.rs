use clap::Parser;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_clients::rpc::get_client_builder;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::chain_definition::ResolvedChain;
use dg_xch_core::consensus::coinbase::{farmer_parent_id, pool_parent_id};
use dg_xch_dev_tools::stack_profile::StackProfile;
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
    /// Require this chain profile; BLS results cannot satisfy a Falcon check.
    #[arg(long, value_enum, default_value = "bls")]
    chain_profile: StackProfile,
    #[arg(long, value_delimiter = ',', default_value = "cpu,nvidia,amd")]
    nodes: Vec<String>,
    /// RPC hostnames in --nodes order; defaults to node-<name> for Compose.
    #[arg(long, value_delimiter = ',')]
    node_hosts: Vec<String>,
    #[arg(long, default_value_t = 8444, value_parser = clap::value_parser!(u16).range(1..))]
    rpc_port: u16,
    /// Certificate name to verify when connecting to the supplied RPC hosts.
    #[arg(long, default_value = "localhost")]
    tls_server_name: String,
    #[arg(long, value_delimiter = ',', default_value = "cpu")]
    require_farmer: Vec<String>,
    #[arg(long, default_value_t = 3)]
    min_height: u32,
    #[arg(long, default_value_t = 1800)]
    timeout_seconds: u64,
    #[arg(long)]
    pooling: bool,
    /// Submit and confirm a real Falcon wallet spend over the regular node RPC.
    #[arg(long)]
    falcon_spend: bool,
    /// Fund, sign and confirm a 256-authorization spend, requiring a real LaBRADOR block.
    #[arg(long)]
    labrador_spend: bool,
    /// Transfer native coins, CAT2 and NFTs between two farmer wallets, then
    /// confirm a mixed 256-signature batch on every node.
    #[arg(long)]
    wallet_transfers: bool,
}

async fn client(args: &Args, node: &str) -> Result<FullnodeClient, Error> {
    let index = args
        .nodes
        .iter()
        .position(|name| name == node)
        .ok_or_else(|| Error::other("RPC node is not selected"))?;
    let host = args
        .node_hosts
        .get(index)
        .cloned()
        .unwrap_or_else(|| format!("node-{node}"));
    let address = tokio::net::lookup_host((host, args.rpc_port))
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
    let timeout = if args.chain_profile == StackProfile::LabradorZk {
        240
    } else {
        10
    };
    let mut client = FullnodeClient::new_verified(
        &args.tls_server_name,
        args.rpc_port,
        timeout,
        ssl.clone(),
        &None,
    )?;
    client.client = get_client_builder(&ssl, timeout)?
        .no_proxy()
        .resolve(&args.tls_server_name, address)
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
        shared_height = shared_height.min(peak.height);
        clients.push(client);
    }
    shared_height = shared_height.min(10_000);
    let labrador_spend: Option<serde_json::Value> = if args.labrador_spend {
        #[cfg(feature = "labrador")]
        {
            Some(check_labrador_spend(args, chain, &clients, shared_height).await?)
        }
        #[cfg(not(feature = "labrador"))]
        {
            return Err(Error::other("LaBRADOR support is not compiled"));
        }
    } else {
        None
    };
    if shared_height < args.min_height {
        return Err(Error::other(format!(
            "shared height {shared_height} is below {}",
            args.min_height
        )));
    }
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
        let mut header_signatures = 0usize;
        let mut transaction_signatures = 0usize;
        let mut authorization_bytes = 0usize;
        let mut legacy_authorization_bytes = 0usize;
        let mut compact_blocks = 0usize;
        let mut labrador_blocks = 0usize;
        let mut labrador_signatures = 0usize;
        let mut labrador_bytes = 0usize;
        if !chain.constants.signature_profile.is_bls() {
            use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
            for height in 0..=shared_height {
                let block_record = client.get_block_record_by_height(height).await?;
                let block = client.get_block(&block_record.header_hash).await?;
                let auth = block.authorization.as_ref().ok_or_else(|| {
                    Error::other("node served a block without Falcon authorization")
                })?;
                auth.check_full(&block, chain.constants.genesis_challenge)?;
                header_signatures += auth.header.len();
                transaction_signatures += auth.transactions.len();
                let encoded = block
                    .authorization
                    .to_bytes(ChiaProtocolVersion::default())?;
                authorization_bytes += encoded.len();
                compact_blocks += usize::from(
                    encoded.starts_with(b"DGXAUT22") || encoded.starts_with(b"DGXAUT23"),
                );
                legacy_authorization_bytes += 49
                    + auth
                        .header
                        .iter()
                        .map(|p| 897 + 2 + p.message.len() + 666)
                        .sum::<usize>()
                    + auth.transactions.len() * 666;
                if let Some(program) = &block.transactions_generator {
                    use dg_xch_core::consensus::block_generator::{
                        BlockGeneratorFlags, BlockGeneratorInput, execute_block_generator_result,
                        validate_block_authorization,
                    };
                    if !block.transactions_generator_ref_list.is_empty() {
                        return Err(Error::other(
                            "stack checker expects self-contained generators",
                        ));
                    }
                    let conditions = execute_block_generator_result(&BlockGeneratorInput {
                        transactions_generator: program.clone(),
                        generator_refs: Vec::new(),
                        constants: chain.constants,
                        height,
                        flags: BlockGeneratorFlags::for_height(&chain.constants, height),
                    })
                    .map_err(Error::other)?;
                    validate_block_authorization(
                        &conditions,
                        &auth.transaction_witness()?,
                        &chain.constants,
                    )
                    .map_err(Error::other)?;
                    if let Some(proof) = &auth.labrador {
                        labrador_blocks += 1;
                        labrador_signatures += conditions.falcon_signatures.len();
                        labrador_bytes += proof.len();
                        legacy_authorization_bytes += conditions.falcon_signatures.len() * 666;
                    }
                }
            }
        }
        summaries.push(serde_json::json!({ "node": name, "height": shared_height, "blocks": shared_height + 1, "header_hash": record.header_hash, "falcon_header_signatures": header_signatures, "falcon_transaction_signatures": transaction_signatures, "falcon_authorization_bytes": authorization_bytes, "falcon_legacy_authorization_bytes": legacy_authorization_bytes, "falcon_blocks_with_compact_encoding": compact_blocks, "labrador_blocks": labrador_blocks, "labrador_signatures": labrador_signatures, "labrador_bytes": labrador_bytes }));
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
    let falcon_spend = if args.falcon_spend {
        #[cfg(feature = "falcon")]
        {
            Some(check_falcon_spend(args, chain, &clients, shared_height).await?)
        }
        #[cfg(not(feature = "falcon"))]
        return Err(Error::other("Falcon support is not compiled"));
    } else {
        None::<serde_json::Value>
    };
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
        serde_json::json!({ "success": true, "nodes": summaries, "required_farmers": args.require_farmer, "genesis_prefarm": 0, "pooling": pooling, "signature_profile": chain.constants.signature_profile, "falcon_aggregation": chain.constants.falcon_aggregation, "falcon_spend": falcon_spend, "labrador_spend": labrador_spend }),
    )
}

#[cfg(feature = "falcon")]
async fn check_falcon_spend(
    args: &Args,
    chain: &ResolvedChain,
    clients: &[FullnodeClient],
    height: u32,
) -> Result<serde_json::Value, Error> {
    use dg_xch_core::{
        blockchain::{coin::Coin, tx_status::TXStatus},
        signatures::{SignatureSigner, falcon_keys::FalconKeyring, falcon_wallet},
    };
    let keys = FalconKeyring::load(&args.root.join("farmer-cpu/falcon-keys.bin"))?;
    let payout = falcon_wallet::puzzle(&keys.wallet.public_key()).tree_hash();
    let coin = clients[0]
        .get_coin_records_by_puzzle_hash(&payout, Some(true), None, None)
        .await?
        .into_iter()
        .filter(|record| record.coinbase && record.coin.amount > 1)
        .min_by_key(|record| {
            (
                record.confirmed_block_index,
                record.coin.name().const_bytes(),
            )
        })
        .ok_or_else(|| Error::other("waiting for spendable Falcon reward"))?;
    let destination = Bytes32::from(dg_xch_core::utils::hash_256(
        b"dg_xch/falcon/compose/acceptance-output/v2",
    ));
    let output = Coin {
        parent_coin_info: coin.coin.name(),
        puzzle_hash: destination,
        amount: coin.coin.amount - 1,
    };
    let spend = falcon_wallet::spend(
        &keys.wallet,
        coin.coin,
        destination,
        output.amount,
        &chain.constants,
        height + 1,
    )?;
    let mut corrupt = spend.bundle.clone();
    corrupt
        .authorization
        .as_mut()
        .ok_or_else(|| Error::other("missing witness"))?
        .transactions[0][100] ^= 1;
    // Transport failures and unrelated admission errors are not evidence that
    // the node checked the signature. Authentication precedes the spent-coin
    // check, so this also remains testable on subsequent acceptance runs.
    match clients[0].push_tx(&corrupt).await {
        Err(error)
            if error
                .error
                .as_deref()
                .is_some_and(|message| message.contains("BadAggregateSignature")) => {}
        result => {
            return Err(Error::other(format!(
                "expected Falcon signature rejection, received {result:?}"
            )));
        }
    }
    if !coin.spent {
        if clients[0].push_tx(&spend.bundle).await? == TXStatus::FAILED {
            return Err(Error::other("node rejected valid Falcon spend"));
        }
        return Err(Error::other(
            "Falcon spend submitted; waiting for confirmation",
        ));
    }
    for client in clients {
        let input = client
            .get_coin_record_by_name(&coin.coin.name())
            .await?
            .ok_or_else(|| Error::other("input missing on peer"))?;
        let output_record = client
            .get_coin_record_by_name(&output.name())
            .await?
            .ok_or_else(|| Error::other("waiting for Falcon output on every node"))?;
        if !input.spent
            || output_record.confirmed_block_index != input.spent_block_index
            || input.spent_block_index > height
        {
            return Err(Error::other("waiting for agreed Falcon spend confirmation"));
        }
    }
    Ok(
        serde_json::json!({"input":coin.coin.name(), "output":output.name(), "confirmed_height":coin.spent_block_index, "fee":1, "corrupt_signature_rejected":true}),
    )
}

#[cfg(feature = "labrador")]
async fn check_labrador_spend(
    args: &Args,
    chain: &ResolvedChain,
    clients: &[FullnodeClient],
    height: u32,
) -> Result<serde_json::Value, Error> {
    use dg_xch_core::{
        blockchain::{
            coin::Coin, coin_spend::CoinSpend, spend_bundle::SpendBundle, tx_status::TXStatus,
        },
        clvm::{program::Program, sexp::SExp},
        consensus::block_generator::conditions_from_spend_bundle,
        signatures::{
            SignatureSigner, extension::FalconAuthorization,
            falcon_condition::transaction_reference, falcon_keys::FalconKeyring, falcon_wallet,
        },
    };
    use std::sync::{LazyLock, Mutex};
    if chain.constants.falcon_aggregation.is_none() {
        return Err(Error::other("LaBRADOR test requires its own chain profile"));
    }
    let first = clients.first().ok_or_else(|| Error::other("no nodes"))?;
    let keys = FalconKeyring::load(&args.root.join("farmer-cpu/falcon-keys.bin"))?;
    let pk = keys.wallet.public_key();
    let payout = falcon_wallet::puzzle(&pk).tree_hash();
    let reward = first
        .get_coin_records_by_puzzle_hash(&payout, Some(true), None, None)
        .await?
        .into_iter()
        .filter(|r| r.coinbase && r.coin.amount > 3)
        .min_by_key(|r| (r.confirmed_block_index, r.coin.name().const_bytes()))
        .ok_or_else(|| Error::other("waiting for a Falcon reward to fund the aggregate test"))?;
    let destination = Bytes32::from(dg_xch_core::utils::hash_256(
        b"dg_xch/labrador/compose/acceptance-output/v2",
    ));
    let mut list: Vec<SExp<'static>> = (0..256)
        .map(|i| {
            vec![
                SExp::from(50u8),
                SExp::from(pk.to_vec()),
                SExp::from(format!("LaBRADOR batch signature {i}").into_bytes()),
                SExp::from(Vec::<u8>::new()),
            ]
            .into()
        })
        .collect();
    list.push(
        vec![
            SExp::from(51u8),
            SExp::from(destination.const_bytes().to_vec()),
            SExp::from(reward.coin.amount - 2),
        ]
        .into(),
    );
    let puzzle = Program::new(SExp::from(1u8).cons(list.into()));
    let funded = Coin {
        parent_coin_info: reward.coin.name(),
        puzzle_hash: puzzle.tree_hash(),
        amount: reward.coin.amount - 1,
    };
    // The single-coin RPC reports a missing coin as an error. The parent query
    // returns an empty list while the funding transaction is still pending.
    let funded_record = first
        .get_coin_records_by_parent_ids(&[reward.coin.name()], Some(true), None, None)
        .await?
        .into_iter()
        .find(|record| record.coin.name() == funded.name());
    let Some(funded_record) = funded_record else {
        let funding = falcon_wallet::spend(
            &keys.wallet,
            reward.coin,
            funded.puzzle_hash,
            funded.amount,
            &chain.constants,
            height + 1,
        )?;
        if first.push_tx(&funding.bundle).await? == TXStatus::FAILED {
            return Err(Error::other("funding spend rejected"));
        }
        return Err(Error::other(
            "waiting for aggregate test funding confirmation",
        ));
    };
    let output = Coin {
        parent_coin_info: funded.name(),
        puzzle_hash: destination,
        amount: funded.amount - 1,
    };
    // Preserve randomized signatures while the background prover runs so polling
    // does not create new spend-bundle identifiers for the same transaction.
    static SIGNED: LazyLock<Mutex<Option<(Bytes32, SpendBundle)>>> =
        LazyLock::new(|| Mutex::new(None));
    let bundle = {
        let mut cache = SIGNED
            .lock()
            .map_err(|_| Error::other("test bundle lock"))?;
        if cache
            .as_ref()
            .is_none_or(|(coin, _)| *coin != funded.name())
        {
            let mut bundle = SpendBundle {
                coin_spends: vec![CoinSpend {
                    coin: funded,
                    puzzle_reveal: puzzle.serialized().map_err(Error::other)?,
                    solution: Program::to(Vec::<u8>::new())
                        .serialized()
                        .map_err(Error::other)?,
                }],
                ..SpendBundle::empty()
            };
            let conditions = conditions_from_spend_bundle(&bundle, height + 1, &chain.constants)
                .map_err(Error::other)?;
            if conditions.falcon_signatures.len() != 256 {
                return Err(Error::other("incorrect aggregate test coverage"));
            }
            let signatures = conditions
                .falcon_signatures
                .iter()
                .map(|r| {
                    keys.wallet
                        .sign_message(r.message.as_slice())
                        .map_err(Error::other)
                })
                .collect::<Result<Vec<_>, _>>()?;
            bundle.aggregated_signature = transaction_reference(
                chain.constants.genesis_challenge,
                &conditions.falcon_signatures,
            );
            bundle.authorization = FalconAuthorization {
                genesis: chain.constants.genesis_challenge,
                header: vec![],
                transactions: signatures,
                labrador: None,
            }
            .into();
            *cache = Some((funded.name(), bundle));
        }
        cache.as_ref().unwrap().1.clone()
    };
    // A node remembers rejected bundle IDs. Record the explicit signature
    // rejection once; polling that same invalid bundle only tests its seen cache.
    static REJECTED: Mutex<Option<Bytes32>> = Mutex::new(None);
    let rejection_checked = *REJECTED
        .lock()
        .map_err(|_| Error::other("test rejection lock"))?
        == Some(funded.name());
    if !rejection_checked {
        let mut corrupt = bundle.clone();
        corrupt.authorization.as_mut().unwrap().transactions[0][100] ^= 1;
        match first.push_tx(&corrupt).await {
            Err(error)
                if error
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("BadAggregateSignature")) => {}
            other => {
                return Err(Error::other(format!(
                    "corrupt batch signature was not explicitly rejected: {other:?}"
                )));
            }
        }
        *REJECTED
            .lock()
            .map_err(|_| Error::other("test rejection lock"))? = Some(funded.name());
    }
    if !funded_record.spent {
        if first.push_tx(&bundle).await? == TXStatus::FAILED {
            return Err(Error::other("aggregate test spend rejected"));
        }
        return Err(Error::other(
            "waiting for background LaBRADOR proof and transaction confirmation",
        ));
    }
    let confirmed = funded_record.spent_block_index;
    let mut bytes = 0;
    for client in clients {
        let record = client
            .get_coin_record_by_name(&output.name())
            .await?
            .ok_or_else(|| Error::other("waiting for aggregate output on all nodes"))?;
        if record.confirmed_block_index != confirmed {
            return Err(Error::other("aggregate output height mismatch"));
        }
        let block_record = client.get_block_record_by_height(confirmed).await?;
        let block = client.get_block(&block_record.header_hash).await?;
        let auth = block
            .authorization
            .as_ref()
            .ok_or_else(|| Error::other("missing Falcon authorization"))?;
        let proof = auth
            .labrador
            .as_ref()
            .ok_or_else(|| Error::other("batch was included without LaBRADOR"))?;
        if !auth.transactions.is_empty() || proof.len() >= 256 * 666 {
            return Err(Error::other(
                "aggregate did not replace and reduce the direct signatures",
            ));
        }
        bytes = proof.len();
    }
    Ok(
        serde_json::json!({"input":funded.name(),"output":output.name(),"confirmed_height":confirmed,"signatures":256,"direct_bytes":256*666,"aggregate_bytes":bytes,"corrupt_signature_rejected":true}),
    )
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    args.chain_profile.ensure_supported()?;
    if args.nodes.is_empty()
        || (!args.node_hosts.is_empty() && args.node_hosts.len() != args.nodes.len())
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
    if !args.chain_profile.matches(&chain.constants)
        || (args.falcon_spend && chain.constants.signature_profile.is_bls())
    {
        return Err(Error::other("stack profile differs from chain"));
    }
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
    let wallet_report: Option<serde_json::Value> = if args.wallet_transfers {
        if args.require_farmer.len() != 2 || args.falcon_spend || args.labrador_spend {
            return Err(Error::other(
                "wallet transfers require two farmers and cannot be combined with the legacy spend checks",
            ));
        }
        #[cfg(feature = "falcon")]
        {
            let mut clients = Vec::new();
            for name in &args.nodes {
                clients.push(client(&args, name).await?);
            }
            Some(
                tokio::time::timeout_at(
                    deadline,
                    dg_xch_dev_tools::stack_wallet::run(
                        &args.root,
                        &chain.constants,
                        &clients,
                        &args.require_farmer,
                    ),
                )
                .await
                .map_err(|_| Error::new(ErrorKind::TimedOut, "wallet scenario timed out"))??,
            )
        }
        #[cfg(not(feature = "falcon"))]
        {
            return Err(Error::other("wallet transfers require Falcon support"));
        }
    } else {
        None
    };
    loop {
        match tokio::time::timeout_at(deadline, check(&args, &chain, &required)).await {
            Ok(Ok(mut report)) => {
                if let Some(wallet_report) = &wallet_report {
                    report["wallet_transfers"] = wallet_report.clone();
                }
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
