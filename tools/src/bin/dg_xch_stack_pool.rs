use blst::min_pk::SecretKey;
use clap::{Parser, ValueEnum};
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::api::pool::{DefaultPoolClient, PoolClient};
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::tx_status::TXStatus;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::config::PoolWalletConfig;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_core::pool::PoolState;
use dg_xch_core::protocols::pool::PoolVersion;
use dg_xch_farmer::farmer::config::Config;
use dg_xch_keys::{
    master_sk_to_pooling_authentication_sk, master_sk_to_singleton_owner_sk, master_sk_to_wallet_sk,
};
use dg_xch_pool::config::{PayoutConfig, PoolConfig, PoolTls};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    DEFAULT_HIDDEN_PUZZLE_TREE_HASH, calculate_synthetic_secret_key, puzzle_hash_for_pk,
};
use dg_xch_servers::chain_config::{read_selection, write_new};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Error, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

#[derive(Clone, Copy, ValueEnum)]
enum Action {
    Prepare,
    Register,
}

#[derive(Parser)]
#[command(
    about = "Prepare and register portable pooling identities on an existing disposable Compose chain"
)]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(value_enum)]
    action: Action,
    #[arg(long, default_value_t = 1800)]
    timeout_seconds: u64,
}

#[derive(Serialize, Deserialize)]
struct Launch {
    launcher_id: Bytes32,
    contract: Bytes32,
    singleton: Coin,
    version: PoolVersion,
    bundle: SpendBundle,
}

fn read(path: &Path, maximum: u64) -> Result<Zeroizing<Vec<u8>>, Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return Err(Error::other(
            "stack pooling input must be a bounded regular file",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(Error::other("stack pooling input exceeds its limit"));
    }
    Ok(bytes)
}

fn key(path: &Path) -> Result<SecretKey, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::symlink_metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(Error::other(
                "development key must not be readable by other users",
            ));
        }
    }
    let encoded = read(path, 128)?;
    let decoded = Zeroizing::new(
        hex::decode(std::str::from_utf8(&encoded).map_err(Error::other)?.trim())
            .map_err(|_| Error::other("invalid development key encoding"))?,
    );
    SecretKey::from_bytes(&decoded).map_err(|_| Error::other("invalid development key"))
}

fn ensure_json<Value: Serialize>(path: &Path, value: &Value) -> Result<(), Error> {
    if path.try_exists()? {
        let existing: serde_json::Value =
            serde_json::from_slice(&read(path, 1024 * 1024)?).map_err(Error::other)?;
        if existing != serde_json::to_value(value).map_err(Error::other)? {
            return Err(Error::other(format!(
                "existing pooling configuration differs: {}",
                path.display()
            )));
        }
        Ok(())
    } else {
        write_new(
            path,
            &serde_json::to_vec_pretty(value).map_err(Error::other)?,
        )
    }
}

fn ssl(root: &Path) -> ClientSSLConfig {
    let farmer = root.join("farmer-cpu/ssl");
    ClientSSLConfig {
        ssl_crt_path: farmer
            .join("farmer/private_farmer.crt")
            .display()
            .to_string(),
        ssl_key_path: farmer
            .join("farmer/private_farmer.key")
            .display()
            .to_string(),
        ssl_ca_crt_path: farmer.join("ca/private_ca.crt").display().to_string(),
    }
}

fn pool_keys(root: &Path) -> Result<SecretKey, Error> {
    let pool = root.join("pool");
    std::fs::create_dir_all(&pool)?;
    let secret = pool.join("payout-key.hex");
    if !secret.try_exists()? {
        if pool.join("config.json").try_exists()? {
            return Err(Error::other(
                "restore the missing pool payout key instead of replacing it",
            ));
        }
        let entropy = Zeroizing::new(rand::random::<[u8; 32]>());
        let generated = SecretKey::key_gen(&*entropy, &[])
            .map_err(|_| Error::other("pool key generation failed"))?;
        let encoded = Zeroizing::new(hex::encode(generated.to_bytes()));
        write_new(&secret, encoded.as_bytes())?;
    }
    let ca_certificate = pool.join("ca.crt");
    let ca_key = pool.join("ca.key");
    if !ca_certificate.try_exists()? && !ca_key.try_exists()? {
        let (certificate, private_key) = dg_xch_core::ssl::make_ca_cert_data()?;
        write_new(&ca_certificate, &certificate)?;
        write_new(&ca_key, &Zeroizing::new(private_key))?;
    }
    let certificate = pool.join("server.crt");
    let private_key = pool.join("server.key");
    if !certificate.try_exists()? && !private_key.try_exists()? {
        let (certificate_bytes, key_bytes) =
            dg_xch_core::ssl::generate_ca_signed_cert_data_for_host(
                &read(&ca_certificate, 1024 * 1024)?,
                &read(&ca_key, 1024 * 1024)?,
                "node-cpu",
            )?;
        write_new(&certificate, &certificate_bytes)?;
        write_new(&private_key, &Zeroizing::new(key_bytes))?;
    }
    for path in [&ca_certificate, &ca_key, &certificate, &private_key] {
        read(path, 1024 * 1024)?;
    }
    key(&secret)
}

