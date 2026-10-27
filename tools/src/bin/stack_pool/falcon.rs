use super::*;
use dg_xch_core::{
    protocols::pool_falcon as wire,
    signatures::{SignatureSigner, falcon_keys::FalconKeyring, falcon_wallet},
};
use dg_xch_wallet::falcon::FalconWallet;

#[derive(Serialize, Deserialize)]
struct FalconLaunch {
    launcher_id: Bytes32,
    contract: Bytes32,
    singleton: Coin,
    bundle: SpendBundle,
}

async fn height(client: &FullnodeClient) -> Result<u32, Error> {
    Ok(client
        .get_blockchain_state()
        .await?
        .peak
        .ok_or_else(|| Error::other("pool node has no peak"))?
        .height)
}

async fn confirm(
    client: &FullnodeClient,
    bundle: &SpendBundle,
    deadline: Instant,
) -> Result<(), Error> {
    let outputs = bundle.net_additions()?;
    if outputs.is_empty() {
        return Err(Error::other("pool setup transaction has no final outputs"));
    }
    let ids: Vec<_> = outputs.iter().map(Coin::name).collect();
    let mut submitted = false;
    loop {
        let records = client
            .get_coin_records_by_names(&ids, Some(true), None, None)
            .await?;
        if outputs
            .iter()
            .all(|coin| records.iter().any(|r| r.coin == *coin))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::other(
                "timed out confirming Falcon pool setup transaction",
            ));
        }
        if !submitted {
            match client.push_tx(bundle).await? {
                TXStatus::SUCCESS | TXStatus::PENDING => submitted = true,
                TXStatus::FAILED => {
                    return Err(Error::other("node rejected Falcon pool setup transaction"));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn ensure_certificates(args: &Args) -> Result<(), Error> {
    let root = args.root.join("pool");
    let ca = root.join("ca.crt");
    let ca_key = root.join("ca.key");
    if !ca.try_exists()? && !ca_key.try_exists()? {
        let (certificate, key) = dg_xch_core::ssl::make_ca_cert_data()?;
        write_new(&ca, &certificate)?;
        write_new(&ca_key, &Zeroizing::new(key))?;
    }
    let host = dg_xch_clients::api::pool_falcon::host(&args.pool_url)?;
    if !root.join("server.crt").try_exists()? && !root.join("server.key").try_exists()? {
        let (certificate, key) = dg_xch_core::ssl::generate_ca_signed_cert_data_for_host(
            &read(&ca, 1024 * 1024)?,
            &read(&ca_key, 1024 * 1024)?,
            &host,
        )?;
        write_new(&root.join("server.crt"), &certificate)?;
        write_new(&root.join("server.key"), &Zeroizing::new(key))?;
    }
    let issuer = args.root.join("node-cpu/ssl/ca");
    if !root.join("rpc.crt").try_exists()? && !root.join("rpc.key").try_exists()? {
        let (certificate, key) = dg_xch_core::ssl::generate_ca_signed_cert_data(
            &read(&issuer.join("private_ca.crt"), 1024 * 1024)?,
            &read(&issuer.join("private_ca.key"), 1024 * 1024)?,
        )?;
        write_new(&root.join("rpc.crt"), &certificate)?;
        write_new(&root.join("rpc.key"), &Zeroizing::new(key))?;
    }
    if !root.join("node-ca.crt").try_exists()? {
        write_new(
            &root.join("node-ca.crt"),
            &read(&issuer.join("private_ca.crt"), 1024 * 1024)?,
        )?;
    }
    for name in [
        "server.crt",
        "server.key",
        "rpc.crt",
        "rpc.key",
        "node-ca.crt",
    ] {
        read(&root.join(name), 1024 * 1024)?;
    }
    Ok(())
}

async fn prepare_falcon(
    args: &Args,
    selection: ChainSelection,
    client: &FullnodeClient,
) -> Result<(), Error> {
    let constants = selection.constants().map_err(Error::other)?;
    let genesis = client.get_block_record_by_height(0).await?;
    if genesis.prev_hash != constants.genesis_challenge {
        return Err(Error::other("pool node is on a different chain"));
    }
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    let root = args.root.join("pool");
    std::fs::create_dir_all(&root)?;
    let key_file = root.join("falcon-keys.bin");
    if !key_file.try_exists()? {
        if root.join("config.json").try_exists()? {
            return Err(Error::other(
                "restore the existing pool key instead of replacing it",
            ));
        }
        write_new(&key_file, &FalconKeyring::generate(1)?.encode())?;
    }
    let pool_keys = FalconKeyring::load(&key_file)?;
    let pool_wallet = FalconWallet::new(pool_keys.wallet, Arc::new(constants))?;
    let target = pool_wallet.puzzle_hash();
    ensure_certificates(args)?;
    let config = PoolConfig {
        chain: selection,
        trusted_genesis_header_hash: genesis.header_hash,
        listen: "0.0.0.0:8448".parse().map_err(Error::other)?,
        database: "/data/pool.sqlite".into(),
        name: args.pool_name.clone(),
        description: "FN-DSA-512 development pool. Test-chain rewards only.".into(),
        target_puzzle_hash: target,
        relative_lock_height: 100,
        minimum_difficulty: 1,
        fee_basis_points: 0,
        authentication_token_timeout: 5,
        partial_time_limit: 60,
        max_concurrent_requests: 16,
        enable_experimental_v2: false,
        pool_memoization: SerializedProgram::from_bytes(&[0x80]),
        node_host: args.node_host.clone(),
        node_port: args.node_port,
        node_tls: ClientSSLConfig {
            ssl_crt_path: "/service/rpc.crt".into(),
            ssl_key_path: "/service/rpc.key".into(),
            ssl_ca_crt_path: "/service/node-ca.crt".into(),
        },
        tls: Some(PoolTls {
            domain: dg_xch_clients::api::pool_falcon::host(&args.pool_url)?,
            certificate: "/service/server.crt".into(),
            private_key: "/service/server.key".into(),
        }),
        payouts: Some(PayoutConfig {
            key_file: "/service/falcon-keys.bin".into(),
            confirmations: 6,
            transaction_fee: 0,
        }),
    };
    config.validate()?;
    ensure_json(&root.join("config.json"), &config)?;
    for farmer in &args.farmers {
        let directory = args.root.join(format!("farmer-{farmer}"));
        let keys = FalconKeyring::load(&directory.join("falcon-keys.bin"))?;
        let wallet = FalconWallet::new(keys.wallet.clone(), Arc::new(constants))?;
        let journal = directory.join("falcon-pool-launch.json");
        let launch = if journal.try_exists()? {
            serde_json::from_slice::<FalconLaunch>(&read(&journal, 1024 * 1024)?)
                .map_err(Error::other)?
        } else {
            let mut native = wallet.sync(client).await?.native;
            if !native.iter().any(|r| !r.spent && r.coin.amount > 1) {
                let funding_file = directory.join("falcon-pool-funding.json");
                let funding = if funding_file.try_exists()? {
                    serde_json::from_slice::<SpendBundle>(&read(&funding_file, 1024 * 1024)?)
                        .map_err(Error::other)?
                } else {
                    let rewards = client
                        .get_coin_records_by_puzzle_hash(
                            &falcon_wallet::puzzle(&keys.wallet.public_key()).tree_hash(),
                            Some(false),
                            None,
                            None,
                        )
                        .await?;
                    let reward = rewards
                        .iter()
                        .filter(|r| !r.spent && r.coin.amount > 1)
                        .min_by_key(|r| r.confirmed_block_index)
                        .ok_or_else(|| {
                            Error::other("farmer has no reward available to fund its PlotNFT")
                        })?;
                    let bundle = wallet.import_reward(reward.coin, 0, height(client).await? + 1)?;
                    ensure_json(&funding_file, &bundle)?;
                    bundle
                };
                confirm(client, &funding, deadline).await?;
                native = wallet.sync(client).await?.native;
            }
            let origin = native
                .iter()
                .filter(|r| !r.spent && r.coin.amount > 1)
                .min_by_key(|r| (r.coin.amount, r.coin.name().const_bytes()))
                .ok_or_else(|| Error::other("Falcon wallet has no PlotNFT funding coin"))?
                .coin;
            let definition = dg_xch_puzzles::pool_v2::PlotNft::launch(
                origin,
                constants.genesis_challenge,
                keys.wallet.public_key(),
                Some(dg_xch_puzzles::pool_v2::PoolConfig {
                    target,
                    relative_lock_height: config.relative_lock_height,
                    memoization: config.pool_memoization.clone(),
                }),
                wallet.puzzle_hash(),
            )?;
            let launcher_id = definition.launcher_id;
            let contract = definition.contract_puzzle_hash;
            let singleton = definition.singleton;
            let bundle = dg_xch_wallet::pooling::fund_falcon_launch(
                &wallet,
                origin,
                definition,
                0,
                height(client).await? + 1,
            )
            .await?;
            let launch = FalconLaunch {
                launcher_id,
                contract,
                singleton,
                bundle,
            };
            ensure_json(&journal, &launch)?;
            launch
        };
        let expected = dg_xch_puzzles::pool_v2::PlotNft {
            launcher_id: launch.launcher_id,
            genesis_challenge: constants.genesis_challenge,
            synthetic_public_key: keys.wallet.public_key(),
            exiting: false,
            pool: Some(dg_xch_puzzles::pool_v2::PoolConfig {
                target,
                relative_lock_height: config.relative_lock_height,
                memoization: config.pool_memoization.clone(),
            }),
        };
        if expected.puzzle()?.tree_hash() != launch.singleton.puzzle_hash
            || dg_xch_puzzles::pool_v2::reward_puzzle(launch.launcher_id)?.tree_hash()
                != launch.contract
        {
            return Err(Error::other(
                "saved PlotNFT differs from existing farmer key or pool",
            ));
        }
        confirm(client, &launch.bundle, deadline).await?;
        let pool = wire::PoolConfig {
            pool_url: args.pool_url.clone(),
            launcher_id: launch.launcher_id,
            contract_puzzle_hash: launch.contract,
            target_puzzle_hash: target,
        };
        let mut farmer_config: Config<()> =
            serde_yaml::from_slice(&read(&directory.join("config.yaml"), 1024 * 1024)?)
                .map_err(Error::other)?;
        farmer_config.falcon_pools = vec![pool.clone()];
        farmer_config
            .pool_ca_certificates
            .push("/service/pool-ca.crt".into());
        farmer_config.validate_keys()?;
        ensure_json(&directory.join("pool-plot-keys.json"), &pool)?;
        let encoded = Zeroizing::new(serde_yaml::to_string(&farmer_config).map_err(Error::other)?);
        let output = directory.join("pool-config.yaml");
        if output.try_exists()? {
            if read(&output, 1024 * 1024)?.as_slice() != encoded.as_bytes() {
                return Err(Error::other(
                    "existing Falcon pool farmer configuration differs",
                ));
            }
        } else {
            write_new(&output, encoded.as_bytes())?;
        }
        let ca = directory.join("pool-ca.crt");
        if !ca.try_exists()? {
            write_new(&ca, &read(&root.join("ca.crt"), 1024 * 1024)?)?;
        }
        println!(
            "{farmer}: Falcon PlotNFT {} confirmed; contract {}",
            launch.launcher_id, launch.contract
        );
    }
    Ok(())
}

async fn register_falcon(args: &Args) -> Result<(), Error> {
    use std::time::{SystemTime, UNIX_EPOCH};
    for farmer in &args.farmers {
        let directory = args.root.join(format!("farmer-{farmer}"));
        let keys = FalconKeyring::load(&directory.join("falcon-keys.bin"))?;
        let mut config: Config<()> =
            serde_yaml::from_slice(&read(&directory.join("pool-config.yaml"), 1024 * 1024)?)
                .map_err(Error::other)?;
        config.pool_ca_certificates = vec![directory.join("pool-ca.crt")];
        let constants = config.constants()?;
        let client = config.pool_client()?;
        for pool in &config.falcon_pools {
            let payout = dg_xch_keys::parse_payout_address(&config.payout_address)?
                .parse()
                .map_err(Error::other)?;
            let request = wire::sign(
                &keys.wallet,
                constants.genesis_challenge,
                pool.target_puzzle_hash,
                "register",
                wire::Registration {
                    launcher_id: pool.launcher_id,
                    payout_puzzle_hash: payout,
                    timestamp: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(Error::other)?
                        .as_secs(),
                },
            )?;
            let response: wire::FarmerInfo = client
                .falcon_request(&pool.pool_url, "farmer", request)
                .await
                .map_err(|e| Error::other(e.error_message))?;
            if response.contract_puzzle_hash != pool.contract_puzzle_hash
                || response.launcher_id != pool.launcher_id
                || response.payout_puzzle_hash != payout
            {
                return Err(Error::other("Falcon pool registration mismatch"));
            }
            println!(
                "{farmer}: Falcon pool registration verified for {}",
                pool.launcher_id
            );
        }
    }
    Ok(())
}

pub(super) async fn run(
    args: &Args,
    selection: ChainSelection,
    client: &FullnodeClient,
) -> Result<(), Error> {
    match args.action {
        Action::Prepare => prepare_falcon(args, selection, client).await,
        Action::Register => register_falcon(args).await,
    }
}
