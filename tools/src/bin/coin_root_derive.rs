//! `coin-root-derive` — derive v1 coin-set roots from a synced store, read-only.
//!
//! ```text
//! coin-root-derive derive --db-url postgres://… --height 100180 --height 200008
//! coin-root-derive find-boundaries --db-url postgres://… --every 100000 --max 1500000
//! ```
//!
//! `derive` emits one JSON document on stdout:
//! `{backend, tool_commit, elapsed_ms, roots: [{height, header_hash, coin_count, spent_count,
//! mmr_root, spent_bitmap_root, root_v1}]}` — progress and diagnostics go to stderr.

use clap::{Parser, Subcommand};
use dg_xch_dev_tools::coin_roots::derive::{StoreUrl, derive, find_ses_boundaries};
use serde::Serialize;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "coin-root-derive",
    about = "Derive v1 coin-set commitment roots from a synced dg_xch store (read-only)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Stream the coin set once and emit the v1 root at each boundary height.
    Derive {
        /// Store locator: postgres://…, sqlite:///path/to.db, or mmap:///dir (a COPY when the
        /// source directory belongs to a live node).
        #[arg(long)]
        db_url: String,
        /// Boundary height (repeatable, any order; deduplicated).
        #[arg(long = "height", required = true)]
        heights: Vec<u32>,
    },
    /// Print main-chain SES-carrying boundary heights: for each multiple of --every up to
    /// --max, the greatest SES block at or below it (SQL backends only).
    FindBoundaries {
        #[arg(long)]
        db_url: String,
        #[arg(long)]
        every: u32,
        #[arg(long)]
        max: u32,
    },
}

#[derive(Serialize)]
struct DeriveOutput {
    backend: &'static str,
    tool_commit: &'static str,
    elapsed_ms: u128,
    roots: Vec<dg_xch_dev_tools::coin_roots::RootV1>,
}

const TOOL_COMMIT: &str = match option_env!("GIT_COMMIT") {
    Some(c) => c,
    None => "unknown",
};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().cmd {
        Cmd::Derive { db_url, heights } => {
            let url = StoreUrl::parse(&db_url)?;
            let started = Instant::now();
            let roots = derive(&url, &heights).await?;
            let out = DeriveOutput {
                backend: url.backend(),
                tool_commit: TOOL_COMMIT,
                elapsed_ms: started.elapsed().as_millis(),
                roots,
            };
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Cmd::FindBoundaries { db_url, every, max } => {
            let url = StoreUrl::parse(&db_url)?;
            let heights = find_ses_boundaries(&url, every, max).await?;
            println!("{}", serde_json::to_string(&heights)?);
        }
    }
    Ok(())
}
