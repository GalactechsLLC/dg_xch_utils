use clap::Parser;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_plotter::{PlotRequest, PoolBinding, read_metadata};
use dg_xch_pos2::plotting::PlotLimits;
use dg_xch_servers::chain_config::{ensure_selection, read_selection};
use serde::Deserialize;
use std::io::{Error, ErrorKind, Read};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
#[command(about = "Create matching development PoS2 plots sequentially on the CPU")]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    plots_root: PathBuf,
    #[arg(long, value_delimiter = ',', default_value = "cpu,nvidia,amd")]
    farmers: Vec<String>,
    #[arg(long, default_value_t = 1)]
    count: u16,
    #[arg(long, default_value_t = 2)]
    strength: u8,
    #[arg(long, default_value_t = 12_288)]
    memory_mib: u64,
    #[arg(long, default_value_t = 10_000_000_000)]
    max_work: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlotKeys {
    farmer_public_key: String,
    pool_public_key: String,
    payout_address: String,
}

fn public_key(value: &str) -> Result<[u8; 48], Error> {
    hex::decode(value)
        .map_err(Error::other)?
        .try_into()
        .map_err(|_| Error::new(ErrorKind::InvalidData, "plot public key must be 48 bytes"))
}

fn main() -> Result<(), Error> {
    let args = Args::parse();
    let selection = read_selection(&args.root.join("common/chain.json"))?;
    if !matches!(&selection, ChainSelection::Custom(definition) if definition.consensus.as_ref().is_some_and(|parameters| parameters.development))
    {
        return Err(Error::other(
            "stack plot preparation requires an explicit development chain",
        ));
    }
    let constants = selection.constants().map_err(Error::other)?;
    if args.count == 0
        || args.farmers.is_empty()
        || args
            .farmers
            .iter()
            .any(|farmer| !matches!(farmer.as_str(), "cpu" | "nvidia" | "amd"))
        || !(constants.min_plot_strength..=constants.max_plot_strength).contains(&args.strength)
    {
        return Err(Error::other(
            "invalid farmer selection, plot count or strength",
        ));
    }
    let marker = args.plots_root.join("chain.json");
    if !marker.try_exists()?
        && args.plots_root.try_exists()?
        && std::fs::read_dir(&args.plots_root)?
            .next()
            .transpose()?
            .is_some()
    {
        return Err(Error::other(
            "use an empty plots directory or one already bound to this development chain",
        ));
    }
    ensure_selection(&marker, &selection)?;
    let initial = 1usize
        .checked_shl(u32::from(constants.plot_size_v2))
        .ok_or_else(|| Error::other("plot size exceeds platform address space"))?;
    let limits = PlotLimits {
        memory_bytes: args
            .memory_mib
            .checked_mul(1024 * 1024)
            .ok_or_else(|| Error::other("memory budget overflow"))?,
        max_entries: initial
            .checked_add(initial / 8)
            .and_then(|entries| entries.checked_add(65_536))
            .ok_or_else(|| Error::other("entry budget overflow"))?,
        max_work: args.max_work,
    };
    let cancelled = AtomicBool::new(false);
    for farmer in &args.farmers {
        let mut bytes = Vec::new();
        std::fs::File::open(args.root.join(format!("farmer-{farmer}/plot-keys.json")))?
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(Error::other("plot key configuration exceeds 4 KiB"));
        }
        let keys: PlotKeys = serde_json::from_slice(&bytes).map_err(Error::other)?;
        let farmer_public_key = public_key(&keys.farmer_public_key)?;
        let pool_public_key = public_key(&keys.pool_public_key)?;
        dg_xch_keys::decode_puzzle_hash(&keys.payout_address)?;
        let directory = args.plots_root.join(farmer);
        std::fs::create_dir_all(&directory)?;
        for index in 0..args.count {
            let output = directory.join(format!(
                "pos2-k{}-s{}-{index}.plot",
                constants.plot_size_v2, args.strength
            ));
            if output.try_exists()? {
                let metadata = read_metadata(&output)?;
                if metadata.info.k != constants.plot_size_v2
                    || metadata.info.strength != args.strength
                    || metadata.info.index != index
                    || metadata.info.meta_group != 0
                    || metadata.memo.len() != 128
                    || metadata.memo[..48] != pool_public_key
                    || metadata.memo[48..96] != farmer_public_key
                {
                    return Err(Error::other(format!(
                        "existing plot does not match this farmer: {}",
                        output.display()
                    )));
                }
                println!("Existing matching plot: {}", output.display());
                continue;
            }
            eprintln!(
                "Creating {} on CPU; no plotting jobs overlap",
                output.display()
            );
            let request = PlotRequest {
                farmer_public_key,
                pool: PoolBinding::PublicKey(pool_public_key),
                k: constants.plot_size_v2,
                strength: args.strength,
                index,
                meta_group: 0,
                testnet: constants.is_testnet,
            };
            let info = dg_xch_plotter::create(&request, &output, limits, &cancelled)?;
            println!("Created {} ({} bytes)", output.display(), info.file_bytes);
        }
    }
    Ok(())
}
