#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
use blst::min_pk::SecretKey;
use clap::Parser;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::consensus::chain_definition::ChainDefinition;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs_from_bytes, load_ssl_cert_and_key, make_ca_cert_data,
};
use dg_xch_dev_tools::stack_profile::StackProfile;
use dg_xch_farmer::farmer::config::{
    Config, FarmingInfo, HarvesterConfig, Pos2Backend, Pos2HarvesterConfig,
};
use dg_xch_keys::{
    encode_puzzle_hash, master_sk_to_farmer_sk, master_sk_to_pool_sk, master_sk_to_wallet_sk,
};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use dg_xch_servers::chain_config::{ensure_selection, read_selection, write_new};
use std::io::{Error, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

fn read_existing(
    path: &Path,
    private: bool,
    limit: u64,
) -> Result<Option<Zeroizing<Vec<u8>>>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "identity must be a regular non-symlink file: {}",
                    path.display()
                ),
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    let file = std::fs::File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?);
    #[cfg(not(unix))]
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "identity must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if private
            && (metadata.mode() & 0o077 != 0
                || metadata.nlink() != 1
                || metadata.uid() != rustix::process::geteuid().as_raw())
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                format!(
                    "private identity must be owned by the current user, owner-only, and have no hard links: {}",
                    path.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("identity file exceeds {limit} bytes: {}", path.display()),
        ));
    }
    Ok(Some(bytes))
}

fn ensure_directory(path: &Path) -> Result<(), Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("stack directory must not be a symlink: {}", path.display()),
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => std::fs::create_dir(path),
        Err(error) => Err(error),
    }
}

fn ensure_public_file(path: &Path, data: &[u8]) -> Result<(), Error> {
    match read_existing(path, false, 1024 * 1024)? {
        Some(existing) if existing.as_slice() == data => Ok(()),
        Some(_) => Err(Error::other(format!(
            "existing identity material does not match: {}",
            path.display()
        ))),
        None => write_new(path, data),
    }
}

fn ensure_json(path: &Path, expected: &serde_json::Value) -> Result<(), Error> {
    match read_existing(path, false, 64 * 1024)? {
        Some(bytes) => {
            let existing: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(Error::other)?;
            if existing != *expected {
                return Err(Error::other(format!(
                    "existing metadata does not match: {}",
                    path.display()
                )));
            }
            Ok(())
        }
        None => write_new(
            path,
            &serde_json::to_vec_pretty(expected).map_err(Error::other)?,
        ),
    }
}

fn ensure_service(root: &Path, expected: &serde_json::Value) -> Result<(), Error> {
    let path = root.join("config.json");
    if let Some(bytes) = read_existing(&path, false, 64 * 1024)? {
        let existing: serde_json::Value = serde_json::from_slice(&bytes).map_err(Error::other)?;
        if existing.get("chain") != expected.get("chain")
            || existing.get("tls") != expected.get("tls")
        {
            return Err(Error::other(format!(
                "existing service belongs to a different chain or identity: {}",
                path.display()
            )));
        }
        Ok(())
    } else {
        write_new(
            &path,
            &serde_json::to_vec_pretty(expected).map_err(Error::other)?,
        )
    }
}

#[derive(Parser)]
#[command(
    about = "Initialize disposable Docker test-stack identities; never use these keys for real funds"
)]
struct Args {
    #[arg(long)]
    root: PathBuf,
    /// Require this chain profile; unsupported profiles fail before creating files.
    #[arg(long, value_enum, default_value = "bls")]
    chain_profile: StackProfile,
    /// Run every farmer with the CPU backend, including the amd/nvidia identities.
    #[arg(long)]
    cpu_farmers: bool,
}

