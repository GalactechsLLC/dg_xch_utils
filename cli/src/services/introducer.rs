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
        std::iter::once(std::ffi::OsString::from("dgx introducer"))
            .chain(arguments.iter().cloned()),
    );
    let config = super::config::load(
        "INTRODUCER",
        args.config.as_deref(),
        super::config::Format::Json,
        1024 * 1024,
    )?;
    dg_xch_introducer::serve(config).await
}
