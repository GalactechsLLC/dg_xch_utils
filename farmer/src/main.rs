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
use clap::Parser;
use dg_xch_farmer::FarmerService;
use dg_xch_farmer::farmer::config::Config;
use std::io::Error;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(long)]
    config: PathBuf,
}

fn main() -> Result<(), Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_secs(5));
    result
}

async fn run() -> Result<(), Error> {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse::<log::Level>().ok())
        .unwrap_or(log::Level::Info);
    let _logger = dg_logger::DruidGardenLogger::build()
        .use_colors(false)
        .current_level(level)
        .init()
        .map_err(|error| Error::other(format!("{error:?}")))?;
    let args = Args::parse();
    let mut service = FarmerService::start(Config::try_from(args.config.as_path())?).await?;
    let shutdown = dg_xch_servers::transport::shutdown_signal();
    tokio::pin!(shutdown);
    let mut health = tokio::time::interval(Duration::from_secs(1));
    let mut diagnostics = tokio::time::interval(Duration::from_secs(30));
    let result = loop {
        tokio::select! {
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
