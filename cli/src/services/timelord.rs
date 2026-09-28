use clap::{Parser, Subcommand};
use dg_xch_timelord::worker::{
    ProofRequest, WORKER_MESSAGE_LIMIT, prove, prove_regular, run_isolated,
};
use std::io::{Error, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    Compact {
        #[arg(long)]
        config: PathBuf,
    },
    Prove {
        #[arg(long)]
        request: PathBuf,
        #[arg(long, default_value_t = 300)]
        timeout_seconds: u64,
    },
    #[command(hide = true)]
    Worker,
    #[command(hide = true)]
    RegularWorker,
}

fn read_limited(input: impl Read) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    input
        .take(WORKER_MESSAGE_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > WORKER_MESSAGE_LIMIT {
        return Err(Error::other("timelord input exceeds 16 KiB"));
    }
    Ok(bytes)
}

pub async fn run(arguments: &[std::ffi::OsString]) -> Result<(), Error> {
    match Args::parse_from(
        std::iter::once(std::ffi::OsString::from("dgx timelord")).chain(arguments.iter().cloned()),
    )
    .command
    {
        Command::Run { config } => {
            let config = serde_json::from_slice(&read_limited(std::fs::File::open(config)?)?)
                .map_err(Error::other)?;
            dg_xch_timelord::regular::serve(config).await
        }
        Command::Compact { config } => {
            let config = serde_json::from_slice(&read_limited(std::fs::File::open(config)?)?)
                .map_err(Error::other)?;
            dg_xch_timelord::service::serve(config).await
        }
        Command::Prove {
            request,
            timeout_seconds,
        } => {
            let request: ProofRequest =
                serde_json::from_slice(&read_limited(std::fs::File::open(request)?)?)
                    .map_err(Error::other)?;
            let result = run_isolated(request, Duration::from_secs(timeout_seconds)).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(Error::other)?
            );
            Ok(())
        }
        Command::Worker => {
            let request = serde_json::from_slice(&read_limited(std::io::stdin().lock())?)
                .map_err(Error::other)?;
            let result = prove(&request)?;
            std::io::stdout()
                .lock()
                .write_all(&serde_json::to_vec(&result).map_err(Error::other)?)
        }
        Command::RegularWorker => {
            let request = serde_json::from_slice(&read_limited(std::io::stdin().lock())?)
                .map_err(Error::other)?;
            let result = prove_regular(&request)?;
            std::io::stdout()
                .lock()
                .write_all(&serde_json::to_vec(&result).map_err(Error::other)?)
        }
    }
}
