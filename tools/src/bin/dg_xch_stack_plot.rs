use clap::Parser;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_dev_tools::stack_profile::StackProfile;
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
    /// Require this chain profile before reading keys or creating plots.
    #[arg(long, value_enum, default_value = "bls")]
    chain_profile: StackProfile,
    #[arg(long)]
    pooling: bool,
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
    pool_public_key: Option<String>,
    pool_contract_puzzle_hash: Option<String>,
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
    args.chain_profile.ensure_supported()?;
    let selection = read_selection(&args.root.join("common/chain.json"))?;
    if !matches!(&selection, ChainSelection::Custom(definition) if definition.consensus.as_ref().is_some_and(|parameters| parameters.development))
    {
        return Err(Error::other(
            "stack plot preparation requires an explicit development chain",
        ));
    }
    let constants = selection.constants().map_err(Error::other)?;
    if !args.chain_profile.matches(&constants) {
        return Err(Error::other("stack profile differs from chain"));
    }
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
        if !constants.signature_profile.is_bls() {
            #[cfg(feature = "falcon")]
            {
                prepare_falcon(&args, farmer, &constants, limits, &cancelled)?;
                continue;
            }
            #[cfg(not(feature = "falcon"))]
            return Err(Error::other("Falcon support is not compiled"));
        }
        let mut bytes = Vec::new();
        let filename = if args.pooling {
            "pool-plot-keys.json"
        } else {
            "plot-keys.json"
        };
        std::fs::File::open(args.root.join(format!("farmer-{farmer}/{filename}")))?
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(Error::other("plot key configuration exceeds 4 KiB"));
        }
        let keys: PlotKeys = serde_json::from_slice(&bytes).map_err(Error::other)?;
        let farmer_public_key = public_key(&keys.farmer_public_key)?;
        let pool = match (&keys.pool_public_key, &keys.pool_contract_puzzle_hash) {
            (Some(public), None) if !args.pooling => PoolBinding::PublicKey(public_key(public)?),
            (None, Some(contract)) if args.pooling => PoolBinding::Contract(
                hex::decode(contract)
                    .map_err(Error::other)?
                    .try_into()
                    .map_err(|_| Error::other("pool contract must be 32 bytes"))?,
            ),
            _ => {
                return Err(Error::other(
                    "plot binding does not match the requested pooling mode",
                ));
            }
        };
        let mut memo_prefix = match &pool {
            PoolBinding::PublicKey(key) => key.to_vec(),
            PoolBinding::Contract(hash) => hash.to_vec(),
        };
        memo_prefix.extend_from_slice(&farmer_public_key);
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
                    || metadata.memo.len() != memo_prefix.len() + 32
                    || !metadata.memo.starts_with(&memo_prefix)
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
                pool: pool.clone(),
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

#[cfg(feature = "falcon")]
fn prepare_falcon(
    args: &Args,
    farmer: &str,
    constants: &dg_xch_core::consensus::constants::ConsensusConstants,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    use dg_xch_core::{
        blockchain::proof_of_space::calculate_plot_id_v2,
        signatures::{SignatureSigner, falcon_block::public_key_id, falcon_keys::FalconKeyring},
    };
    use dg_xch_pos2::{compact::CompactPlot, params::ProofParams};
    if args.pooling {
        return Err(Error::other("Falcon pool protocol is not implemented"));
    }
    let keys = FalconKeyring::load(&args.root.join(format!("farmer-{farmer}/falcon-keys.bin")))?;
    if usize::from(args.count) > keys.plots.len() {
        return Err(Error::other("plot count exceeds keyring size"));
    }
    let directory = args.plots_root.join(farmer);
    std::fs::create_dir_all(&directory)?;
    for (index, pair) in keys.plots.iter().take(usize::from(args.count)).enumerate() {
        let index = index as u16;
        let pool = public_key_id(&pair.pool.public_key());
        let plot = public_key_id(&pair.plot.public_key());
        let id = calculate_plot_id_v2(args.strength, plot, Some(pool), None, index, 0);
        let mut memo = b"DGXFAL02".to_vec();
        memo.resize(32, 0);
        memo.extend_from_slice(pool.as_ref());
        memo.extend_from_slice(plot.as_ref());
        let output = directory.join(format!(
            "pos2-k{}-s{}-{index}.plot",
            constants.plot_size_v2, args.strength
        ));
        if output.try_exists()? {
            let existing = read_metadata(&output)?;
            if existing.info.signature_profile != constants.signature_profile
                || existing.info.plot_id != id.const_bytes()
                || existing.info.k != constants.plot_size_v2
                || existing.memo.as_slice() != memo
            {
                return Err(Error::other("existing Falcon plot does not match identity"));
            }
            continue;
        }
        let params = ProofParams::new(
            id,
            constants.plot_size_v2,
            args.strength,
            constants.is_testnet,
        )?;
        let native = CompactPlot::build(params, limits, cancelled)?;
        let temporary = output.with_extension("plot.partial");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&temporary)?;
        let result =
            dg_xch_plotter::format::write_compact(&mut file, &native, index, 0, &memo, cancelled);
        if let Err(error) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        file.sync_all()?;
        read_metadata(&temporary)?;
        // Hard-link publication refuses to overwrite a concurrently created plot.
        std::fs::hard_link(&temporary, &output)?;
        std::fs::remove_file(&temporary)?;
        println!("Created Falcon plot {}", output.display());
    }
    Ok(())
}
