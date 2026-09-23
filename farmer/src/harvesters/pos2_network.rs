use crate::farmer::PathInfo;
use crate::farmer::config::{Pos2Backend, Pos2HarvesterConfig};
use crate::harvesters::FarmingKeys;
use crate::harvesters::pos2::DiskHarvester;
use blst::min_pk::{PublicKey, SecretKey};
use dg_xch_core::blockchain::proof_of_space::{
    ProofOfSpace, calculate_plot_id_v2, calculate_pos_challenge, calculate_prefix_bits_v2,
    generate_plot_public_key, passes_plot_filter,
};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::bls_bindings::sign_prepend;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality_v2, calculate_sp_interval_iters,
};
use dg_xch_core::protocols::harvester::{
    NewProofOfSpace, NewSignagePointHarvester, RequestSignatures, RespondSignatures,
};
use dg_xch_keys::master_sk_to_local_sk;
use dg_xch_plotter::backend::{GpuBackend, GpuDevice, GpuPreference, parse_cuda_probe, select_gpu};
use dg_xch_pos::pos2::chainer::{Chain, SearchLimits};
use dg_xch_pos::pos2::compute::{CpuHasher, check_cancelled};
use dg_xch_pos::pos2::params::ProofParams;
use dg_xch_pos::pos2::plotting::PlotLimits;
use dg_xch_pos::pos2::quality::quality_hash;
use dg_xch_pos::pos2::validator::ProofValidator;
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{RwLock, Semaphore};

const MAX_HELPER_OUTPUT: usize = 8192;
const MAX_DISCOVERED_PLOTS: usize = 100_000;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Pos2Status {
    pub backend: Pos2Backend,
    pub device: usize,
    pub loaded_plots: u64,
    pub filter_passes: u64,
    pub eligible_qualities: u64,
    pub recovered_proofs: u64,
    pub recovery_errors: u64,
    pub deadlines_exceeded: u64,
}

#[derive(Default)]
struct Counters {
    loaded_plots: AtomicU64,
    filter_passes: AtomicU64,
    eligible_qualities: AtomicU64,
    recovered_proofs: AtomicU64,
    recovery_errors: AtomicU64,
    deadlines_exceeded: AtomicU64,
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct FarmingPlot {
    path: PathInfo,
    info: dg_xch_plotter::PlotInfo,
    local_secret: SecretKey,
    farmer_public_key: Bytes48,
    pool_public_key: Option<Bytes48>,
    pool_contract_puzzle_hash: Option<Bytes32>,
    plot_public_key: Bytes48,
}

impl FarmingPlot {
    fn open(
        path: PathBuf,
        keys: &FarmingKeys,
        constants: &ConsensusConstants,
    ) -> Result<Self, Error> {
        let dg_xch_plotter::PlotMetadata { info, memo } = dg_xch_plotter::read_metadata(&path)?;
        if info.k != constants.plot_size_v2
            || info.strength < constants.min_plot_strength
            || info.strength > constants.max_plot_strength
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "PoS2 plot parameters do not match this chain",
            ));
        }
        let pool_size = if info.portable { 32 } else { 48 };
        let farmer = PublicKey::key_validate(&memo[pool_size..pool_size + 48])
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid plot farmer key"))?;
        let farmer_public_key = Bytes48::from(farmer.to_bytes());
        if !keys.farmer_public_keys.contains(&farmer_public_key) {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "missing PoS2 farmer key",
            ));
        }
        let (pool_public_key, pool_contract_puzzle_hash) = if info.portable {
            (
                None,
                Some(Bytes32::from(
                    <[u8; 32]>::try_from(&memo[..32]).map_err(Error::other)?,
                )),
            )
        } else {
            let pool = PublicKey::key_validate(&memo[..48])
                .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid plot pool key"))?;
            let public_key = Bytes48::from(pool.to_bytes());
            if !keys.pool_public_keys.contains(&public_key) {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "missing PoS2 pool signing key",
                ));
            }
            (Some(public_key), None)
        };
        let master = SecretKey::from_bytes(&memo[pool_size + 48..])
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid plot local secret"))?;
        let local_secret = master_sk_to_local_sk(&master)?;
        let plot_public_key = Bytes48::from(
            generate_plot_public_key(&local_secret.sk_to_pk(), &farmer, info.portable)?.to_bytes(),
        );
        if calculate_plot_id_v2(
            info.strength,
            plot_public_key,
            pool_public_key,
            pool_contract_puzzle_hash,
            info.index,
            info.meta_group,
        )
        .as_ref()
            != info.plot_id
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "PoS2 signing identity does not match plot ID",
            ));
        }
        Ok(Self {
            path: PathInfo::new(path),
            info,
            local_secret,
            farmer_public_key,
            pool_public_key,
            pool_contract_puzzle_hash,
            plot_public_key,
        })
    }

    fn proof(&self, challenge: Bytes32, proof: Vec<u8>) -> ProofOfSpace {
        ProofOfSpace::v2(
            challenge,
            self.pool_public_key,
            self.pool_contract_puzzle_hash,
            self.plot_public_key,
            self.info.index,
            self.info.meta_group,
            self.info.strength,
            proof.into(),
        )
    }

    fn sign(&self, request: RequestSignatures) -> Result<RespondSignatures, Error> {
        if request.messages.len() != 2
            && !(request.messages.len() == 1 && self.pool_contract_puzzle_hash.is_some())
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "expected two block messages or one portable-pool partial message",
            ));
        }
        let public_key = PublicKey::key_validate(self.plot_public_key.as_ref())
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid aggregate plot key"))?;
        Ok(RespondSignatures {
            plot_identifier: request.plot_identifier,
            challenge_hash: request.challenge_hash,
            sp_hash: request.sp_hash,
            local_pk: self.local_secret.sk_to_pk().to_bytes().into(),
            farmer_pk: self.farmer_public_key,
            message_signatures: request
                .messages
                .into_iter()
                .map(|message| {
                    (
                        message,
                        sign_prepend(&self.local_secret, message.as_ref(), &public_key)
                            .to_bytes()
                            .into(),
                    )
                })
                .collect(),
            include_source_signature_data: false,
            farmer_reward_address_override: None,
        })
    }
}

