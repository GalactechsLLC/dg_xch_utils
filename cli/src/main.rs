// The `dgx full-node` process shares the allocator expected by full-node memory metrics.
#[cfg(all(unix, feature = "full-node"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> Result<(), std::io::Error> {
    use dg_xch_cli_lib::cli::RootCommands;
    let cli = dg_xch_cli_lib::parse_cli()?;
    if let RootCommands::Gui(args) = &cli.action {
        if cli.network.is_some() {
            return Err(std::io::Error::other(
                "configure the desktop network in Settings",
            ));
        }
        let root = dg_xch_servers::app_config::config_dir(cli.config_dir.as_deref())?;
        if args.arguments.as_slice() != ["--smoke-test"] {
            dg_xch_servers::app_config::AppConfig::load(&root)?;
        }
        #[cfg(feature = "desktop")]
        return dg_xch_gui::runner::run(&args.arguments, &root)
            .map_err(|error| std::io::Error::other(error.to_string()));
        #[cfg(not(feature = "desktop"))]
        return Err(std::io::Error::other(
            "this build does not include the desktop feature",
        ));
    }
    let native_worker = matches!(&cli.action, RootCommands::Timelord(args)
        if args.arguments.first().and_then(|value| value.to_str()).is_some_and(|mode|
            matches!(mode, "worker" | "regular-worker" | "regular-stream-worker")));
    if native_worker {
        run_on_runtime(
            dg_xch_cli_lib::run_cli_with(cli),
            std::time::Duration::from_secs(5),
            tokio::runtime::Builder::new_current_thread(),
        )
    } else {
        run_with_runtime(
            dg_xch_cli_lib::run_cli_with(cli),
            std::time::Duration::from_secs(5),
        )
    }
}

fn run_with_runtime(
    operation: impl std::future::Future<Output = Result<(), std::io::Error>>,
    shutdown_timeout: std::time::Duration,
) -> Result<(), std::io::Error> {
    run_on_runtime(
        operation,
        shutdown_timeout,
        tokio::runtime::Builder::new_multi_thread(),
    )
}

fn run_on_runtime(
    operation: impl std::future::Future<Output = Result<(), std::io::Error>>,
    shutdown_timeout: std::time::Duration,
    mut builder: tokio::runtime::Builder,
) -> Result<(), std::io::Error> {
    let runtime = builder.enable_all().build()?;
    let result = runtime.block_on(operation);
    runtime.shutdown_timeout(shutdown_timeout);
    result
}

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