fn identity(root: &Path, prefix: &str, issuer: &[u8], issuer_key: &[u8]) -> Result<(), Error> {
    let certificate = root.join(format!("{prefix}.crt"));
    let key = root.join(format!("{prefix}.key"));
    match (
        read_existing(&certificate, false, 1024 * 1024)?,
        read_existing(&key, true, 1024 * 1024)?,
    ) {
        (Some(_), Some(_)) => {
            let (certificate, key) = load_ssl_cert_and_key(&certificate, &key)?;
            let _key = Zeroizing::new(key);
            let mut roots = rustls::RootCertStore::empty();
            for authority in load_certs_from_bytes(issuer)? {
                roots.add(authority).map_err(Error::other)?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
                Arc::new(roots),
                Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
            )
            .build()
            .map_err(Error::other)?;
            let certificates = load_certs_from_bytes(&certificate)?;
            let (leaf, intermediates) = certificates
                .split_first()
                .ok_or_else(|| Error::other("empty TLS identity"))?;
            verifier
                .verify_client_cert(leaf, intermediates, rustls::pki_types::UnixTime::now())
                .map_err(Error::other)?;
            return Ok(());
        }
        (None, None) => {}
        _ => {
            return Err(Error::other(format!(
                "partial TLS identity at {}",
                root.display()
            )));
        }
    }
    let (certificate_bytes, key_bytes) = generate_ca_signed_cert_data(issuer, issuer_key)?;
    let key_bytes = Zeroizing::new(key_bytes);
    write_new(&certificate, &certificate_bytes)?;
    write_new(&key, &key_bytes)
}

fn node_ca(root: &Path) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let certificate = root.join("ssl/ca/private_ca.crt");
    let key = root.join("ssl/ca/private_ca.key");
    match (
        read_existing(&certificate, false, 1024 * 1024)?,
        read_existing(&key, true, 1024 * 1024)?,
    ) {
        (Some(_), Some(_)) => {
            let pair = load_ssl_cert_and_key(&certificate, &key)?;
            if load_certs_from_bytes(&pair.0)? == load_certs_from_bytes(CHIA_CA_CRT.as_bytes())? {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "the public Chia CA cannot protect a node's private RPC interface",
                ));
            }
            return Ok(pair);
        }
        (None, None) => {}
        _ => {
            return Err(Error::other(
                "partial node CA; repair the test volume before restarting",
            ));
        }
    }
    let (certificate_bytes, key_bytes) = make_ca_cert_data()?;
    write_new(&certificate, &certificate_bytes)?;
    write_new(&key, &key_bytes)?;
    Ok((certificate_bytes, key_bytes))
}

