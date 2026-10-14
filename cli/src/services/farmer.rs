use clap::{Parser, Subcommand};
use dg_xch_farmer::FarmerService;
use dg_xch_farmer::farmer::config::Config;
use std::io::Error;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about, after_help = super::config::ENV_HELP, args_conflicts_with_subcommands = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, env = "DGX_FARMER_NODE_HOST")]
    node_host: Option<String>,
    #[arg(long, env = "DGX_FARMER_NODE_PORT", value_parser = clap::value_parser!(u16).range(1..))]
    node_port: Option<u16>,
    /// Reconnect after this many seconds without a signage point.
    #[arg(long, env = "DGX_FARMER_SIGNAGE_POINT_TIMEOUT_SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    signage_point_timeout_seconds: Option<u64>,
    /// Override PoS2 recovery memory without changing farmer credentials.
    #[arg(long, env = "DGX_FARMER_POS2_MEMORY_MIB", value_parser = clap::value_parser!(u64).range(1..))]
    pos2_memory_mib: Option<u64>,
    #[arg(long, env = "DGX_FARMER_POS2_DEADLINE_MS", value_parser = clap::value_parser!(u64).range(1..=120_000))]
    pos2_deadline_ms: Option<u64>,
}

#[derive(Subcommand)]
enum Command {
    /// Generate stable farmer credentials and public plotting keys for a custom chain.
    Init(super::farmer_setup::InitArgs),
}

pub async fn run(arguments: &[std::ffi::OsString]) -> Result<(), Error> {
    let args = Args::parse_from(
        std::iter::once(std::ffi::OsString::from("dgx farmer")).chain(arguments.iter().cloned()),
    );
    if let Some(Command::Init(args)) = &args.command {
        return super::farmer_setup::initialize(args);
    }
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse::<log::Level>().ok())
        .unwrap_or(log::Level::Info);
    let _logger = dg_logger::DruidGardenLogger::build()
        .use_colors(false)
        .current_level(level)
        .init()
        .map_err(|error| Error::other(format!("{error:?}")))?;
    let mut config = super::config::load::<Config>(
        "FARMER",
        args.config.as_deref(),
        super::config::Format::Yaml,
        1024 * 1024,
    )?;
    if let Some(timeout) = args.signage_point_timeout_seconds {
        config.signage_point_timeout_seconds = timeout;
    }
    if let Some(host) = args.node_host {
        config.fullnode_ws_host = host.clone();
        config.fullnode_rpc_host = host;
    }
    if let Some(port) = args.node_port {
        config.fullnode_ws_port = port;
        config.fullnode_rpc_port = port;
    }
    if args.pos2_memory_mib.is_some() || args.pos2_deadline_ms.is_some() {
        let pos2 = config.harvester_configs.pos2.as_mut().ok_or_else(|| {
            Error::other("PoS2 resource overrides require a configured PoS2 harvester")
        })?;
        if let Some(memory) = args.pos2_memory_mib {
            pos2.memory_mib = memory;
        }
        if let Some(deadline) = args.pos2_deadline_ms {
            pos2.deadline_ms = deadline;
        }
        pos2.validate()?;
    }
    let mut service = FarmerService::start(config).await?;
    let shutdown = dg_xch_servers::transport::shutdown_signal();
    tokio::pin!(shutdown);
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let mut diagnostics = tokio::time::interval(Duration::from_secs(30));
    let result = loop {
        tokio::select! {
            biased;
            result = &mut shutdown => break result,
            _ = health.tick() => {
                if let Some(failure) = service.failure() {
                    log::error!("{failure}");
                    break Err(Error::other(failure));
                }
            }
            _ = diagnostics.tick() => {
                log::info!("PoS2 farmer status: {}", serde_json::to_string(&service.pos2_status()).map_err(Error::other)?);
            }
        }
    };
    service.stop().await;
    result
}
