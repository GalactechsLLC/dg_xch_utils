use clap::Parser;
use std::io::Error;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about, after_help = super::config::ENV_HELP)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    /// Shared PostgreSQL catalog; alternatively use DGX_INTRODUCER_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,
    /// Override the configuration's plain HTTP metrics listener.
    #[arg(long, conflicts_with = "disable_metrics")]
    metrics_listen: Option<std::net::SocketAddr>,
    /// Disable the metrics listener, overriding the configuration.
    #[arg(long)]
    disable_metrics: bool,
}

pub async fn run(arguments: &[std::ffi::OsString]) -> Result<(), Error> {
    let args = Args::parse_from(
        std::iter::once(std::ffi::OsString::from("dgx introducer"))
            .chain(arguments.iter().cloned()),
    );
    let mut config: dg_xch_introducer::Config = super::config::load(
        "INTRODUCER",
        args.config.as_deref(),
        super::config::Format::Json,
        1024 * 1024,
    )?;
    if let Some(url) = args.database_url {
        config.database_url = Some(url);
    } else if let Ok(url) = std::env::var("DGX_INTRODUCER_DATABASE_URL") {
        config.database_url = Some(url);
    }
    if args.disable_metrics {
        config.metrics_listen = None;
    } else if let Some(listen) = args.metrics_listen {
        config.metrics_listen = Some(listen);
    }
    dg_xch_introducer::serve(config).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_flags_parse_and_conflicting_overrides_fail() {
        let args = Args::try_parse_from([
            "introducer",
            "--config",
            "config.json",
            "--metrics-listen",
            "0.0.0.0:80",
        ])
        .unwrap();
        assert_eq!(args.metrics_listen, Some("0.0.0.0:80".parse().unwrap()));
        assert!(!args.disable_metrics);
        let args = Args::try_parse_from(["introducer", "--disable-metrics"]).unwrap();
        assert!(args.disable_metrics);
        assert!(
            Args::try_parse_from([
                "introducer",
                "--disable-metrics",
                "--metrics-listen",
                "0.0.0.0:80"
            ])
            .is_err()
        );
    }
}
