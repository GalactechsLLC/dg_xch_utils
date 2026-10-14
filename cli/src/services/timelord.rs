use clap::{Parser, Subcommand};
use dg_xch_timelord::worker::{
    ProofRequest, StreamingRegularWorker, WORKER_MESSAGE_LIMIT, prove, prove_regular, run_isolated,
};
use std::io::{Error, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about, after_help = super::config::ENV_HELP)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(after_help = super::config::ENV_HELP)]
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    #[command(after_help = super::config::ENV_HELP)]
    Compact {
        #[arg(long)]
        config: Option<PathBuf>,
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
    #[command(hide = true)]
    RegularStreamWorker,
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
            let config = super::config::load(
                "TIMELORD",
                config.as_deref(),
                super::config::Format::Json,
                WORKER_MESSAGE_LIMIT as usize,
            )?;
            dg_xch_timelord::regular::serve(config).await
        }
        Command::Compact { config } => {
            let config = super::config::load(
                "TIMELORD",
                config.as_deref(),
                super::config::Format::Json,
                WORKER_MESSAGE_LIMIT as usize,
            )?;
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
        Command::RegularStreamWorker => {
            let mut prover = StreamingRegularWorker::default();
            let mut input = std::io::stdin().lock();
            let mut output = std::io::stdout().lock();
            loop {
                let mut header = [0; 4];
                // EOF between requests is a normal shutdown; truncated frames are errors.
                if input.read(&mut header[..1])? == 0 {
                    return Ok(());
                }
                input.read_exact(&mut header[1..])?;
                let length = u32::from_be_bytes(header) as usize;
                if length == 0 || length as u64 > WORKER_MESSAGE_LIMIT {
                    return Err(Error::other("timelord input exceeds 16 KiB"));
                }
                let mut bytes = vec![0; length];
                input.read_exact(&mut bytes)?;
                let request: dg_xch_timelord::worker::StreamingRegularProofRequest =
                    serde_json::from_slice(&bytes).map_err(Error::other)?;
                let result = prover.prove_with_threads(&request.work, request.witness_threads)?;
                let bytes = serde_json::to_vec(&result).map_err(Error::other)?;
                if bytes.len() as u64 > WORKER_MESSAGE_LIMIT {
                    return Err(Error::other("timelord output exceeds 16 KiB"));
                }
                output.write_all(&(bytes.len() as u32).to_be_bytes())?;
                output.write_all(&bytes)?;
                output.flush()?;
            }
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
