use clap::{Parser, Subcommand};
use dg_xch_plotter::{PlotRequest, PoolBinding};
use dg_xch_pos2::{
    chainer::SearchLimits,
    params::ProofParams,
    plotting::{NativePlot, PlotLimits},
    validator::ProofValidator,
};
use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
#[command(about = "Native Rust PoS2 development plotter")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Resources {
    #[arg(long, default_value_t = 512)]
    memory_mib: u64,
    #[arg(long, default_value_t = 4_194_304)]
    max_entries: usize,
    #[arg(long, default_value_t = 1_000_000_000)]
    max_work: u64,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Backend {
    Cpu,
    Vulkan,
}

#[derive(clap::Args)]
struct Engine {
    #[arg(long, value_enum, default_value = "cpu")]
    backend: Backend,
    #[arg(long, default_value_t = 0)]
    device: usize,
}

impl Engine {
    fn create(
        &self,
        request: &PlotRequest,
        destination: &std::path::Path,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<dg_xch_plotter::PlotInfo, Error> {
        #[cfg(feature = "vulkan")]
        if matches!(self.backend, Backend::Vulkan) {
            return dg_xch_plotter::vulkan::create(
                request,
                destination,
                limits,
                cancelled,
                self.device,
            );
        }
        dg_xch_plotter::create_compact_with_engine(
            request,
            destination,
            limits,
            cancelled,
            |params, limits, cancelled| self.compact(params, limits, cancelled),
        )
    }

    fn compact(
        &self,
        params: ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<dg_xch_pos2::compact::CompactPlot, Error> {
        if matches!(self.backend, Backend::Cpu) && self.device == 0 {
            return dg_xch_pos2::compact::CompactPlot::build(params, limits, cancelled);
        }
        let mut engine = self.hasher(&params)?;
        dg_xch_pos2::compact::CompactPlot::build_with_engine(params, limits, cancelled, &mut engine)
    }

    fn hasher(
        &self,
        params: &ProofParams,
    ) -> Result<Box<dyn dg_xch_pos2::compute::HashEngine>, Error> {
        match self.backend {
            Backend::Cpu if self.device == 0 => {
                Ok(Box::new(dg_xch_pos2::compute::CpuHasher::new(params)))
            }
            Backend::Cpu => Err(Error::new(
                ErrorKind::InvalidInput,
                "--device requires --backend vulkan",
            )),
            Backend::Vulkan => {
                #[cfg(feature = "vulkan")]
                {
                    Ok(Box::new(dg_xch_pos2::vulkan::Hasher::for_params(
                        params,
                        self.device,
                    )?))
                }
                #[cfg(not(feature = "vulkan"))]
                {
                    Err(Error::new(
                        ErrorKind::Unsupported,
                        "rebuild dg_xch_plotter with --features vulkan",
                    ))
                }
            }
        }
    }

    fn build(
        &self,
        params: ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<NativePlot, Error> {
        match self.backend {
            Backend::Cpu => {
                if self.device != 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "--device requires --backend vulkan",
                    ));
                }
                NativePlot::build(params, limits, cancelled)
            }
            Backend::Vulkan => {
                #[cfg(feature = "vulkan")]
                {
                    dg_xch_pos2::vulkan::build(params, limits, cancelled, self.device)
                }
                #[cfg(not(feature = "vulkan"))]
                {
                    Err(Error::new(
                        ErrorKind::Unsupported,
                        "rebuild dg_xch_plotter with --features vulkan",
                    ))
                }
            }
        }
    }
}

impl Resources {
    fn limits(&self) -> Result<PlotLimits, Error> {
        Ok(PlotLimits {
            memory_bytes: self
                .memory_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "memory limit overflow"))?,
            max_entries: self.max_entries,
            max_work: self.max_work,
        })
    }
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "List hardware Vulkan devices; software adapters are excluded")]
    Devices,
    #[command(
        about = "Read challenge fragments and reconstruct independently verified proofs with bounded memory"
    )]
    ProvePlot {
        path: PathBuf,
        #[arg(long)]
        challenge: String,
        #[arg(long)]
        testnet: bool,
        #[command(flatten)]
        resources: Resources,
        #[command(flatten)]
        engine: Engine,
    },
    Create {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        farmer_key: String,
        #[arg(
            long,
            required_unless_present = "contract",
            conflicts_with = "contract"
        )]
        pool_key: Option<String>,
        #[arg(long)]
        contract: Option<String>,
        #[arg(long, default_value_t = 28)]
        k: u8,
        #[arg(long, default_value_t = 2)]
        strength: u8,
        #[arg(long, default_value_t = 0)]
        index: u16,
        #[arg(long, default_value_t = 0)]
        meta_group: u8,
        #[arg(long)]
        testnet: bool,
        #[arg(long)]
        experimental_size: bool,
        #[command(flatten)]
        resources: Resources,
        #[command(flatten)]
        engine: Engine,
    },
    Inspect {
        path: PathBuf,
    },
    #[command(
        about = "Build native tables in memory, search a challenge and prove using retained witnesses"
    )]
    SelfTest {
        #[arg(long)]
        plot_id: String,
        #[arg(long, default_value_t = 18)]
        k: u8,
        #[arg(long, default_value_t = 2)]
        strength: u8,
        #[arg(long)]
        challenge: String,
        #[arg(long)]
        testnet: bool,
        #[command(flatten)]
        resources: Resources,
        #[command(flatten)]
        engine: Engine,
    },
    #[command(about = "Verify proof validity natively; does not check block eligibility")]
    Verify {
        #[arg(long)]
        plot_id: String,
        #[arg(long, default_value_t = 28)]
        k: u8,
        #[arg(long, default_value_t = 2)]
        strength: u8,
        #[arg(long)]
        challenge: String,
        #[arg(long)]
        proof: String,
        #[arg(long)]
        testnet: bool,
    },
}