pub struct Pos2Harvester {
    plots: RwLock<HashMap<String, Arc<FarmingPlot>>>,
    directories: Vec<PathBuf>,
    keys: Arc<FarmingKeys>,
    constants: ConsensusConstants,
    config: Pos2HarvesterConfig,
    slots: Arc<Semaphore>,
    running: Arc<AtomicBool>,
    counters: Counters,
}

impl Pos2Harvester {
    pub async fn new(
        mut config: Pos2HarvesterConfig,
        inherited_directories: &[PathBuf],
        keys: Arc<FarmingKeys>,
        constants: ConsensusConstants,
        running: Arc<AtomicBool>,
    ) -> Result<Arc<Self>, Error> {
        config.validate()?;
        if matches!(
            config.backend,
            Pos2Backend::Auto | Pos2Backend::Cuda | Pos2Backend::Vulkan
        ) {
            let (backend, device) = select_backend(&config).await?;
            config.backend = backend;
            config.device = device;
        }
        let directories = if config.plot_directories.is_empty() {
            inherited_directories.to_vec()
        } else {
            config.plot_directories.iter().map(PathBuf::from).collect()
        };
        let harvester = Arc::new(Self {
            plots: RwLock::new(HashMap::new()),
            directories,
            keys,
            constants,
            slots: Arc::new(Semaphore::new(config.parallelism)),
            config,
            running,
            counters: Counters::default(),
        });
        harvester.refresh().await?;
        Ok(harvester)
    }

    pub async fn paths(&self) -> Vec<PathBuf> {
        self.plots
            .read()
            .await
            .values()
            .map(|plot| plot.path.path.clone())
            .collect()
    }

    pub fn status(&self) -> Pos2Status {
        Pos2Status {
            backend: self.config.backend,
            device: self.config.device,
            loaded_plots: self.counters.loaded_plots.load(Ordering::Relaxed),
            filter_passes: self.counters.filter_passes.load(Ordering::Relaxed),
            eligible_qualities: self.counters.eligible_qualities.load(Ordering::Relaxed),
            recovered_proofs: self.counters.recovered_proofs.load(Ordering::Relaxed),
            recovery_errors: self.counters.recovery_errors.load(Ordering::Relaxed),
            deadlines_exceeded: self.counters.deadlines_exceeded.load(Ordering::Relaxed),
        }
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        let directories = self.directories.clone();
        let keys = self.keys.clone();
        let constants = self.constants;
        let running = self.running.clone();
        let plots = tokio::task::spawn_blocking(move || {
            discover(&directories, &keys, &constants, &running)
        })
        .await
        .map_err(Error::other)??;
        let count = plots.len();
        *self.plots.write().await = plots;
        self.counters
            .loaded_plots
            .store(count as u64, Ordering::Relaxed);
        if count > 0 {
            info!(
                "Loaded {count} PoS2 plots using {:?} device {}",
                self.config.backend, self.config.device
            );
        }
        Ok(())
    }

