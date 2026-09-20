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
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::ssl::{generate_ca_signed_cert_data, make_ca_cert_data};
use dg_xch_farmer::farmer::config::{
    Config, DruidGardenHarvesterConfig, FarmingInfo, HarvesterConfig,
};
use dg_xch_keys::{
    encode_puzzle_hash, master_sk_to_farmer_sk, master_sk_to_pool_sk, master_sk_to_wallet_sk,
};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use std::io::{Error, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    about = "Initialize disposable Docker test-stack identities; never use these keys for real funds"
)]
struct Args {
    #[arg(long)]
    root: PathBuf,
}

fn write_new(path: &Path, data: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::other("missing output directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}

fn identity(root: &Path, prefix: &str, issuer: &[u8], issuer_key: &[u8]) -> Result<(), Error> {
    let certificate = root.join(format!("{prefix}.crt"));
    let key = root.join(format!("{prefix}.key"));
    if certificate.exists() && key.exists() {
        return Ok(());
    }
    if certificate.exists() || key.exists() {
        return Err(Error::other(format!(
            "partial TLS identity at {}",
            root.display()
        )));
    }
    let (certificate_bytes, key_bytes) = generate_ca_signed_cert_data(issuer, issuer_key)?;
    write_new(&certificate, &certificate_bytes)?;
    write_new(&key, &key_bytes)
}

fn node_ca(root: &Path) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let certificate = root.join("ssl/ca/private_ca.crt");
    let key = root.join("ssl/ca/private_ca.key");
    if certificate.exists() && key.exists() {
        return Ok((std::fs::read(certificate)?, std::fs::read(key)?));
    }
    if certificate.exists() || key.exists() {
        return Err(Error::other(
            "partial node CA; repair the test volume before restarting",
        ));
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
) -> Result<(), Error> {
    if root.join("config.yaml").exists() {
        return dg_xch_farmer::utils::ensure_farmer_tls(&root.join("ssl"));
    }
    if !root.join("ssl/ca/private_ca.crt").exists() {
        write_new(&root.join("ssl/ca/private_ca.crt"), issuer)?;
    }
    if !root.join("ssl/ca/chia_ca.crt").exists() {
        write_new(&root.join("ssl/ca/chia_ca.crt"), CHIA_CA_CRT.as_bytes())?;
    }
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
    let master_path = root.join("dev-master-key.hex");
    let master = if master_path.exists() {
        SecretKey::from_bytes(
            &hex::decode(std::fs::read_to_string(&master_path)?).map_err(Error::other)?,
        )
        .map_err(|_| Error::other("invalid development master key"))?
    } else {
        let mut entropy: [u8; 32] = rand::random();
        let master = SecretKey::key_gen_v3(&entropy, &[])
            .map_err(|_| Error::other("development key generation failed"))?;
        entropy.fill(0);
        write_new(&master_path, hex::encode(master.to_bytes()).as_bytes())?;
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
        farmer_info: vec![FarmingInfo {
            farmer_secret_key: Bytes32::from(farmer.to_bytes()),
            pool_secret_key: Some(Bytes32::from(pool.to_bytes())),
            ..FarmingInfo::default()
        }],
        pool_info: Vec::new(),
        payout_address: encode_puzzle_hash(&payout, "dgx")?,
        harvester_configs: HarvesterConfig {
            druid_garden: Some(DruidGardenHarvesterConfig {
                plot_directories: vec!["/plots".to_owned()],
            }),
            custom_config: None,
        },
        metrics: None,
    };
    config.validate_keys()?;
    write_new(
        &root.join("config.yaml"),
        serde_yaml::to_string(&config)
            .map_err(Error::other)?
            .as_bytes(),
    )?;
    write_new(&root.join("plot-keys.json"), &serde_json::to_vec_pretty(&serde_json::json!({ "farmer_public_key": hex::encode(farmer.sk_to_pk().to_bytes()), "pool_public_key": hex::encode(pool.sk_to_pk().to_bytes()), "payout_address": config.payout_address })).map_err(Error::other)?)
}

fn service_identity(root: &Path, roots: &[u8]) -> Result<serde_json::Value, Error> {
    identity(
        &root.join("tls"),
        "identity",
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
    )?;
    if !root.join("tls/roots.crt").exists() {
        write_new(&root.join("tls/roots.crt"), roots)?;
    }
    Ok(serde_json::json!({
        "certificate": "/service/tls/identity.crt",
        "private_key": "/service/tls/identity.key",
        "ca_certificate": "/service/tls/roots.crt"
    }))
}

fn main() -> Result<(), Error> {
    let root = Args::parse().root;
    let chain_path = root.join("common/chain.json");
    let chain: ChainDefinition = if chain_path.exists() {
        serde_json::from_slice(&std::fs::read(&chain_path)?).map_err(Error::other)?
    } else {
        let chain = ChainDefinition {
            network_id: "dgx-dev".to_owned(),
            genesis_seed: format!("dg_xch/compose/{}", hex::encode(rand::random::<[u8; 32]>())),
            ..ChainDefinition::default()
        };
        write_new(
            &chain_path,
            &serde_json::to_vec_pretty(&chain).map_err(Error::other)?,
        )?;
        chain
    };
    chain.constants().map_err(Error::other)?;
    let mut roots = Vec::new();
    for (node, farmer) in [
        ("node-cpu", "farmer-cpu"),
        ("node-nvidia", "farmer-nvidia"),
        ("node-amd", "farmer-amd"),
    ] {
        let (certificate, key) = node_ca(&root.join(node))?;
        create_farmer(&root.join(farmer), &chain, &certificate, &key)?;
        roots.extend_from_slice(&certificate);
    }
    let introducer = root.join("introducer");
    if !introducer.join("config.json").exists() {
        let tls = service_identity(&introducer, &roots)?;
        let config = serde_json::json!({ "listen": "0.0.0.0:8445", "chain": chain, "tls": tls, "peer_server_name": "localhost", "allow_private_addresses": true, "max_connections": 32, "max_peers": 128, "peer_ttl_seconds": 3600 });
        write_new(
            &introducer.join("config.json"),
            &serde_json::to_vec_pretty(&config).map_err(Error::other)?,
        )?;
    }
    let timelord = root.join("timelord");
    if !timelord.join("config.json").exists() {
        let tls = service_identity(
            &timelord,
            &std::fs::read(root.join("node-cpu/ssl/ca/private_ca.crt"))?,
        )?;
        let config = serde_json::json!({ "chain": chain, "fullnode_host": "localhost", "fullnode_port": 8444, "server_name": "localhost", "tls": tls, "max_iterations": 1048576, "job_timeout_seconds": 300, "reconnect_seconds": 5 });
        write_new(
            &timelord.join("config.json"),
            &serde_json::to_vec_pretty(&config).map_err(Error::other)?,
        )?;
    }
    eprintln!(
        "Disposable test-stack identities are ready; no production funds, regular timelord scheduler, or PoS2 network-farming support is implied"
    );
    Ok(())
}
