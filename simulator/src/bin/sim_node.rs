//! A simulated full node a wallet dials. It serves the peer/wallet protocol from its own chain and
//! the simulator RPC (`farm_block` / `set_auto_farming` / `get_auto_farming`), so a test harness
//! farms coins to any address on demand.
//!
//! ```text
//! sim_node --listen 0.0.0.0:8444 --rpc 127.0.0.1:8555 --network mainnet \
//!          --db sim-node.sqlite --plots-dir sim-plots --interval-secs 2
//! ```
//!
//! Coins are minted by calling the `farm_block` RPC with a target address; there is no startup
//! reward address. The chain carries mainnet's genesis challenge, so a mainnet wallet syncs and
//! spends against it unchanged.

use dg_xch_simulator_lib::pos2::PlotSet;
use dg_xch_simulator_lib::server::{SimulatorServer, simulator_constants};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Simulator defaults: peer 58444, RPC 8555, handshake network `simulator0` over the
    // mainnet genesis challenge.
    let mut listen = "0.0.0.0:58444".to_string();
    let mut rpc = "127.0.0.1:8555".to_string();
    let mut network = "simulator0".to_string();
    let mut db = PathBuf::from("sim-node.sqlite");
    let mut plots_dir = PathBuf::from("sim-plots");
    let mut interval = 2u64;
    let mut plot_count = 12u32;
    let mut auto_farm = false;
    let mut reset = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut next = || {
            args.next()
                .ok_or_else(|| format!("missing value for {arg}"))
        };
        match arg.as_str() {
            "--listen" => listen = next()?,
            "--rpc" => rpc = next()?,
            "--network" => network = next()?,
            "--db" => db = PathBuf::from(next()?),
            "--plots-dir" => plots_dir = PathBuf::from(next()?),
            "--interval-secs" => interval = next()?.parse()?,
            "--plot-count" => plot_count = next()?.parse()?,
            "--auto-farm" => auto_farm = true,
            // Start from a clean chain, KEEPING the plots (a fresh genesis, not a re-plot).
            "--reset" => reset = true,
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    // Reset drops only the chain database — the pos2 plots in --plots-dir are always reused.
    if reset {
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", db.display()));
        }
        println!(
            "reset: cleared the chain db, keeping plots in {}",
            plots_dir.display()
        );
    }

    let constants = simulator_constants();
    println!(
        "generating {plot_count} plots in {}...",
        plots_dir.display()
    );
    let plots = PlotSet::setup(&plots_dir, 1, plot_count, 18, 2, false)?;

    let server = SimulatorServer::start(
        &db,
        &listen,
        &rpc,
        &network,
        constants,
        plots,
        Duration::from_secs(interval),
    )
    .await?;
    if auto_farm {
        server.set_auto_farming(true);
    }
    println!(
        "simulator node up: peer {listen}, rpc {rpc} (network {network}); \
         mint coins via curl -k https://{rpc}/farm_block -d '{{\"address\": \"xch1...\"}}'"
    );

    tokio::signal::ctrl_c().await?;
    server.stop().await;
    println!("stopped");
    Ok(())
}