    pub async fn sign(
        &self,
        request: &RequestSignatures,
    ) -> Result<Option<RespondSignatures>, Error> {
        let identifier = request
            .plot_identifier
            .get(64..)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "malformed plot identifier"))?;
        let plot = self.plots.read().await.get(identifier).cloned();
        plot.map(|plot| plot.sign(request.clone())).transpose()
    }

    pub async fn farm(
        &self,
        point: Arc<NewSignagePointHarvester>,
        height: u32,
        proofs: tokio::sync::mpsc::Sender<NewProofOfSpace>,
    ) -> Result<(), Error> {
        if point.last_tx_height < self.constants.hard_fork2_height {
            return Ok(());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(cancelled.clone());
        let operation = self.farm_inner(point, height, cancelled, proofs);
        tokio::select! {
            result = tokio::time::timeout(Duration::from_millis(self.config.deadline_ms), operation) => {
                result.map_err(|_| {
                    self.counters.deadlines_exceeded.fetch_add(1, Ordering::Relaxed);
                    Error::new(ErrorKind::TimedOut, "PoS2 signage recovery deadline exceeded")
                })?
            }
            () = wait_for_shutdown(&self.running) => Err(Error::new(ErrorKind::Interrupted, "farmer stopped")),
        }
    }

    async fn farm_inner(
        &self,
        point: Arc<NewSignagePointHarvester>,
        height: u32,
        cancelled: Arc<AtomicBool>,
        proofs: tokio::sync::mpsc::Sender<NewProofOfSpace>,
    ) -> Result<(), Error> {
        let permit = Arc::new(
            self.slots
                .clone()
                .acquire_owned()
                .await
                .map_err(Error::other)?,
        );
        let plots: Vec<_> = self.plots.read().await.values().cloned().collect();
        let prefix_bits = calculate_prefix_bits_v2(&self.constants, height);
        let limits = PlotLimits {
            memory_bytes: self
                .config
                .memory_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| Error::other("PoS2 memory budget overflow"))?,
            max_entries: self.config.max_entries,
            max_work: self.config.max_work,
        };
        let search = SearchLimits {
            max_hashes: self.config.search_hashes,
            max_results: self.config.max_qualities,
        };
        for plot in plots {
            check_cancelled(&cancelled)?;
            let plot_id = Bytes32::from(plot.info.plot_id);
            if !passes_plot_filter(prefix_bits, plot_id, point.challenge_hash, point.sp_hash) {
                continue;
            }
            self.counters.filter_passes.fetch_add(1, Ordering::Relaxed);
            let challenge = calculate_pos_challenge(plot_id, point.challenge_hash, point.sp_hash);
            let work_plot = plot.clone();
            let work_cancelled = cancelled.clone();
            let work_permit = permit.clone();
            let testnet = self.constants.is_testnet;
            let selection = tokio::task::spawn_blocking(move || {
                let _permit = work_permit;
                let mut disk =
                    DiskHarvester::open(&work_plot.path.path, testnet, limits, &work_cancelled)?;
                if disk.info() != &work_plot.info {
                    return Err(Error::other("PoS2 plot changed since discovery"));
                }
                let chains = disk.qualities(challenge, search, &work_cancelled)?;
                Ok::<_, Error>((disk, chains))
            })
            .await
            .map_err(Error::other)?;
            let (mut disk, chains) = match selection {
                Ok(selection) => selection,
                Err(error) => {
                    self.counters
                        .recovery_errors
                        .fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "PoS2 quality search failed for {}: {error}",
                        plot.path.path.display()
                    );
                    continue;
                }
            };
            for chain in chains {
                check_cancelled(&cancelled)?;
                let quality = quality_hash(&chain.fragments, plot.info.strength);
                if !eligible(
                    &self.constants,
                    &point,
                    plot.pool_contract_puzzle_hash,
                    quality,
                )? {
                    continue;
                }
                self.counters
                    .eligible_qualities
                    .fetch_add(1, Ordering::Relaxed);
                let recovered = if self.config.backend == Pos2Backend::Cuda {
                    recover_cuda(
                        &self.config,
                        &plot.path.path,
                        disk.params(),
                        &chain,
                        challenge,
                        &cancelled,
                    )
                    .await
                } else {
                    let work_cancelled = cancelled.clone();
                    let work_permit = permit.clone();
                    let backend = self.config.backend;
                    let device = self.config.device;
                    let result = tokio::task::spawn_blocking(move || {
                        let _permit = work_permit;
                        let result = recover_local(
                            &mut disk,
                            &chain,
                            challenge,
                            backend,
                            device,
                            &work_cancelled,
                        );
                        (disk, result)
                    })
                    .await
                    .map_err(Error::other)?;
                    disk = result.0;
                    result.1
                };
                let proof = match recovered {
                    Ok(proof) => proof,
                    Err(error) => {
                        self.counters
                            .recovery_errors
                            .fetch_add(1, Ordering::Relaxed);
                        warn!(
                            "PoS2 proof recovery failed for {}: {error}",
                            plot.path.path.display()
                        );
                        continue;
                    }
                };
                let proof = plot.proof(challenge, proof);
                if dg_xch_pos::verify_and_get_quality_string_with_context(
                    &proof,
                    &self.constants,
                    point.challenge_hash,
                    point.sp_hash,
                    height,
                    point.last_tx_height,
                ) != Some(quality)
                {
                    self.counters
                        .recovery_errors
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "recovered PoS2 proof failed consensus verification",
                    ));
                }
                self.counters
                    .recovered_proofs
                    .fetch_add(1, Ordering::Relaxed);
                proofs
                    .send(NewProofOfSpace {
                        challenge_hash: point.challenge_hash,
                        sp_hash: point.sp_hash,
                        plot_identifier: format!(
                            "{}{}",
                            hex::encode(quality),
                            plot.path.identifier()
                        ),
                        proof,
                        signage_point_index: point.signage_point_index,
                        include_source_signature_data: false,
                        farmer_reward_address_override: None,
                        fee_info: None,
                    })
                    .await
                    .map_err(|_| {
                        Error::new(ErrorKind::BrokenPipe, "PoS2 proof consumer stopped")
                    })?;
            }
        }
        Ok(())
    }
}

