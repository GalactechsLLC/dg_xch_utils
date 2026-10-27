use clap::Args;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_farmer::farmer::config::{Config, FarmingInfo, HarvesterConfig, Pos2HarvesterConfig};
use dg_xch_keys::{
    encode_puzzle_hash, master_sk_to_farmer_sk, master_sk_to_pool_sk,
    master_sk_to_wallet_sk_unhardened,
};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use dg_xch_servers::chain_config::{read_selection, write_new};
use std::io::{Error, ErrorKind};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub(super) struct InitArgs {
    #[arg(long)]
    chain_config: PathBuf,
    #[arg(long)]
    output_dir: PathBuf,
    #[arg(long)]
    fullnode_host: String,
    #[arg(long, default_value_t = 58444, value_parser = clap::value_parser!(u16).range(1..))]
    fullnode_port: u16,
    #[arg(long, default_value = "/runtime/ssl")]
    ssl_root: PathBuf,
    #[arg(long, default_value = "/plots")]
    plots_dir: PathBuf,
    /// Import an existing owner-only Falcon keyring instead of generating keys.
    #[arg(long)]
    import_falcon_keys: Option<PathBuf>,
    /// Runtime path for the generated or imported Falcon keyring.
    #[arg(long, default_value = "/runtime/falcon-keys.bin")]
    falcon_key_file: PathBuf,
}

