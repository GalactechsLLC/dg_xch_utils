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
use std::io::{Error, Read};
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
    let mut bytes = Vec::new();
    std::fs::File::open(args.config)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::other("introducer configuration exceeds 1 MiB"));
    }
    let config = serde_json::from_slice(&bytes).map_err(Error::other)?;
    dg_xch_introducer::serve(config).await
}