fn create_farmer(
    root: &Path,
    chain: &ChainDefinition,
    issuer: &[u8],
    issuer_key: &[u8],
    backend: Pos2Backend,
) -> Result<(), Error> {
    let existing = if let Some(bytes) = read_existing(&root.join("config.yaml"), true, 64 * 1024)? {
        let existing: Config = serde_yaml::from_slice(&bytes).map_err(Error::other)?;
        if existing.constants()? != chain.constants().map_err(Error::other)?
            || existing
                .harvester_configs
                .pos2
                .as_ref()
                .is_none_or(|config| config.backend != backend)
        {
            return Err(Error::other(
                "existing farmer configuration does not match this chain/backend; use a separate stack directory",
            ));
        }
        existing.validate_keys()?;
        Some(existing)
    } else {
        None
    };
    ensure_public_file(&root.join("ssl/ca/private_ca.crt"), issuer)?;
    ensure_public_file(&root.join("ssl/ca/chia_ca.crt"), CHIA_CA_CRT.as_bytes())?;
    identity(
        &root.join("ssl/farmer"),
        "public_farmer",
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
    )?;
    identity(
        &root.join("ssl/farmer"),
        "private_farmer",
        issuer,
        issuer_key,
    )?;
    identity(
        &root.join("ssl/harvester"),
        "private_harvester",
        issuer,
        issuer_key,
    )?;
    if !chain
        .constants()
        .map_err(Error::other)?
        .signature_profile
        .is_bls()
    {
        #[cfg(feature = "falcon")]
        return create_falcon_farmer(root, chain, existing, backend);
        #[cfg(not(feature = "falcon"))]
        return Err(Error::other("Falcon support is not compiled"));
    }
    let master_path = root.join("dev-master-key.hex");
    let master_bytes = read_existing(&master_path, true, 64)?;
    if master_bytes.is_none()
        && (existing.is_some()
            || read_existing(&root.join("plot-keys.json"), false, 64 * 1024)?.is_some())
    {
        return Err(Error::other(
            "development master key is missing; restore it instead of generating a different farmer identity",
        ));
    }
    let master = if let Some(bytes) = master_bytes {
        let decoded = Zeroizing::new(hex::decode(bytes.as_slice()).map_err(Error::other)?);
        SecretKey::from_bytes(&decoded)
            .map_err(|_| Error::other("invalid development master key"))?
    } else {
        let entropy = Zeroizing::new(rand::random::<[u8; 32]>());
        let master = SecretKey::key_gen_v3(entropy.as_ref(), &[])
            .map_err(|_| Error::other("development key generation failed"))?;
        let encoded = Zeroizing::new(hex::encode(master.to_bytes()));
        write_new(&master_path, encoded.as_bytes())?;
        master
    };
    let farmer = master_sk_to_farmer_sk(&master)?;
    let pool = master_sk_to_pool_sk(&master)?;
    let wallet = master_sk_to_wallet_sk(&master, 0)?;
    let payout = puzzle_hash_for_pk(Bytes48::from(wallet.sk_to_pk().to_bytes()))?;
    let config: Config = Config {
        selected_network: chain.network_id.clone(),
        chain_definition: Some(chain.clone()),
        ssl_root_path: Some("/service/ssl".to_owned()),
        fullnode_ws_host: "localhost".to_owned(),
        fullnode_ws_port: 8444,
        fullnode_rpc_host: "localhost".to_owned(),
        fullnode_rpc_port: 8444,
        signage_point_timeout_seconds: Config::<()>::default().signage_point_timeout_seconds,
        falcon_key_file: None,
        #[cfg(feature = "falcon")]
        runtime_falcon_keys: None,
        farmer_info: vec![FarmingInfo {
            farmer_secret_key: Bytes32::from(farmer.to_bytes()),
            pool_secret_key: Some(Bytes32::from(pool.to_bytes())),
            ..FarmingInfo::default()
        }],
        pool_info: Vec::new(),
        falcon_pools: Vec::new(),
        pool_ca_certificates: Vec::new(),
        payout_address: encode_puzzle_hash(&payout, "dgx")?,
        harvester_configs: HarvesterConfig {
            plot_directories: vec!["/plots".to_owned()],
            druid_garden: None,
            custom_config: None,
            gigahorse: None,
            bladebit: None,
            pos2: Some(Pos2HarvesterConfig {
                backend,
                cuda_helper: if backend == Pos2Backend::Cuda {
                    Some(PathBuf::from("/usr/local/bin/dg_xch_plotter_cuda"))
                } else {
                    None
                },
                ..Pos2HarvesterConfig::default()
            }),
        },
        metrics: None,
    };
    config.validate_keys()?;
    if let Some(existing) = existing {
        let expected_keys = config
            .farmer_info
            .first()
            .ok_or_else(|| Error::other("generated farmer keys missing"))?;
        let existing_keys = existing
            .farmer_info
            .first()
            .ok_or_else(|| Error::other("existing farmer keys missing"))?;
        if existing.farmer_info.len() != 1
            || existing_keys.farmer_secret_key != expected_keys.farmer_secret_key
            || existing_keys.pool_secret_key != expected_keys.pool_secret_key
            || existing.payout_address != config.payout_address
        {
            return Err(Error::other(
                "existing farmer configuration does not match its development master key",
            ));
        }
    } else {
        let encoded = Zeroizing::new(serde_yaml::to_string(&config).map_err(Error::other)?);
        write_new(&root.join("config.yaml"), encoded.as_bytes())?;
    }
    ensure_json(
        &root.join("plot-keys.json"),
        &serde_json::json!({ "farmer_public_key": hex::encode(farmer.sk_to_pk().to_bytes()), "pool_public_key": hex::encode(pool.sk_to_pk().to_bytes()), "payout_address": config.payout_address }),
    )?;
    dg_xch_farmer::utils::ensure_farmer_tls(&root.join("ssl"))
}