fn eligible(
    constants: &ConsensusConstants,
    point: &NewSignagePointHarvester,
    contract: Option<Bytes32>,
    quality: Bytes32,
) -> Result<bool, Error> {
    if calculate_iterations_quality_v2(
        constants.difficulty_constant_factor,
        quality,
        constants.plot_size_v2,
        point.difficulty,
        point.sp_hash,
    ) < calculate_sp_interval_iters(constants, point.sub_slot_iters)?
    {
        return Ok(true);
    }
    for difficulty in &point.pool_difficulties {
        if Some(difficulty.pool_contract_puzzle_hash) == contract
            && calculate_iterations_quality_v2(
                constants.difficulty_constant_factor,
                quality,
                constants.plot_size_v2,
                difficulty.difficulty,
                point.sp_hash,
            ) < calculate_sp_interval_iters(constants, difficulty.sub_slot_iters)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn recover_local(
    disk: &mut DiskHarvester,
    chain: &Chain,
    challenge: Bytes32,
    backend: Pos2Backend,
    device: usize,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    check_cancelled(cancelled)?;
    match backend {
        Pos2Backend::Cpu => {
            let mut engine = CpuHasher::new(disk.params());
            disk.prove_with_engine(chain, challenge, cancelled, &mut engine)
                .map(|candidate| candidate.proof)
        }
        Pos2Backend::Vulkan => {
            #[cfg(feature = "vulkan")]
            {
                let mut engine =
                    dg_xch_pos::pos2::vulkan::Hasher::for_params(disk.params(), device)?;
                disk.prove_with_engine(chain, challenge, cancelled, &mut engine)
                    .map(|candidate| candidate.proof)
            }
            #[cfg(not(feature = "vulkan"))]
            {
                let _ = device;
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "rebuild dg_xch_farmer with feature vulkan",
                ))
            }
        }
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "unresolved PoS2 proof backend",
        )),
    }
}

