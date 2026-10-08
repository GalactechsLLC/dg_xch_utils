use clap::Parser;
use std::io::Error;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about, after_help = super::config::ENV_HELP)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
}

pub async fn run(arguments: &[std::ffi::OsString]) -> Result<(), Error> {
    let args = Args::parse_from(
        std::iter::once(std::ffi::OsString::from("dgx pool")).chain(arguments.iter().cloned()),
    );
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse::<log::Level>().ok())
        .unwrap_or(log::Level::Info);
    let _logger = dg_logger::DruidGardenLogger::build()
        .current_level(level)
        .init()
        .map_err(|error| Error::other(format!("{error:?}")))?;
    let config = super::config::load(
        "POOL",
        args.config.as_deref(),
        super::config::Format::Json,
        1024 * 1024,
    )?;
    dg_xch_pool::config::serve(config).await
}