#[cfg(feature = "falcon")]
fn create_falcon_farmer(
    root: &Path,
    chain: &ChainDefinition,
    existing: Option<Config>,
    backend: Pos2Backend,
) -> Result<(), Error> {
    use dg_xch_core::signatures::{SignatureSigner, falcon_keys::FalconKeyring, falcon_wallet};
    let path = root.join("falcon-keys.bin");
    let keys = if path.try_exists()? {
        FalconKeyring::load(&path)?
    } else {
        if existing.is_some() || root.join("plot-keys.json").try_exists()? {
            return Err(Error::other(
                "Falcon keys missing; restore the existing farmer identity",
            ));
        }
        // Leave room for the 26 k28 plots used by each 25 GB cluster farmer.
        let keys = FalconKeyring::generate(32)?;
        write_new(&path, &keys.encode())?;
        keys
    };
    let payout = falcon_wallet::puzzle(&keys.wallet.public_key()).tree_hash();
    let config: Config = Config {
        selected_network: chain.network_id.clone(),
        chain_definition: Some(chain.clone()),
        ssl_root_path: Some("/service/ssl".to_owned()),
        fullnode_ws_host: "localhost".to_owned(),
        fullnode_ws_port: 8444,
        fullnode_rpc_host: "localhost".to_owned(),
        fullnode_rpc_port: 8444,
        falcon_key_file: Some(PathBuf::from("/service/falcon-keys.bin")),
        runtime_falcon_keys: None,
        payout_address: encode_puzzle_hash(&payout, "dgx")?,
        harvester_configs: HarvesterConfig {
            plot_directories: vec!["/plots".to_owned()],
            druid_garden: None,
            bladebit: None,
            gigahorse: None,
            custom_config: None,
            pos2: Some(Pos2HarvesterConfig {
                backend,
                max_proofs_per_signage_point: Some(1),
                cuda_helper: (backend == Pos2Backend::Cuda)
                    .then(|| PathBuf::from("/usr/local/bin/dg_xch_plotter_cuda")),
                ..Pos2HarvesterConfig::default()
            }),
        },
        metrics: None,
        ..Config::default()
    };
    config.validate_keys()?;
    if let Some(existing) = existing {
        if existing.payout_address != config.payout_address
            || existing.falcon_key_file != config.falcon_key_file
            || !existing.farmer_info.is_empty()
            || !existing.pool_info.is_empty()
        {
            return Err(Error::other(
                "existing Falcon farmer configuration differs from its identity",
            ));
        }
    } else {
        write_new(
            &root.join("config.yaml"),
            serde_yaml::to_string(&config)
                .map_err(Error::other)?
                .as_bytes(),
        )?;
    }
    ensure_json(
        &root.join("plot-keys.json"),
        &serde_json::json!({"signature_profile":"fn_dsa512_v2", "payout_address":config.payout_address, "plot_count":keys.plots.len()}),
    )?;
    dg_xch_farmer::utils::ensure_farmer_tls(&root.join("ssl"))
}

fn service_identity(root: &Path, roots: &[u8]) -> Result<serde_json::Value, Error> {
    identity(
        &root.join("tls"),
        "identity",
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
    )?;
    ensure_public_file(&root.join("tls/roots.crt"), roots)?;
    Ok(serde_json::json!({
        "certificate": "/service/tls/identity.crt",
        "private_key": "/service/tls/identity.key",
        "ca_certificate": "/service/tls/roots.crt"
    }))
}

#[cfg(test)]
fn initialize(root: &Path) -> Result<(), Error> {
    initialize_profile(root, StackProfile::Bls, false)
}

fn read_development_selection(path: &Path) -> Result<ChainSelection, Error> {
    read_selection(path).map_err(|error| {
        Error::other(format!("cannot reuse the existing development chain: {error}; FN-DSA v2 requires fresh development volumes, keys and plots; preserve the old data separately"))
    })
}