fn discover(
    directories: &[PathBuf],
    keys: &FarmingKeys,
    constants: &ConsensusConstants,
    running: &AtomicBool,
) -> Result<HashMap<String, Arc<FarmingPlot>>, Error> {
    let mut plots = HashMap::new();
    let mut plot_ids = HashSet::new();
    for directory in directories {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(
                    "Cannot scan PoS2 plot directory {}: {error}",
                    directory.display()
                );
                continue;
            }
        };
        for entry in entries {
            if !running.load(Ordering::Acquire) {
                return Err(Error::new(ErrorKind::Interrupted, "farmer stopped"));
            }
            let entry = entry?;
            if !entry.file_type()?.is_file()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "plot")
            {
                continue;
            }
            let path = entry.path().canonicalize()?;
            let mut magic = [0u8; 4];
            if File::open(&path)
                .and_then(|mut input| input.read_exact(&mut magic))
                .is_err()
                || &magic != b"pos2"
            {
                continue;
            }
            match FarmingPlot::open(path.clone(), keys, constants) {
                Ok(plot) => {
                    if !plot_ids.insert(plot.info.plot_id) {
                        continue;
                    }
                    if plots.len() == MAX_DISCOVERED_PLOTS {
                        return Err(Error::other("PoS2 plot registry limit exceeded"));
                    }
                    plots.try_reserve(1).map_err(Error::other)?;
                    plots.insert(plot.path.identifier().to_owned(), Arc::new(plot));
                }
                Err(error) => warn!("Cannot farm PoS2 plot {}: {error}", path.display()),
            }
        }
    }
    Ok(plots)
}

async fn wait_for_shutdown(running: &AtomicBool) {
    while running.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn read_bounded(input: impl AsyncRead + Unpin, maximum: usize) -> Result<Vec<u8>, Error> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(maximum + 1)
        .map_err(Error::other)?;
    input
        .take(maximum as u64 + 1)
        .read_to_end(&mut result)
        .await?;
    if result.len() > maximum {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "CUDA helper output limit exceeded",
        ));
    }
    Ok(result)
}

async fn helper_output(mut command: Command) -> Result<Vec<u8>, Error> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("missing CUDA helper stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::other("missing CUDA helper stderr"))?;
    let completed = tokio::try_join!(
        child.wait(),
        read_bounded(stdout, MAX_HELPER_OUTPUT),
        read_bounded(stderr, MAX_HELPER_OUTPUT)
    );
    match completed {
        Ok((status, output, _)) if status.success() => Ok(output),
        Ok((status, _, _)) => Err(Error::other(format!(
            "CUDA proof helper exited with {status}"
        ))),
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error)
        }
    }
}

