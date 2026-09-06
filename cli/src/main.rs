// The `dg full-node` process shares the allocator expected by full-node memory metrics.
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    dg_xch_cli_lib::run_cli().await
}
