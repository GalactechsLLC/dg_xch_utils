use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::{ConsensusConstants, MAINNET, TESTNET_11};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::BlockStore;
use dg_xch_weight_proof::serve::WeightProofServer;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = None;
    let mut tip = None;
    let mut out = None;
    let mut network = String::from("mainnet");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut take = || args.next().ok_or(format!("missing value for {a}"));
        match a.as_str() {
            "--db" => db = Some(take()?),
            "--tip" => tip = Some(take()?),
            "--out" => out = Some(PathBuf::from(take()?)),
            "--network" => network = take()?,
            other => return Err(format!("unknown arg {other}").into()),
        }
    }
    let db = db.ok_or("--db required")?;
    let tip = tip.ok_or("--tip required")?;
    let out = out.ok_or("--out required")?;
    let constants = match network.as_str() {
        "mainnet" => MAINNET,
        "testnet11" => TESTNET_11,
        other => return Err(format!("unknown network {other}").into()),
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        match dg_full_node::config::Backend::parse(&db) {
            dg_full_node::config::Backend::Sqlite(path) => {
                let store = Arc::new(dg_xch_stores::SqliteStore::open(&path).await?);
                run(store, constants, &tip, &out).await
            }
            #[cfg(feature = "postgres")]
            dg_full_node::config::Backend::Postgres(url) => {
                let store = Arc::new(dg_xch_stores::PostgresStore::open(&url).await?);
                run(store, constants, &tip, &out).await
            }
            #[cfg(not(feature = "postgres"))]
            dg_full_node::config::Backend::Postgres(url) => Err(format!(
                "postgres:// --db ({url}) requires a binary built with --features postgres"
            )
            .into()),
            #[cfg(feature = "mmap")]
            dg_full_node::config::Backend::Mmap(dir) => {
                let store = Arc::new(dg_xch_stores::MmapStore::open(&dir).await?);
                run(store, constants, &tip, &out).await
            }
            #[cfg(not(feature = "mmap"))]
            dg_full_node::config::Backend::Mmap(dir) => Err(format!(
                "mmap:// --db ({}) requires a binary built with --features mmap",
                dir.display()
            )
            .into()),
        }
    })
}

async fn run<S: BlockStore + Send + Sync + 'static>(
    store: Arc<S>,
    constants: ConsensusConstants,
    tip_arg: &str,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // --tip: a 64-hex header hash, or a height resolved through the store's main chain.
    let tip: Bytes32 = if tip_arg.trim_start_matches("0x").len() == 64 {
        Bytes32::from_str(tip_arg).map_err(|e| format!("parse tip hash: {e}"))?
    } else {
        let height: u32 = tip_arg
            .parse()
            .map_err(|e| format!("parse tip height: {e}"))?;
        store
            .get_block_record_by_height(height)
            .await?
            .ok_or(format!("no block record at height {height}"))?
            .header_hash
    };

    let server = WeightProofServer::new(store, constants);
    let started = Instant::now();
    let wp = server.get_proof_of_weight(tip).await?;
    let bytes = wp.to_bytes(ChiaProtocolVersion::default())?;
    std::fs::write(out, &bytes)?;
    println!(
        "wrote {} ({} bytes): tip {tip}, {} sub_epochs, {} segments, {} recent blocks, {:.1}s",
        out.display(),
        bytes.len(),
        wp.sub_epochs.len(),
        wp.sub_epoch_segments.len(),
        wp.recent_chain_data.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