async fn select_backend(config: &Pos2HarvesterConfig) -> Result<(Pos2Backend, usize), Error> {
    let cuda = if matches!(config.backend, Pos2Backend::Auto | Pos2Backend::Cuda) {
        if let Some(helper) = &config.cuda_helper {
            let mut command = Command::new(helper);
            command
                .arg("--probe-device")
                .arg("--device")
                .arg(config.device.to_string());
            match tokio::time::timeout(Duration::from_secs(15), helper_output(command)).await {
                Ok(Ok(output)) => Some(parse_cuda_probe(
                    std::str::from_utf8(&output).map_err(Error::other)?,
                    config.device,
                )?),
                Ok(Err(error)) if config.backend == Pos2Backend::Cuda => return Err(error),
                Err(_) if config.backend == Pos2Backend::Cuda => {
                    return Err(Error::new(
                        ErrorKind::TimedOut,
                        "CUDA device probe timed out",
                    ));
                }
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    #[cfg(feature = "vulkan")]
    let vulkan: Vec<GpuDevice> = tokio::task::spawn_blocking(|| {
        dg_xch_pos::pos2::vulkan::adapters()
            .into_iter()
            .map(|adapter| GpuDevice {
                ordinal: adapter.ordinal,
                vendor: adapter.vendor,
                name: adapter.name,
            })
            .collect()
    })
    .await
    .map_err(Error::other)?;
    #[cfg(not(feature = "vulkan"))]
    let vulkan: Vec<GpuDevice> = Vec::new();
    let preference = match config.backend {
        Pos2Backend::Auto => GpuPreference::Auto,
        Pos2Backend::Cuda => GpuPreference::Cuda(config.device),
        Pos2Backend::Vulkan => GpuPreference::Vulkan(config.device),
        Pos2Backend::Cpu => return Ok((Pos2Backend::Cpu, 0)),
    };
    let selection = select_gpu(preference, cuda.as_ref(), &vulkan)?;
    Ok((
        match selection.backend {
            GpuBackend::Cuda => Pos2Backend::Cuda,
            GpuBackend::Vulkan => Pos2Backend::Vulkan,
        },
        selection.device.ordinal,
    ))
}

async fn recover_cuda(
    config: &Pos2HarvesterConfig,
    path: &Path,
    params: &ProofParams,
    chain: &Chain,
    challenge: Bytes32,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    check_cancelled(cancelled)?;
    let quality = quality_hash(&chain.fragments, params.strength());
    let mut command = Command::new(
        config
            .cuda_helper
            .as_ref()
            .ok_or_else(|| Error::other("CUDA proof helper is not configured"))?,
    );
    command
        .arg("--prove-plot")
        .arg(path)
        .arg("--challenge")
        .arg(hex::encode(challenge))
        .arg("--quality")
        .arg(hex::encode(quality))
        .arg("--device")
        .arg(config.device.to_string())
        .arg("--memory-mib")
        .arg(config.memory_mib.to_string())
        .arg("--max-entries")
        .arg(config.max_entries.to_string())
        .arg("--max-work")
        .arg(config.max_work.to_string());
    if params.is_testnet() {
        command.arg("--testnet");
    }
    let output = helper_output(command).await?;
    check_cancelled(cancelled)?;
    decode_cuda_proof(&output, params, chain, challenge)
}

fn decode_cuda_proof(
    output: &[u8],
    params: &ProofParams,
    chain: &Chain,
    challenge: Bytes32,
) -> Result<Vec<u8>, Error> {
    if output.len() > MAX_HELPER_OUTPUT {
        return Err(Error::other("CUDA helper output limit exceeded"));
    }
    let text = std::str::from_utf8(output)
        .map_err(Error::other)?
        .trim_end_matches(['\r', '\n']);
    let (quality, proof) = text
        .strip_prefix("quality=")
        .and_then(|text| text.split_once(" proof="))
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "invalid CUDA proof response"))?;
    let expected = quality_hash(&chain.fragments, params.strength());
    let quality = quality.strip_prefix("0x").unwrap_or(quality);
    if quality != hex::encode(expected) || proof.len() != usize::from(params.k()) * 32 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "CUDA returned an unexpected quality or proof length",
        ));
    }
    let proof = hex::decode(proof).map_err(Error::other)?;
    let fragments = ProofValidator::new(params.clone())?
        .validate_packed_proof(&proof, challenge)
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "CUDA returned an invalid proof"))?;
    if fragments != chain.fragments {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "CUDA proof does not recover the requested chain",
        ));
    }
    Ok(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use blst::BLST_ERROR;
    use blst::min_pk::{AggregateSignature, Signature};
    use dg_xch_core::blockchain::proof_of_space::generate_taproot_sk;
    use dg_xch_core::consensus::chain_definition::ChainDefinition;
    use dg_xch_core::constants::AUG_SCHEME_DST;
    use dg_xch_core::protocols::harvester::PoolDifficulty;
    use dg_xch_pos::pos2::compact::PackedChunk;
    use zeroize::Zeroizing;

    fn constants() -> ConsensusConstants {
        ChainDefinition::development("farmer-pos2-unit-fixture".into())
            .constants()
            .unwrap()
    }

    fn fixture(
        directory: &Path,
        strength: u8,
        portable: bool,
    ) -> (PathBuf, FarmingKeys, SecretKey) {
        let farmer = SecretKey::key_gen(&[3u8; 32], &[]).unwrap();
        let pool = SecretKey::key_gen(&[4u8; 32], &[]).unwrap();
        let contract = Bytes32::from([5; 32]);
        let master = SecretKey::key_gen(&[6u8; 32], &[]).unwrap();
        let local = master_sk_to_local_sk(&master).unwrap();
        let plot_key =
            generate_plot_public_key(&local.sk_to_pk(), &farmer.sk_to_pk(), portable).unwrap();
        let plot_id = calculate_plot_id_v2(
            strength,
            plot_key.to_bytes().into(),
            (!portable).then(|| pool.sk_to_pk().to_bytes().into()),
            portable.then_some(contract),
            513,
            7,
        );
        let params = ProofParams::new(plot_id, 28, strength, constants().is_testnet).unwrap();
        let mut memo = Zeroizing::new(Vec::new());
        if portable {
            memo.extend_from_slice(contract.as_ref());
        } else {
            memo.extend_from_slice(&pool.sk_to_pk().to_bytes());
        }
        memo.extend_from_slice(&farmer.sk_to_pk().to_bytes());
        memo.extend_from_slice(&master.to_bytes());
        let path = directory.join("fixture.plot");
        let mut output = File::create(&path).unwrap();
        dg_xch_plotter::format::write_packed_chunks(
            &mut output,
            &params,
            4096,
            513,
            7,
            &memo,
            &AtomicBool::new(false),
            |_| {
                Ok(PackedChunk {
                    count: 0,
                    deltas: Vec::new(),
                    stubs: Vec::new(),
                })
            },
        )
        .unwrap();
        (
            path,
            FarmingKeys {
                farmer_public_keys: vec![farmer.sk_to_pk().to_bytes().into()],
                pool_public_keys: vec![pool.sk_to_pk().to_bytes().into()],
                pool_contract_hashes: vec![contract],
            },
            farmer,
        )
    }

    #[test]
    fn portable_partial_signatures_accept_one_message_only_for_pool_plots() {
        for portable in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (path, keys, _) = fixture(directory.path(), 5, portable);
            let plot = FarmingPlot::open(path, &keys, &constants()).unwrap();
            for count in [0, 1, 2, 3] {
                let request = RequestSignatures {
                    plot_identifier: plot.path.identifier().to_string(),
                    challenge_hash: [10; 32].into(),
                    sp_hash: [11; 32].into(),
                    messages: vec![[12; 32].into(); count],
                    message_data: None,
                    rc_block_unfinished: None,
                };
                assert_eq!(
                    plot.sign(request).is_ok(),
                    count == 2 || portable && count == 1
                );
            }
        }
    }

    #[test]
    fn k28_discovery_preserves_metadata_and_signs_with_the_memo_identity() {
        for portable in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let (path, keys, farmer) = fixture(directory.path(), 5, portable);
            let plot = FarmingPlot::open(path, &keys, &constants()).unwrap();
            let proof = plot.proof(Bytes32::from([7; 32]), Vec::new());
            assert_eq!(
                (
                    proof.version,
                    proof.size,
                    proof.strength,
                    proof.plot_index,
                    proof.meta_group
                ),
                (1, 0, 5, 513, 7)
            );
            assert_eq!(proof.get_plot_id(), Some(Bytes32::from(plot.info.plot_id)));
            let messages = vec![Bytes32::from([8; 32]), Bytes32::from([9; 32])];
            let response = plot
                .sign(RequestSignatures {
                    plot_identifier: format!("{}{}", "00".repeat(32), plot.path.identifier()),
                    challenge_hash: Bytes32::from([10; 32]),
                    sp_hash: messages[0],
                    messages: messages.clone(),
                    message_data: None,
                    rc_block_unfinished: None,
                })
                .unwrap();
            let aggregate_key = PublicKey::from_bytes(plot.plot_public_key.as_ref()).unwrap();
            for (message, signature) in response.message_signatures {
                let local = Signature::from_bytes(signature.as_ref()).unwrap();
                let farmer_signature = sign_prepend(&farmer, message.as_ref(), &aggregate_key);
                let taproot = portable.then(|| {
                    sign_prepend(
                        &generate_taproot_sk(&plot.local_secret.sk_to_pk(), &farmer.sk_to_pk())
                            .unwrap(),
                        message.as_ref(),
                        &aggregate_key,
                    )
                });
                let mut signatures = vec![&local, &farmer_signature];
                if let Some(signature) = &taproot {
                    signatures.push(signature);
                }
                let aggregate = AggregateSignature::aggregate(&signatures, true)
                    .unwrap()
                    .to_signature();
                assert_eq!(
                    aggregate.verify(
                        true,
                        message.as_ref(),
                        AUG_SCHEME_DST,
                        &aggregate_key.to_bytes(),
                        &aggregate_key,
                        true
                    ),
                    BLST_ERROR::BLST_SUCCESS
                );
            }
        }
    }

    #[test]
    fn k28_discovery_rejects_wrong_keys_and_network_parameters() {
        let directory = tempfile::tempdir().unwrap();
        let (path, mut keys, _) = fixture(directory.path(), 2, false);
        let mut wrong_constants = constants();
        wrong_constants.plot_size_v2 = 30;
        assert!(FarmingPlot::open(path.clone(), &keys, &wrong_constants).is_err());
        keys.pool_public_keys.clear();
        assert!(FarmingPlot::open(path.clone(), &keys, &constants()).is_err());
        keys.farmer_public_keys.clear();
        assert!(FarmingPlot::open(path, &keys, &constants()).is_err());
    }

    #[test]
    fn k28_discovery_deduplicates_plot_ids_and_honors_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let (path, keys, _) = fixture(directory.path(), 2, true);
        std::fs::copy(&path, directory.path().join("duplicate.plot")).unwrap();
        let directories = vec![directory.path().to_path_buf()];
        assert_eq!(
            discover(&directories, &keys, &constants(), &AtomicBool::new(true))
                .unwrap()
                .len(),
            1
        );
        assert!(discover(&directories, &keys, &constants(), &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn k28_eligibility_checks_block_and_matching_pool_difficulty() {
        let mut constants = constants();
        constants.difficulty_constant_factor = 1u128 << 127;
        let contract = Bytes32::from([13; 32]);
        let mut point = NewSignagePointHarvester {
            challenge_hash: Bytes32::from([11; 32]),
            sp_hash: Bytes32::from([12; 32]),
            difficulty: u64::MAX,
            sub_slot_iters: constants.sub_slot_iters_starting,
            signage_point_index: 0,
            pool_difficulties: Vec::new(),
            filter_prefix_bits: 0,
            last_tx_height: 0,
        };
        let quality = Bytes32::from([14; 32]);
        assert!(!eligible(&constants, &point, Some(contract), quality).unwrap());
        point.pool_difficulties.push(PoolDifficulty {
            pool_contract_puzzle_hash: contract,
            difficulty: 0,
            sub_slot_iters: constants.sub_slot_iters_starting,
        });
        assert!(eligible(&constants, &point, Some(contract), quality).unwrap());
        assert!(!eligible(&constants, &point, None, quality).unwrap());
        point.difficulty = 0;
        assert!(eligible(&constants, &point, None, quality).unwrap());
    }

    #[test]
    fn k28_cuda_response_requires_one_exact_verified_chain() {
        let params = ProofParams::new(Bytes32::from([1; 32]), 28, 5, false).unwrap();
        let chain = Chain { fragments: [0; 16] };
        let challenge = Bytes32::from([2; 32]);
        assert!(
            decode_cuda_proof(
                &vec![b'x'; MAX_HELPER_OUTPUT + 1],
                &params,
                &chain,
                challenge
            )
            .is_err()
        );
        let line = format!(
            "quality={} proof={}\n",
            quality_hash(&chain.fragments, 5),
            "00".repeat(448)
        );
        assert!(decode_cuda_proof(line.as_bytes(), &params, &chain, challenge).is_err());
        assert!(
            decode_cuda_proof(
                format!("{line}{line}").as_bytes(),
                &params,
                &chain,
                challenge
            )
            .is_err()
        );
        assert!(decode_cuda_proof(b"quality=00 proof=00", &params, &chain, challenge).is_err());
    }

    #[tokio::test]
    async fn helper_streams_are_bounded_and_cancellation_is_signalled() {
        assert_eq!(read_bounded(&b"abc"[..], 3).await.unwrap(), b"abc");
        assert!(read_bounded(&b"abcd"[..], 3).await.is_err());
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let _guard = CancelOnDrop(cancelled.clone());
        }
        assert!(cancelled.load(Ordering::Acquire));
        wait_for_shutdown(&AtomicBool::new(false)).await;
    }

    #[test]
    fn k28_local_recovery_checks_cancellation_before_any_work() {
        let directory = tempfile::tempdir().unwrap();
        let (path, _, _) = fixture(directory.path(), 2, true);
        let mut disk = DiskHarvester::open(
            &path,
            constants().is_testnet,
            PlotLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let result = recover_local(
            &mut disk,
            &Chain { fragments: [0; 16] },
            Bytes32::from([1; 32]),
            Pos2Backend::Cpu,
            0,
            &AtomicBool::new(true),
        );
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Interrupted);
    }
}