fn bytes<const SIZE: usize>(value: &str) -> Result<[u8; SIZE], Error> {
    let decoded =
        hex::decode(value).map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid hex"))?;
    decoded
        .try_into()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, format!("expected {SIZE} bytes")))
}

pub fn run(arguments: &[std::ffi::OsString]) -> Result<(), Error> {
    let cancelled = AtomicBool::new(false);
    match Cli::parse_from(
        std::iter::once(std::ffi::OsString::from("dgx plotter")).chain(arguments.iter().cloned()),
    )
    .command
    {
        Command::Devices => {
            #[cfg(feature = "vulkan")]
            {
                for adapter in dg_xch_pos2::vulkan::adapters() {
                    println!(
                        "{}: {} vendor={:#06x} device={:#06x}",
                        adapter.ordinal, adapter.name, adapter.vendor, adapter.device
                    );
                }
            }
            #[cfg(not(feature = "vulkan"))]
            {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "rebuild dg_xch_plotter with --features vulkan",
                ));
            }
        }
        Command::ProvePlot {
            path,
            challenge,
            testnet,
            resources,
            engine,
        } => {
            let mut plot = dg_xch_plotter::reader::PlotReader::open(
                &path,
                testnet,
                resources.limits()?.memory_bytes,
            )?;
            let mut hasher = engine.hasher(plot.params())?;
            let challenge = bytes::<32>(&challenge)?.into();
            for chain in plot.qualities(
                challenge,
                SearchLimits {
                    max_hashes: resources.max_work,
                    max_results: 1024,
                },
                &cancelled,
            )? {
                println!(
                    "quality={} proof={}",
                    dg_xch_pos2::quality::quality_hash(&chain.fragments, plot.info.strength),
                    hex::encode(plot.prove_with_engine(
                        &chain,
                        challenge,
                        resources.limits()?,
                        &cancelled,
                        &mut hasher
                    )?)
                );
            }
        }
        Command::Create {
            output,
            farmer_key,
            pool_key,
            contract,
            k,
            strength,
            index,
            meta_group,
            testnet,
            experimental_size,
            resources,
            engine,
        } => {
            if k != 28 && !experimental_size {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "non-k28 plots require --experimental-size",
                ));
            }
            let pool = match (pool_key, contract) {
                (Some(key), None) => PoolBinding::PublicKey(bytes(&key)?),
                (None, Some(hash)) => PoolBinding::Contract(bytes(&hash)?),
                _ => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "provide exactly one pool binding",
                    ));
                }
            };
            eprintln!(
                "Compact in-memory pipeline; set explicit memory, entry and work budgets for large plots. Network mode is not stored in the plot."
            );
            let info = engine.create(
                &PlotRequest {
                    farmer_public_key: bytes(&farmer_key)?,
                    pool,
                    k,
                    strength,
                    index,
                    meta_group,
                    testnet,
                },
                &output,
                resources.limits()?,
                &cancelled,
            )?;
            println!("{info:?}");
        }
        Command::Inspect { path } => println!("{:?}", dg_xch_plotter::inspect(&path)?),
        Command::SelfTest {
            plot_id,
            k,
            strength,
            challenge,
            testnet,
            resources,
            engine,
        } => {
            let plot = engine.build(
                ProofParams::new(bytes::<32>(&plot_id)?.into(), k, strength, testnet)?,
                resources.limits()?,
                &cancelled,
            )?;
            let challenge = bytes::<32>(&challenge)?.into();
            let chains = plot.qualities(
                challenge,
                SearchLimits {
                    max_hashes: resources.max_work,
                    max_results: 1024,
                },
                &cancelled,
            )?;
            println!("tables={:?} qualities={}", plot.table_counts, chains.len());
            for chain in chains {
                println!("proof={}", hex::encode(plot.prove(&chain, challenge)?));
            }
        }
        Command::Verify {
            plot_id,
            k,
            strength,
            challenge,
            proof,
            testnet,
        } => {
            let params = ProofParams::new(bytes::<32>(&plot_id)?.into(), k, strength, testnet)?;
            if proof.len() != usize::from(k) * 32 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "incorrect proof length",
                ));
            }
            let packed = hex::decode(proof)
                .map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid proof hex"))?;
            let fragments = ProofValidator::new(params)?
                .validate_packed_proof(&packed, bytes::<32>(&challenge)?.into())
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "invalid proof"))?;
            let quality = dg_xch_pos2::quality::quality_hash(&fragments, strength);
            println!("quality={}", hex::encode(AsRef::<[u8]>::as_ref(&quality)));
        }
    }
    Ok(())
}