pub(super) fn initialize(args: &InitArgs) -> Result<(), Error> {
    let selection = read_selection(&args.chain_config)?;
    let chain = selection.resolve().map_err(Error::other)?;
    let definition = match selection {
        dg_xch_core::consensus::chain_definition::ChainSelection::Custom(definition) => definition,
        _ => {
            return Err(Error::other(
                "farmer init requires a custom chain definition",
            ));
        }
    };
    if args.fullnode_host.is_empty() || args.fullnode_host.contains(['/', '\n', '\r']) {
        return Err(Error::other("supply a full-node DNS hostname"));
    }
    if chain.constants.signature_profile.is_bls() && args.import_falcon_keys.is_some() {
        return Err(Error::other("Falcon keys cannot initialize a BLS farmer"));
    }
    if !chain.constants.signature_profile.is_bls() && !cfg!(feature = "falcon") {
        return Err(Error::other(
            "Falcon farmer initialization requires the falcon feature",
        ));
    }
    // Never replace an existing identity, including when setup is run twice.
    std::fs::create_dir_all(
        args.output_dir
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    )?;
    std::fs::create_dir(&args.output_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&args.output_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(feature = "falcon")]
    if !chain.constants.signature_profile.is_bls() {
        return initialize_falcon(args, definition, chain.constants);
    }
    let master = blst::min_pk::SecretKey::key_gen_v3(&rand::random::<[u8; 32]>(), &[])
        .map_err(|_| Error::other("could not generate farming key"))?;
    let farmer = master_sk_to_farmer_sk(&master)?;
    let pool = master_sk_to_pool_sk(&master)?;
    let wallet = master_sk_to_wallet_sk_unhardened(&master, 0)?;
    let payout = puzzle_hash_for_pk(Bytes48::from(wallet.sk_to_pk().to_bytes()))?;
    let config: Config = Config {
        selected_network: chain.network_id,
        chain_definition: Some(definition),
        ssl_root_path: Some(args.ssl_root.to_string_lossy().into_owned()),
        fullnode_ws_host: args.fullnode_host.clone(),
        fullnode_rpc_host: args.fullnode_host.clone(),
        fullnode_ws_port: args.fullnode_port,
        fullnode_rpc_port: args.fullnode_port,
        falcon_key_file: None,
        farmer_info: vec![FarmingInfo {
            farmer_secret_key: Bytes32::from(farmer.to_bytes()),
            pool_secret_key: Some(Bytes32::from(pool.to_bytes())),
            ..Default::default()
        }],
        payout_address: encode_puzzle_hash(&payout, chain.constants.bech32_prefix)?,
        harvester_configs: HarvesterConfig {
            plot_directories: vec![args.plots_dir.to_string_lossy().into_owned()],
            druid_garden: None,
            pos2: Some(Pos2HarvesterConfig {
                memory_mib: 512,
                max_entries: 4_194_304,
                max_work: 10_000_000_000,
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    config.validate_keys()?;
    if !config.is_ready() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid generated farmer configuration",
        ));
    }
    write_new(
        &args.output_dir.join("master-key.hex"),
        hex::encode(master.to_bytes()).as_bytes(),
    )?;
    write_new(
        &args.output_dir.join("farmer.yaml"),
        serde_yaml::to_string(&config)
            .map_err(Error::other)?
            .as_bytes(),
    )?;
    write_new(&args.output_dir.join("plotter.env"), format!(
        "DGX_PLOTTER_FARMER_KEY={}\nDGX_PLOTTER_POOL_KEY={}\nDGX_PLOTTER_TESTNET={}\nDGX_PLOTTER_K={}\nDGX_PLOTTER_STRENGTH={}\n",
        hex::encode(farmer.sk_to_pk().to_bytes()), hex::encode(pool.sk_to_pk().to_bytes()),
        chain.constants.is_testnet, chain.constants.plot_size_v2, chain.constants.min_plot_strength,
    ).as_bytes())?;
    eprintln!(
        "Farmer credentials created in {}. Import these files into a Kubernetes Secret; retain the master key to control payouts.",
        args.output_dir.display()
    );
    Ok(())
}

#[cfg(feature = "falcon")]
fn initialize_falcon(
    args: &InitArgs,
    definition: dg_xch_core::consensus::chain_definition::ChainDefinition,
    constants: dg_xch_core::consensus::constants::ConsensusConstants,
) -> Result<(), Error> {
    use dg_xch_core::signatures::{SignatureSigner, falcon_keys::FalconKeyring, falcon_wallet};
    let keys = match &args.import_falcon_keys {
        Some(path) => FalconKeyring::load(path)?,
        None => FalconKeyring::generate(32)?,
    };
    let payout = falcon_wallet::puzzle(&keys.wallet.public_key()).tree_hash();
    let config: Config = Config {
        selected_network: definition.network_id.clone(),
        chain_definition: Some(definition),
        ssl_root_path: Some(args.ssl_root.to_string_lossy().into_owned()),
        fullnode_ws_host: args.fullnode_host.clone(),
        fullnode_rpc_host: args.fullnode_host.clone(),
        fullnode_ws_port: args.fullnode_port,
        fullnode_rpc_port: args.fullnode_port,
        falcon_key_file: Some(args.falcon_key_file.clone()),
        payout_address: encode_puzzle_hash(&payout, constants.bech32_prefix)?,
        harvester_configs: HarvesterConfig {
            plot_directories: vec![args.plots_dir.to_string_lossy().into_owned()],
            druid_garden: None,
            pos2: Some(Pos2HarvesterConfig::default()),
            ..Default::default()
        },
        ..Default::default()
    };
    config.validate_keys()?;
    if !config.is_ready() {
        return Err(Error::other(
            "invalid generated Falcon farmer configuration",
        ));
    }
    write_new(&args.output_dir.join("falcon-keys.bin"), &keys.encode())?;
    let encoded = serde_yaml::to_string(&config).map_err(Error::other)?;
    write_new(&args.output_dir.join("farmer.yaml"), encoded.as_bytes())?;
    write_new(&args.output_dir.join("plot-keys.json"), &serde_json::to_vec_pretty(&serde_json::json!({"signature_profile":"fn_dsa512_v2", "plot_count":keys.plots.len(), "payout_address":config.payout_address})).map_err(Error::other)?)?;
    eprintln!(
        "Falcon farmer identity saved in {}. Mount falcon-keys.bin at {} and enroll its RPC certificate before starting.",
        args.output_dir.display(),
        args.falcon_key_file.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "falcon")]
    #[test]
    fn imports_existing_falcon_identity_without_generating_bls_keys() {
        use dg_xch_core::signatures::falcon_keys::FalconKeyring;
        let root = tempfile::tempdir().unwrap();
        let chain = root.path().join("chain.json");
        let definition =
            dg_xch_core::consensus::chain_definition::ChainDefinition::falcon_development(
                "CLI Falcon init".into(),
            );
        write_new(&chain, &serde_json::to_vec(&definition).unwrap()).unwrap();
        let source = root.path().join("existing.bin");
        let keys = FalconKeyring::generate(1).unwrap();
        write_new(&source, &keys.encode()).unwrap();
        let args = InitArgs {
            chain_config: chain,
            output_dir: root.path().join("farmer"),
            fullnode_host: "dgx-test.druid.garden".into(),
            fullnode_port: 30444,
            ssl_root: "/runtime/ssl".into(),
            plots_dir: "/plots".into(),
            import_falcon_keys: Some(source.clone()),
            falcon_key_file: "/runtime/falcon-keys.bin".into(),
        };
        initialize(&args).unwrap();
        let config: Config =
            serde_yaml::from_slice(&std::fs::read(args.output_dir.join("farmer.yaml")).unwrap())
                .unwrap();
        assert!(config.farmer_info.is_empty());
        assert!(config.pool_info.is_empty());
        assert_eq!(config.falcon_key_file, Some(args.falcon_key_file.clone()));
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(args.output_dir.join("falcon-keys.bin")).unwrap()
        );
        assert!(!args.output_dir.join("master-key.hex").exists());
        assert!(initialize(&args).is_err());
    }
    #[test]
    fn creates_matching_farmer_and_plotter_credentials_without_rotating_keys() {
        let root = tempfile::tempdir().unwrap();
        let chain = root.path().join("chain.json");
        let definition = dg_xch_core::consensus::chain_definition::ChainDefinition::dg_pos2_test();
        write_new(&chain, &serde_json::to_vec(&definition).unwrap()).unwrap();
        let args = InitArgs {
            chain_config: chain,
            output_dir: root.path().join("credentials"),
            fullnode_host: "node-0.nodes.test.svc.cluster.local".into(),
            fullnode_port: 58444,
            ssl_root: "/runtime/ssl".into(),
            plots_dir: "/plots".into(),
            import_falcon_keys: None,
            falcon_key_file: "/runtime/falcon-keys.bin".into(),
        };
        initialize(&args).unwrap();
        let config: Config =
            serde_yaml::from_slice(&std::fs::read(args.output_dir.join("farmer.yaml")).unwrap())
                .unwrap();
        let master_bytes = std::fs::read(args.output_dir.join("master-key.hex")).unwrap();
        let master =
            blst::min_pk::SecretKey::from_bytes(&hex::decode(&master_bytes).unwrap()).unwrap();
        let farmer = master_sk_to_farmer_sk(&master).unwrap();
        assert_eq!(
            config.farmer_info[0].farmer_secret_key,
            Bytes32::from(farmer.to_bytes())
        );
        assert!(config.payout_address.starts_with("txch"));
        assert_eq!(config.constants().unwrap(), definition.constants().unwrap());
        assert_eq!(config.fullnode_rpc_host, args.fullnode_host);
        let public = std::fs::read_to_string(args.output_dir.join("plotter.env")).unwrap();
        assert!(public.contains(&hex::encode(farmer.sk_to_pk().to_bytes())));
        assert!(public.contains("DGX_PLOTTER_TESTNET=true"));
        assert!(initialize(&args).is_err());
        assert_eq!(
            std::fs::read(args.output_dir.join("master-key.hex")).unwrap(),
            master_bytes
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for file in ["master-key.hex", "farmer.yaml"] {
                assert_eq!(
                    std::fs::metadata(args.output_dir.join(file))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }
}
