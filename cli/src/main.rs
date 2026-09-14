// The `dg full-node` process shares the allocator expected by full-node memory metrics.
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> Result<(), std::io::Error> {
    run_with_runtime(dg_xch_cli_lib::run_cli(), std::time::Duration::from_secs(5))
}

fn run_with_runtime(
    operation: impl std::future::Future<Output = Result<(), std::io::Error>>,
    shutdown_timeout: std::time::Duration,
) -> Result<(), std::io::Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(operation);
    runtime.shutdown_timeout(shutdown_timeout);
    result
}

#[cfg(test)]
#[path = "../tests/unit/runtime.rs"]
mod tests;
