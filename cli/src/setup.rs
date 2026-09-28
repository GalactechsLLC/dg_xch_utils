use clap::Args;
use dg_xch_servers::app_config::{AppConfig, default_paths};
use dg_xch_servers::chain_config::write_new;
use dialoguer::Input;
use std::ffi::OsString;
use std::io::{Error, ErrorKind, IsTerminal};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct InitArgs {
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long)]
    pub plots_dir: Option<PathBuf>,
    #[arg(long, help = "Use defaults without prompts; suitable for automation")]
    pub non_interactive: bool,
}

#[derive(Args, Debug)]
pub struct ServiceArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub arguments: Vec<OsString>,
}

fn choose_path(label: &str, default: &Path, interactive: bool) -> Result<PathBuf, Error> {
    let path = if interactive {
        PathBuf::from(
            Input::<String>::new()
                .with_prompt(label)
                .default(default.display().to_string())
                .interact_text()
                .map_err(Error::other)?,
        )
    } else {
        default.to_path_buf()
    };
    if path.as_os_str().is_empty() || path.starts_with("~") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "use a full path, not an empty path or literal ~",
        ));
    }
    std::path::absolute(path)
}

pub fn initialize(args: &InitArgs, root: &Path, explicit_root: bool) -> Result<(), Error> {
    let interactive = !args.non_interactive;
    if interactive && !std::io::stdin().is_terminal() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "interactive setup needs a terminal; use --non-interactive for automation",
        ));
    }
    let root = choose_path(
        "Configuration directory",
        root,
        interactive && !explicit_root,
    )?;
    if root.join("dgx.json").try_exists()? {
        let config = AppConfig::load(&root)?;
        if args
            .data_dir
            .as_ref()
            .map(std::path::absolute)
            .transpose()?
            .is_some_and(|path| path != config.data_dir)
            || args
                .plots_dir
                .as_ref()
                .map(std::path::absolute)
                .transpose()?
                .is_some_and(|path| !config.plot_directories.contains(&path))
        {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "already initialized with different paths; use a separate --config-dir",
            ));
        }
        println!(
            "Already initialized at {}. Existing settings and keys were retained.",
            root.display()
        );
        return Ok(());
    }
    let default_data = default_paths()?.1;
    let data = choose_path(
        "Node and wallet data directory",
        args.data_dir.as_deref().unwrap_or(&default_data),
        interactive && args.data_dir.is_none(),
    )?;
    let default_plots = data.join("plots");
    let plots = choose_path(
        "Plot directory (choose a disk with enough free space)",
        args.plots_dir.as_deref().unwrap_or(&default_plots),
        interactive && args.plots_dir.is_none(),
    )?;
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&plots)?;
    let ssl = root.join("ssl");
    println!("Preparing local TLS credentials; initial key generation may take a minute...");
    dg_xch_core::ssl::create_all_ssl(&ssl, false)?;
    let desktop = root.join("desktop.json");
    if !desktop.try_exists()? {
        let settings = serde_json::json!({
            "version": 1, "network": "mainnet", "node_host": "localhost", "node_port": 8444,
            "genesis_header_hash": dg_xch_core::consensus::constants::ChiaNetwork::Mainnet.genesis_header_hash().map(hex::encode),
            "certificate": ssl.join("full_node/private_full_node.crt"),
            "private_key": ssl.join("full_node/private_full_node.key"),
            "certificate_authority": ssl.join("ca/private_ca.crt"),
            "farmer_ssl_root": ssl, "plot_directories": [&plots]
        });
        write_new(
            &desktop,
            &serde_json::to_vec_pretty(&settings).map_err(Error::other)?,
        )?;
    }
    AppConfig {
        version: 1,
        data_dir: data,
        plot_directories: vec![plots],
    }
    .save_new(&root)?;
    println!("Initialized Chia mainnet at {}.", root.display());
    #[cfg(feature = "full-node")]
    println!(
        "Start the node: dgx --config-dir '{}' full-node",
        root.display()
    );
    #[cfg(not(feature = "full-node"))]
    println!(
        "This launcher has no embedded full node. Configure an existing node in desktop Settings."
    );
    println!(
        "Open the desktop: dgx --config-dir '{}' gui\nPlotting can run while the node syncs. Back up wallet recovery phrases separately.",
        root.display()
    );
    Ok(())
}

pub async fn launch(binary: &str, args: &ServiceArgs, root: &Path) -> Result<(), Error> {
    AppConfig::load(root)?;
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| Error::other("cannot find executable directory"))?;
    let child = directory.join(format!("{binary}{}", std::env::consts::EXE_SUFFIX));
    if !child.is_file() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!(
                "{} is not installed beside dgx; install the corresponding package into the same bin directory",
                child.display()
            ),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(std::process::Command::new(child)
            .args(&args.arguments)
            .env("DGX_CONFIG_DIR", root)
            .exec())
    }
    #[cfg(not(unix))]
    {
        let status = tokio::process::Command::new(child)
            .args(&args.arguments)
            .env("DGX_CONFIG_DIR", root)
            .kill_on_drop(true)
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::other(format!("{binary} exited with {status}")))
        }
    }
}