fn initialize_profile(root: &Path, profile: StackProfile, cpu_farmers: bool) -> Result<(), Error> {
    let existing_chain = root.join("common/chain.json");
    if read_existing(&existing_chain, false, 64 * 1024)?.is_some()
        && !profile.matches(
            &read_development_selection(&existing_chain)?
                .constants()
                .map_err(Error::other)?,
        )
    {
        return Err(Error::other("stack profile differs from existing chain"));
    }
    if !root.try_exists()? {
        std::fs::create_dir_all(root)?;
    }
    ensure_directory(root)?;
    ensure_directory(&root.join("common"))?;
    for service in [
        "node-cpu",
        "node-nvidia",
        "node-amd",
        "farmer-cpu",
        "farmer-nvidia",
        "farmer-amd",
        "introducer",
        "timelord",
    ] {
        let directory = root.join(service);
        ensure_directory(&directory)?;
        if service.starts_with("node-") || service.starts_with("farmer-") {
            ensure_directory(&directory.join("ssl"))?;
            ensure_directory(&directory.join("ssl/ca"))?;
            if service.starts_with("farmer-") {
                ensure_directory(&directory.join("ssl/farmer"))?;
                ensure_directory(&directory.join("ssl/harvester"))?;
            }
        } else {
            ensure_directory(&directory.join("tls"))?;
        }
    }
    let chain_path = root.join("common/chain.json");
    let chain: ChainDefinition = if read_existing(&chain_path, false, 64 * 1024)?.is_some() {
        match read_development_selection(&chain_path)? {
            ChainSelection::Custom(chain)
                if chain
                    .consensus
                    .as_ref()
                    .is_some_and(|parameters| parameters.development) =>
            {
                chain
            }
            _ => {
                return Err(Error::other(
                    "Compose requires a versioned development chain; use fresh volumes rather than changing existing chain identities",
                ));
            }
        }
    } else {
        let seed = format!("dg_xch/compose/{}", hex::encode(rand::random::<[u8; 32]>()));
        let chain = match profile {
            StackProfile::Bls => ChainDefinition::development(seed),
            StackProfile::Falcon => ChainDefinition::falcon_development(seed),
            StackProfile::Labrador | StackProfile::LabradorZk => {
                let mut chain = ChainDefinition::falcon_development(seed);
                chain.network_id = if profile == StackProfile::LabradorZk {
                    "dgx-labrador-zk-dev-v2"
                } else {
                    "dgx-labrador-dev-v2"
                }
                .into();
                chain.falcon_aggregation = Some(if profile == StackProfile::LabradorZk {
                    dg_xch_core::signatures::labrador::LabradorProfile::Packed11ZkV2
                } else {
                    dg_xch_core::signatures::labrador::LabradorProfile::Packed11V2
                });
                chain
            }
        };
        ensure_selection(&chain_path, &ChainSelection::Custom(chain.clone()))?;
        chain
    };
    let constants = chain.constants().map_err(Error::other)?;
    if !profile.matches(&constants) {
        return Err(Error::other("stack profile differs from existing chain"));
    }
    let mut roots = Vec::new();
    for (node, farmer, backend) in [
        ("node-cpu", "farmer-cpu", Pos2Backend::Cpu),
        ("node-nvidia", "farmer-nvidia", Pos2Backend::Cuda),
        ("node-amd", "farmer-amd", Pos2Backend::Vulkan),
    ] {
        let (certificate, key) = node_ca(&root.join(node))?;
        let key = Zeroizing::new(key);
        let backend = if cpu_farmers {
            Pos2Backend::Cpu
        } else {
            backend
        };
        create_farmer(&root.join(farmer), &chain, &certificate, &key, backend)?;
        roots.extend_from_slice(&certificate);
        let config_root = root.join(node).join("config");
        ensure_directory(&config_root)?;
        let profile = dg_xch_servers::app_config::AppConfig {
            version: 1,
            data_dir: PathBuf::from("/data"),
            plot_directories: vec![PathBuf::from("/plots")],
        };
        profile.ensure(&config_root)?;
    }
    let introducer = root.join("introducer");
    {
        let tls = service_identity(&introducer, &roots)?;
        let config = serde_json::json!({ "listen": "0.0.0.0:8445", "chain": chain, "tls": tls, "peer_server_name": "localhost", "allow_private_addresses": true, "max_connections": 32, "peer_ttl_seconds": 3600 });
        ensure_service(&introducer, &config)?;
    }
    let timelord = root.join("timelord");
    {
        let tls = service_identity(
            &timelord,
            &std::fs::read(root.join("node-cpu/ssl/ca/private_ca.crt"))?,
        )?;
        let config = serde_json::json!({ "chain": chain, "fullnode_host": "localhost", "fullnode_port": 8444, "server_name": "localhost", "tls": tls, "max_iterations": 1048576, "job_timeout_seconds": 300, "reconnect_seconds": 5, "max_iterations_per_second": if constants.signature_profile.is_bls() {27} else {8} });
        ensure_service(&timelord, &config)?;
    }
    for service in [
        "farmer-cpu",
        "farmer-nvidia",
        "farmer-amd",
        "introducer",
        "timelord",
    ] {
        let config_root = root.join(service).join("app");
        ensure_directory(&config_root)?;
        dg_xch_servers::app_config::AppConfig {
            version: 1,
            data_dir: PathBuf::from("/service"),
            plot_directories: vec![PathBuf::from("/plots")],
        }
        .ensure(&config_root)?;
    }
    Ok(())
}

