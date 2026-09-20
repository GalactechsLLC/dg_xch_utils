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

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    let service = FarmerService::start(Config::try_from(args.config.as_path())?).await?;
    dg_xch_servers::transport::shutdown_signal().await?;
    service.stop().await;
    Ok(())
}