fn account_keys(
    root: &Path,
    farmer: &str,
    version: PoolVersion,
) -> Result<(SecretKey, SecretKey), Error> {
    let master = key(&root.join(format!("farmer-{farmer}/dev-master-key.hex")))?;
    let owner = master_sk_to_singleton_owner_sk(&master, 0)?;
    if version == PoolVersion::V2 {
        let owner = calculate_synthetic_secret_key(&owner, DEFAULT_HIDDEN_PUZZLE_TREE_HASH)?;
        Ok((owner.clone(), owner))
    } else {
        Ok((
            owner,
            master_sk_to_pooling_authentication_sk(&master, 0, 0)?,
        ))
    }
}

async fn prepare(
    args: &Args,
    selection: ChainSelection,
    client: &FullnodeClient,
) -> Result<(), Error> {
    let constants = selection.constants().map_err(Error::other)?;
    let genesis = client.get_block_record_by_height(0).await?;
    if genesis.prev_hash != constants.genesis_challenge {
        return Err(Error::other(
            "node genesis does not match the stack definition",
        ));
    }
    let pool_key = pool_keys(&args.root)?;
    let target = puzzle_hash_for_pk(pool_key.sk_to_pk().to_bytes().into())?;
    let config = PoolConfig {
        chain: selection,
        trusted_genesis_header_hash: genesis.header_hash,
        listen: "0.0.0.0:8448".parse().map_err(Error::other)?,
        database: "/service/pool.sqlite".into(),
        name: "Compose reference pool".into(),
        description: "Disposable integration test".into(),
        target_puzzle_hash: target,
        relative_lock_height: 100,
        minimum_difficulty: 1,
        fee_basis_points: 0,
        authentication_token_timeout: 5,
        partial_time_limit: 60,
        max_concurrent_requests: 16,
        enable_experimental_v2: true,
        pool_memoization: SerializedProgram::from_bytes(&[0x80]),
        node_host: "localhost".into(),
        node_port: 8444,
        node_tls: ssl(Path::new("/stack")),
        tls: Some(PoolTls {
            domain: "node-cpu".into(),
            certificate: "/service/server.crt".into(),
            private_key: "/service/server.key".into(),
        }),
        payouts: Some(PayoutConfig {
            key_file: "/service/payout-key.hex".into(),
            confirmations: 2,
            transaction_fee: 0,
        }),
    };
    ensure_json(&args.root.join("pool/config.json"), &config)?;
    dg_xch_servers::app_config::AppConfig {
        version: 1,
        data_dir: "/service".into(),
        plot_directories: vec!["/plots".into()],
    }
    .ensure(&args.root.join("pool/app"))?;
    let master = key(&args.root.join("farmer-cpu/dev-master-key.hex"))?;
    let funding_hash = puzzle_hash_for_pk(
        master_sk_to_wallet_sk(&master, 0)?
            .sk_to_pk()
            .to_bytes()
            .into(),
    )?;
    let wallet =
        dg_xch_wallet::memory_wallet::MemoryWallet::new(master, client, Arc::new(constants))?;
    let coins = client
        .get_coin_records_by_puzzle_hash(&funding_hash, Some(false), None, None)
        .await?;
    let peak = client
        .get_blockchain_state()
        .await?
        .peak
        .ok_or_else(|| Error::other("node has no peak"))?;
    let mut reserved = HashSet::new();
    for farmer in ["cpu", "nvidia", "amd"] {
        let path = args.root.join(format!("farmer-{farmer}/pool-launch.json"));
        if path.try_exists()? {
            let launch: Launch =
                serde_json::from_slice(&read(&path, 1024 * 1024)?).map_err(Error::other)?;
            reserved.extend(
                launch
                    .bundle
                    .coin_spends
                    .iter()
                    .map(|spend| spend.coin.name()),
            );
        }
    }
    let mut launches = Vec::new();
    for (farmer, version) in [
        ("cpu", PoolVersion::V1),
        ("nvidia", PoolVersion::V2),
        ("amd", PoolVersion::V2),
    ] {
        let directory = args.root.join(format!("farmer-{farmer}"));
        let (owner, authentication) = account_keys(&args.root, farmer, version)?;
        let owner_public_key = Bytes48::from(owner.sk_to_pk().to_bytes());
        let path = directory.join("pool-launch.json");
        let launch: Launch = if path.try_exists()? {
            serde_json::from_slice(&read(&path, 1024 * 1024)?).map_err(Error::other)?
        } else {
            let origin = coins
                .iter()
                .find(|record| {
                    !record.spent
                        && record.coin.amount > 1000
                        && peak.height.saturating_sub(record.confirmed_block_index) >= 6
                        && !reserved.contains(&record.coin.name())
                })
                .ok_or_else(|| {
                    Error::other(
                        "CPU farmer needs three mature unspent rewards to fund pooling launches",
                    )
                })?
                .coin;
            reserved.insert(origin.name());
            let launch = if version == PoolVersion::V1 {
                dg_xch_puzzles::pool_launch::launch_v1(
                    origin,
                    &PoolState {
                        version: 1,
                        state: 3,
                        target_puzzle_hash: target,
                        owner_pubkey: owner_public_key,
                        pool_url: Some("https://node-cpu:8448".into()),
                        relative_lock_height: 100,
                    },
                    constants.genesis_challenge,
                    3600,
                    funding_hash,
                )?
            } else {
                dg_xch_puzzles::pool_v2::PlotNft::launch(
                    origin,
                    constants.genesis_challenge,
                    owner_public_key,
                    Some(dg_xch_puzzles::pool_v2::PoolConfig {
                        target,
                        relative_lock_height: 100,
                        memoization: SerializedProgram::from_bytes(&[0x80]),
                    }),
                    funding_hash,
                )?
            };
            let launcher_id = launch.launcher_id;
            let contract = launch.contract_puzzle_hash;
            let singleton = launch.singleton;
            let bundle = dg_xch_wallet::pooling::fund_launch(&wallet, origin, launch, 0).await?;
            let launch = Launch {
                launcher_id,
                contract,
                singleton,
                version,
                bundle,
            };
            ensure_json(&path, &launch)?;
            launch
        };
        if launch.version != version {
            return Err(Error::other("saved launch protocol differs"));
        }
        if client
            .get_coin_records_by_names(&[launch.singleton.name()], Some(true), None, None)
            .await?
            .is_empty()
            && client.push_tx(&launch.bundle).await? == TXStatus::FAILED
        {
            return Err(Error::other("node rejected development PlotNFT launch"));
        }
        let mut farmer_config: Config<()> =
            serde_yaml::from_slice(&read(&directory.join("config.yaml"), 1024 * 1024)?)
                .map_err(Error::other)?;
        let keys = farmer_config
            .farmer_info
            .first_mut()
            .ok_or_else(|| Error::other("farmer identity missing"))?;
        keys.launcher_id = Some(launch.launcher_id);
        keys.owner_secret_key = Some(owner.to_bytes().into());
        keys.auth_secret_key = Some(authentication.to_bytes().into());
        let farmer_public_key = hex::encode(
            SecretKey::from_bytes(keys.farmer_secret_key.as_ref())
                .map_err(|_| Error::other("invalid farmer key"))?
                .sk_to_pk()
                .to_bytes(),
        );
        farmer_config.pool_info = vec![PoolWalletConfig {
            pooling_version: version,
            launcher_id: launch.launcher_id,
            pool_url: if version == PoolVersion::V1 {
                "https://node-cpu:8448"
            } else {
                "https://node-cpu:8448/v2"
            }
            .into(),
            target_puzzle_hash: target,
            payout_instructions: dg_xch_keys::parse_payout_address(&farmer_config.payout_address)?,
            p2_singleton_puzzle_hash: launch.contract,
            owner_public_key,
            difficulty: None,
        }];
        farmer_config.pool_ca_certificates = vec!["/service/pool-ca.crt".into()];
        let encoded = Zeroizing::new(serde_yaml::to_string(&farmer_config).map_err(Error::other)?);
        let config_path = directory.join("pool-config.yaml");
        if config_path.try_exists()? {
            if read(&config_path, 1024 * 1024)?.as_slice() != encoded.as_bytes() {
                return Err(Error::other(
                    "existing pooling farmer configuration differs",
                ));
            }
        } else {
            write_new(&config_path, encoded.as_bytes())?;
        }
        let ca = read(&args.root.join("pool/ca.crt"), 1024 * 1024)?;
        let ca_path = directory.join("pool-ca.crt");
        if ca_path.try_exists()? {
            if read(&ca_path, 1024 * 1024)?.as_slice() != ca.as_slice() {
                return Err(Error::other("existing pool CA differs"));
            }
        } else {
            write_new(&ca_path, &ca)?;
        }
        ensure_json(
            &directory.join("pool-plot-keys.json"),
            &serde_json::json!({
            "farmer_public_key": farmer_public_key,
            "pool_contract_puzzle_hash": hex::encode(launch.contract), "payout_address": farmer_config.payout_address }),
        )?;
        launches.push((farmer, launch));
    }
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    loop {
        let mut confirmed = true;
        for (_, launch) in &launches {
            if client
                .get_coin_records_by_names(&[launch.singleton.name()], Some(true), None, None)
                .await?
                .is_empty()
            {
                confirmed = false;
            }
        }
        if confirmed {
            break;
        }
        if Instant::now() >= deadline {
            return Err(Error::other(
                "PlotNFT confirmation timed out; signed launches are saved and can be retried",
            ));
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    for (farmer, launch) in launches {
        println!(
            "{farmer}: {:?} PlotNFT {} confirmed",
            launch.version, launch.launcher_id
        );
    }
    Ok(())
}

async fn register(args: &Args) -> Result<(), Error> {
    let client = DefaultPoolClient::with_ca_certificates(&[read(
        &args.root.join("pool/ca.crt"),
        1024 * 1024,
    )?
    .to_vec()])?;
    for farmer in ["cpu", "nvidia", "amd"] {
        let mut config: Config<()> = serde_yaml::from_slice(&read(
            &args.root.join(format!("farmer-{farmer}/pool-config.yaml")),
            1024 * 1024,
        )?)
        .map_err(Error::other)?;
        config.pool_ca_certificates = vec![args.root.join("pool/ca.crt")];
        let pool = config
            .pool_info
            .first()
            .ok_or_else(|| Error::other("pool configuration missing"))?;
        let (owner, authentication) = account_keys(&args.root, farmer, pool.pooling_version)?;
        let info = client
            .get_pool_info(&pool.pool_url)
            .await
            .map_err(|error| Error::other(error.error_message))?;
        if info.target_puzzle_hash != pool.target_puzzle_hash
            || info.protocol_version != pool.pooling_version.protocol_number()
        {
            return Err(Error::other(
                "pool server identity differs from the prepared configuration",
            ));
        }
        let client = Arc::new(config.pool_client()?);
        let current = dg_xch_farmer::tasks::pool_state_updater::get_farmer(
            pool,
            info.authentication_token_timeout,
            &authentication,
            client.clone(),
            HashMap::new(),
            async || None,
        )
        .await;
        if let Err(error) = current {
            if error.error_code != dg_xch_core::protocols::pool::PoolErrorCode::FarmerNotKnown as u8
            {
                return Err(Error::other(error.error_message));
            }
            dg_xch_farmer::tasks::pool_state_updater::post_farmer(
                pool,
                &pool.payout_instructions,
                info.authentication_token_timeout,
                &owner,
                &HashMap::from([(pool.owner_public_key, authentication.clone())]),
                None,
                client.clone(),
                HashMap::new(),
                async || None,
            )
            .await
            .map_err(|error| Error::other(error.error_message))?;
        }
        dg_xch_farmer::tasks::pool_state_updater::get_farmer(
            pool,
            info.authentication_token_timeout,
            &authentication,
            client,
            HashMap::new(),
            async || None,
        )
        .await
        .map_err(|error| Error::other(error.error_message))?;
        println!("{farmer}: pool registration verified; existing payout settings were not changed");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    let selection = read_selection(&args.root.join("common/chain.json"))?;
    if !matches!(&selection, ChainSelection::Custom(definition) if definition.consensus.as_ref().is_some_and(|parameters| parameters.development))
    {
        return Err(Error::other(
            "pool setup is restricted to disposable development chains",
        ));
    }
    if args.timeout_seconds == 0 || args.timeout_seconds > 7200 {
        return Err(Error::other("invalid setup timeout"));
    }
    let client = FullnodeClient::new_verified("localhost", 8444, 10, Some(ssl(&args.root)), &None)?;
    match args.action {
        Action::Prepare => prepare(&args, selection, &client).await,
        Action::Register => register(&args).await,
    }
}