fn main() -> Result<(), Error> {
    let args = Args::parse();
    args.chain_profile.ensure_supported()?;
    initialize_profile(&args.root, args.chain_profile, args.cpu_farmers)?;
    eprintln!(
        "Disposable test-stack identities are ready. Generate matching PoS2 plots before starting the farmers. Never use these development keys for real funds."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    #[cfg(feature = "falcon")]
    #[test]
    fn cpu_only_falcon_stack_has_distinct_farmer_identities_and_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        initialize_profile(directory.path(), StackProfile::Falcon, true).unwrap();
        let mut payouts = std::collections::HashSet::new();
        let mut identities = Vec::new();
        for farmer in ["cpu", "nvidia", "amd"] {
            let root = directory.path().join(format!("farmer-{farmer}"));
            let config: Config =
                serde_yaml::from_slice(&std::fs::read(root.join("config.yaml")).unwrap()).unwrap();
            assert_eq!(
                config.harvester_configs.pos2.unwrap().backend,
                Pos2Backend::Cpu
            );
            assert!(config.farmer_info.is_empty());
            assert!(payouts.insert(config.payout_address));
            let path = root.join("falcon-keys.bin");
            identities.push((path.clone(), std::fs::read(path).unwrap()));
        }
        initialize_profile(directory.path(), StackProfile::Falcon, true).unwrap();
        for (path, expected) in identities {
            assert_eq!(std::fs::read(path).unwrap(), expected);
        }
    }

    fn issuer() -> &'static (Vec<u8>, Vec<u8>) {
        static ISSUER: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
        ISSUER.get_or_init(|| make_ca_cert_data().unwrap())
    }

    #[test]
    fn farmer_reinitialization_preserves_identity_and_recovers_missing_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let chain = ChainDefinition::development("initializer fixture".to_owned());
        let (certificate, key) = issuer();
        create_farmer(directory.path(), &chain, certificate, key, Pos2Backend::Cpu).unwrap();
        let config_path = directory.path().join("config.yaml");
        let master_path = directory.path().join("dev-master-key.hex");
        let metadata_path = directory.path().join("plot-keys.json");
        let config = std::fs::read(&config_path).unwrap();
        let master = std::fs::read(&master_path).unwrap();
        let metadata = std::fs::read(&metadata_path).unwrap();
        let farmer_key_path = directory.path().join("ssl/farmer/private_farmer.key");
        let farmer_key = std::fs::read(&farmer_key_path).unwrap();
        std::fs::remove_file(&metadata_path).unwrap();
        create_farmer(directory.path(), &chain, certificate, key, Pos2Backend::Cpu).unwrap();
        create_farmer(directory.path(), &chain, certificate, key, Pos2Backend::Cpu).unwrap();
        assert_eq!(std::fs::read(&config_path).unwrap(), config);
        assert_eq!(std::fs::read(&master_path).unwrap(), master);
        assert_eq!(std::fs::read(&metadata_path).unwrap(), metadata);
        assert_eq!(std::fs::read(&farmer_key_path).unwrap(), farmer_key);
        assert!(
            create_farmer(
                directory.path(),
                &chain,
                certificate,
                key,
                Pos2Backend::Vulkan
            )
            .is_err()
        );
        let different = ChainDefinition::development("different chain".to_owned());
        assert!(
            create_farmer(
                directory.path(),
                &different,
                certificate,
                key,
                Pos2Backend::Cpu
            )
            .is_err()
        );
        std::fs::remove_file(&master_path).unwrap();
        assert!(
            create_farmer(directory.path(), &chain, certificate, key, Pos2Backend::Cpu).is_err()
        );
        assert!(!master_path.exists());
        assert_eq!(std::fs::read(&config_path).unwrap(), config);
        assert_eq!(std::fs::read(&metadata_path).unwrap(), metadata);
    }

    #[test]
    fn retained_service_configuration_allows_tuning_but_rejects_identity_changes() {
        let directory = tempfile::tempdir().unwrap();
        let expected = serde_json::json!({
            "chain": ChainDefinition::development("service fixture".to_owned()),
            "tls": { "certificate": "/service/tls/identity.crt", "private_key": "/service/tls/identity.key", "ca_certificate": "/service/tls/roots.crt" },
            "max_connections": 32,
        });
        ensure_service(directory.path(), &expected).unwrap();
        let path = directory.path().join("config.json");
        let mut tuned = expected.clone();
        tuned["max_connections"] = serde_json::json!(64);
        std::fs::write(&path, serde_json::to_vec(&tuned).unwrap()).unwrap();
        ensure_service(directory.path(), &expected).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),
            tuned
        );
        let mut changed = expected.clone();
        changed["chain"] =
            serde_json::json!(ChainDefinition::development("other service".to_owned()));
        assert!(ensure_service(directory.path(), &changed).is_err());
        changed = expected.clone();
        changed["tls"]["certificate"] = serde_json::json!("/other.crt");
        assert!(ensure_service(directory.path(), &changed).is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),
            tuned
        );
    }

    #[test]
    fn public_metadata_and_roots_are_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let roots = directory.path().join("roots.crt");
        ensure_public_file(&roots, b"first CA").unwrap();
        ensure_public_file(&roots, b"first CA").unwrap();
        assert!(ensure_public_file(&roots, b"other CA").is_err());
        assert_eq!(std::fs::read(&roots).unwrap(), b"first CA");
        let metadata = directory.path().join("plot-keys.json");
        let expected = serde_json::json!({"farmer_public_key": "first"});
        ensure_json(&metadata, &expected).unwrap();
        ensure_json(&metadata, &expected).unwrap();
        assert!(
            ensure_json(
                &metadata,
                &serde_json::json!({"farmer_public_key": "other"})
            )
            .is_err()
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(metadata).unwrap()).unwrap(),
            expected
        );
    }

    #[test]
    fn partial_tls_pairs_are_not_regenerated() {
        let directory = tempfile::tempdir().unwrap();
        let certificate = directory.path().join("identity.crt");
        write_new(&certificate, CHIA_CA_CRT.as_bytes()).unwrap();
        assert!(
            identity(
                directory.path(),
                "identity",
                CHIA_CA_CRT.as_bytes(),
                CHIA_CA_KEY.as_bytes()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(certificate).unwrap(), CHIA_CA_CRT.as_bytes());
        assert!(!directory.path().join("identity.key").exists());
        let node_certificate = directory.path().join("ssl/ca/private_ca.crt");
        write_new(&node_certificate, CHIA_CA_CRT.as_bytes()).unwrap();
        assert!(node_ca(directory.path()).is_err());
        let node_key = directory.path().join("ssl/ca/private_ca.key");
        assert!(!node_key.exists());
        write_new(&node_key, CHIA_CA_KEY.as_bytes()).unwrap();
        assert!(node_ca(directory.path()).is_err());
        assert_eq!(std::fs::read(node_key).unwrap(), CHIA_CA_KEY.as_bytes());
    }

    #[test]
    fn reused_tls_requires_matching_key_and_expected_issuer() {
        let directory = tempfile::tempdir().unwrap();
        let (certificate, key) = issuer();
        identity(directory.path(), "identity", certificate, key).unwrap();
        identity(directory.path(), "identity", certificate, key).unwrap();
        assert!(
            identity(
                directory.path(),
                "identity",
                CHIA_CA_CRT.as_bytes(),
                CHIA_CA_KEY.as_bytes()
            )
            .is_err()
        );
        let key_path = directory.path().join("identity.key");
        std::fs::write(&key_path, CHIA_CA_KEY.as_bytes()).unwrap();
        assert!(identity(directory.path(), "identity", certificate, key).is_err());
        assert_eq!(std::fs::read(key_path).unwrap(), CHIA_CA_KEY.as_bytes());
    }

    #[test]
    fn identity_reads_are_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret");
        assert!(read_existing(&path, true, 64).unwrap().is_none());
        write_new(&path, &[b'0'; 65]).unwrap();
        assert_eq!(
            read_existing(&path, true, 64).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_identity_rejects_links_and_public_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret");
        write_new(&path, b"secret").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let linked = directory.path().join("linked");
        symlink(&path, &linked).unwrap();
        assert!(read_existing(&linked, true, 64).is_err());
        std::fs::remove_file(&linked).unwrap();
        std::fs::hard_link(&path, &linked).unwrap();
        assert_eq!(
            read_existing(&path, true, 64).unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
        std::fs::remove_file(&linked).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            read_existing(&path, true, 64).unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"secret");
    }

    #[cfg(unix)]
    #[test]
    fn initialization_rejects_symlinked_service_directories() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("node-cpu")).unwrap();
        assert!(initialize(directory.path()).is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        assert!(!directory.path().join("common/chain.json").exists());
    }
}
