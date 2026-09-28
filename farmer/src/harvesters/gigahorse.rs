use super::FarmingKeys;
use super::discovery::{PlotFormat, PlotInventory};
use crate::farmer::PathInfo;
use crate::farmer::config::{GigahorseBackend, GigahorseHarvesterConfig};
use blst::min_pk::{PublicKey, SecretKey};
use dg_xch_core::blockchain::proof_of_space::{
    ProofBytes, ProofOfSpace, calculate_pos_challenge, generate_plot_public_key, passes_plot_filter,
};
use dg_xch_core::blockchain::sized_bytes::Bytes48;
use dg_xch_core::clvm::bls_bindings::sign_prepend;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality, calculate_sp_interval_iters,
};
use dg_xch_core::protocols::harvester::{
    NewProofOfSpace, NewSignagePointHarvester, RequestSignatures, RespondSignatures,
};
use dg_xch_core::utils::hash_256;
use dg_xch_keys::master_sk_to_local_sk;
use dg_xch_pos::gigahorse::{Gh3Header, Gh3Reader};
use dg_xch_pos::gigahorse_cpu::{
    C30Table5Entry, c30_quality_candidate, c30_quality_entries_in_buckets, finish_c30_proofs,
    reconstruct_c30_table5_checked,
};
use log::warn;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, mpsc};
use zeroize::Zeroizing;

struct FarmingPlot {
    path: PathInfo,
    header: Gh3Header,
    local_secret: SecretKey,
    farmer_public_key: Bytes48,
    pool_public_key: Bytes48,
    plot_public_key: PublicKey,
}

impl FarmingPlot {
    fn open(path: &Path, keys: &FarmingKeys) -> Result<Self, Error> {
        let header = Gh3Header::read(&mut File::open(path)?)?;
        if header.compression_parameters != [0, 8, 11] {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Only GigaHorse 3.0 C30 plots are supported",
            ));
        }
        let memo = header.decrypt_og_memo()?;
        let master_bytes = Zeroizing::new(memo.local_master_secret_key);
        let farmer_public_key = Bytes48::from(memo.farmer_public_key);
        let pool_public_key = Bytes48::from(memo.pool_public_key);
        if !keys.farmer_public_keys.contains(&farmer_public_key)
            || !keys.pool_public_keys.contains(&pool_public_key)
        {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "missing GigaHorse farming keys",
            ));
        }
        let master = SecretKey::from_bytes(master_bytes.as_ref())
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid local master key"))?;
        let local_secret = master_sk_to_local_sk(&master)?;
        let farmer = PublicKey::key_validate(farmer_public_key.as_ref())
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid farmer public key"))?;
        PublicKey::key_validate(pool_public_key.as_ref())
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid pool public key"))?;
        let plot_public_key = generate_plot_public_key(&local_secret.sk_to_pk(), &farmer, false)?;
        let mut identity = memo.pool_public_key.to_vec();
        identity.extend_from_slice(&plot_public_key.to_bytes());
        if hash_256(identity).as_ref() != header.plot_id {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "GigaHorse memo does not match plot ID",
            ));
        }
        Ok(Self {
            path: PathInfo::new(path.to_path_buf()),
            header,
            local_secret,
            farmer_public_key,
            pool_public_key,
            plot_public_key,
        })
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

enum ReconstructionBackend {
    Cpu,
    #[cfg(any(feature = "cuda", feature = "vulkan"))]
    Gpu {
        engine: std::sync::Mutex<dg_xch_pos::gigahorse_gpu::Engine>,
        memory_bytes: u64,
    },
}

impl ReconstructionBackend {
    fn new(config: &GigahorseHarvesterConfig) -> Result<Self, Error> {
        match config.backend {
            GigahorseBackend::Cpu => Ok(Self::Cpu),
            #[cfg(any(feature = "cuda", feature = "vulkan"))]
            backend => {
                let engine = match backend {
                    GigahorseBackend::Cuda => {
                        dg_xch_pos::gigahorse_gpu::Engine::cuda(config.device)?
                    }
                    GigahorseBackend::Vulkan => {
                        dg_xch_pos::gigahorse_gpu::Engine::vulkan(config.device)?
                    }
                    GigahorseBackend::Cpu => unreachable!(),
                };
                Ok(Self::Gpu {
                    engine: std::sync::Mutex::new(engine),
                    memory_bytes: config.memory_mib * 1024 * 1024,
                })
            }
            #[cfg(not(any(feature = "cuda", feature = "vulkan")))]
            _ => Err(Error::new(
                ErrorKind::Unsupported,
                "GigaHorse GPU support not compiled; enable cuda or vulkan",
            )),
        }
    }

    fn reconstruct(
        &self,
        plot_id: &[u8; 32],
        bitmap: &[u64],
        check: &(impl Fn() -> Result<(), Error> + Sync),
    ) -> Result<Vec<C30Table5Entry>, Error> {
        match self {
            Self::Cpu => reconstruct_c30_table5_checked(plot_id, bitmap, check),
            #[cfg(any(feature = "cuda", feature = "vulkan"))]
            Self::Gpu {
                engine,
                memory_bytes,
            } => engine
                .lock()
                .map_err(|_| Error::other("GigaHorse GPU lock poisoned"))?
                .reconstruct(plot_id, bitmap, *memory_bytes, check),
        }
    }

    fn finish(
        &self,
        entries: Vec<C30Table5Entry>,
        challenge: &[u8; 32],
        check: &(impl Fn() -> Result<(), Error> + Sync),
    ) -> Result<Vec<Vec<u8>>, Error> {
        check()?;
        match self {
            Self::Cpu => Ok(finish_c30_proofs(entries, challenge)),
            #[cfg(any(feature = "cuda", feature = "vulkan"))]
            Self::Gpu { engine, .. } => engine
                .lock()
                .map_err(|_| Error::other("GigaHorse GPU lock poisoned"))?
                .finish(entries, challenge, check),
        }
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub struct GigahorseHarvester {
    plots: RwLock<HashMap<String, Arc<FarmingPlot>>>,
    directories: Vec<PathBuf>,
    keys: Arc<FarmingKeys>,
    constants: ConsensusConstants,
    config: GigahorseHarvesterConfig,
    workers: Arc<rayon::ThreadPool>,
    slot: Arc<Semaphore>,
    running: Arc<AtomicBool>,
    reconstruction: Arc<ReconstructionBackend>,
}

impl GigahorseHarvester {
    pub async fn new(
        config: GigahorseHarvesterConfig,
        inherited_directories: &[PathBuf],
        keys: Arc<FarmingKeys>,
        constants: ConsensusConstants,
        running: Arc<AtomicBool>,
    ) -> Result<Arc<Self>, Error> {
        let directories = if config.plot_directories.is_empty() {
            inherited_directories.to_vec()
        } else {
            config.plot_directories.iter().map(PathBuf::from).collect()
        };
        let inventory = PlotInventory::scan(directories).await?;
        Self::new_from_paths(
            config,
            inherited_directories,
            keys,
            constants,
            running,
            inventory.paths(PlotFormat::Gigahorse),
        )
        .await
    }

    pub(crate) async fn new_from_paths(
        config: GigahorseHarvesterConfig,
        inherited_directories: &[PathBuf],
        keys: Arc<FarmingKeys>,
        constants: ConsensusConstants,
        running: Arc<AtomicBool>,
        paths: Vec<PathBuf>,
    ) -> Result<Arc<Self>, Error> {
        config.validate()?;
        let backend_config = config.clone();
        let reconstruction =
            tokio::task::spawn_blocking(move || ReconstructionBackend::new(&backend_config))
                .await
                .map_err(Error::other)??;
        let directories = if config.plot_directories.is_empty() {
            inherited_directories.to_vec()
        } else {
            config.plot_directories.iter().map(PathBuf::from).collect()
        };
        let workers = rayon::ThreadPoolBuilder::new()
            .num_threads(if config.backend == GigahorseBackend::Cpu {
                config.threads
            } else {
                1
            })
            .thread_name(|index| format!("gigahorse-{index}"))
            .build()
            .map_err(Error::other)?;
        let harvester = Arc::new(Self {
            plots: RwLock::new(HashMap::new()),
            directories,
            keys,
            constants,
            config,
            workers: Arc::new(workers),
            slot: Arc::new(Semaphore::new(1)),
            running,
            reconstruction: Arc::new(reconstruction),
        });
        harvester.refresh_paths(paths).await?;
        Ok(harvester)
    }

    pub async fn refresh(&self) -> Result<(), Error> {
        let inventory = PlotInventory::scan(self.directories.clone()).await?;
        self.refresh_paths(inventory.paths(PlotFormat::Gigahorse))
            .await
    }

    pub(crate) async fn refresh_paths(&self, paths: Vec<PathBuf>) -> Result<(), Error> {
        let keys = self.keys.clone();
        let running = self.running.clone();
        let plots = tokio::task::spawn_blocking(move || {
            let mut plots = HashMap::new();
            let mut identities = HashSet::new();
            for path in paths {
                if !running.load(Ordering::Relaxed) {
                    return Err(Error::new(ErrorKind::Interrupted, "farmer stopped"));
                }
                match FarmingPlot::open(&path, &keys) {
                    Ok(plot) => {
                        if identities.insert(plot.header.plot_id) {
                            plots.insert(plot.path.identifier().to_owned(), Arc::new(plot));
                        }
                    }
                    Err(error) => warn!("GigaHorse plot rejected {}: {error}", path.display()),
                }
            }
            Ok::<_, Error>(plots)
        })
        .await
        .map_err(Error::other)??;
        *self.plots.write().await = plots;
        Ok(())
    }

    pub async fn paths(&self) -> Vec<PathBuf> {
        self.plots
            .read()
            .await
            .values()
            .map(|plot| plot.path.path.clone())
            .collect()
    }

    pub async fn sign(
        &self,
        request: &RequestSignatures,
    ) -> Result<Option<RespondSignatures>, Error> {
        let identifier = request
            .plot_identifier
            .get(64..)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "malformed plot identifier"))?;
        let plots = self.plots.read().await;
        let Some(plot) = plots.get(identifier) else {
            return Ok(None);
        };
        if request.messages.len() != 2 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "OG signing requires two messages",
            ));
        }
        Ok(Some(RespondSignatures {
            plot_identifier: request.plot_identifier.clone(),
            challenge_hash: request.challenge_hash,
            sp_hash: request.sp_hash,
            local_pk: plot.local_secret.sk_to_pk().to_bytes().into(),
            farmer_pk: plot.farmer_public_key,
            message_signatures: request
                .messages
                .iter()
                .map(|message| {
                    (
                        *message,
                        sign_prepend(&plot.local_secret, message.as_ref(), &plot.plot_public_key)
                            .to_bytes()
                            .into(),
                    )
                })
                .collect(),
            include_source_signature_data: false,
            farmer_reward_address_override: None,
        }))
    }

    pub async fn farm(
        &self,
        point: Arc<NewSignagePointHarvester>,
        height: u32,
        proofs: mpsc::Sender<NewProofOfSpace>,
    ) -> Result<(), Error> {
        if !dg_xch_core::blockchain::proof_of_space::is_proof_version_active(
            0,
            point.last_tx_height,
            &self.constants,
        ) {
            return Ok(());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(cancelled.clone());
        let duration = Duration::from_millis(self.config.deadline_ms);
        let deadline = Instant::now() + duration;
        let operation = async {
            let permit = self
                .slot
                .clone()
                .acquire_owned()
                .await
                .map_err(Error::other)?;
            let plots: Vec<_> = self.plots.read().await.values().cloned().collect();
            let running = self.running.clone();
            let workers = self.workers.clone();
            let constants = self.constants;
            let reconstruction = self.reconstruction.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let check = || {
                    if cancelled.load(Ordering::Relaxed) || !running.load(Ordering::Relaxed) {
                        Err(Error::new(
                            ErrorKind::Interrupted,
                            "GigaHorse farming cancelled",
                        ))
                    } else if Instant::now() >= deadline {
                        Err(Error::new(
                            ErrorKind::TimedOut,
                            "GigaHorse signage deadline exceeded",
                        ))
                    } else {
                        Ok(())
                    }
                };
                workers.install(|| {
                    for plot in plots {
                        check()?;
                        if let Err(error) = farm_plot(
                            &plot,
                            &point,
                            height,
                            &constants,
                            &proofs,
                            &reconstruction,
                            &check,
                        ) {
                            check()?;
                            warn!(
                                "GigaHorse recovery failed for {}: {error}",
                                plot.path.path.display()
                            );
                        }
                    }
                    Ok(())
                })
            })
            .await
            .map_err(Error::other)?
        };
        tokio::time::timeout(duration, operation)
            .await
            .map_err(|_| Error::new(ErrorKind::TimedOut, "GigaHorse signage deadline exceeded"))?
    }
}

fn farm_plot(
    plot: &FarmingPlot,
    point: &NewSignagePointHarvester,
    height: u32,
    constants: &ConsensusConstants,
    proofs: &mpsc::Sender<NewProofOfSpace>,
    reconstruction: &ReconstructionBackend,
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<(), Error> {
    let plot_id = plot.header.plot_id.into();
    if !passes_plot_filter(
        point.filter_prefix_bits,
        plot_id,
        point.challenge_hash,
        point.sp_hash,
    ) {
        return Ok(());
    }
    let challenge = calculate_pos_challenge(plot_id, point.challenge_hash, point.sp_hash);
    let challenge_bytes = *AsRef::<[u8; 32]>::as_ref(&challenge);
    let interval = calculate_sp_interval_iters(constants, point.sub_slot_iters)?;
    let mut reader = Gh3Reader::new(File::open(&plot.path.path)?)?;
    if reader.header() != &plot.header {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "GigaHorse plot changed since discovery",
        ));
    }
    let mut cache: HashMap<u64, Vec<C30Table5Entry>> = HashMap::new();
    let mut emitted = HashSet::new();
    for index in reader.matching_f7_indices(&challenge_bytes)? {
        check()?;
        if cache.len() > 64 {
            cache.clear();
        }
        let quality_buckets = reader.c30_quality_bucket_indices(index, &challenge_bytes)?;
        for bucket in &quality_buckets {
            check()?;
            if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(*bucket) {
                let bitmap = reader.c30_bitmap(*bucket)?;
                entry.insert(reconstruction.reconstruct(&plot.header.plot_id, &bitmap, check)?);
            }
        }
        let eligible = c30_quality_entries_in_buckets(
            quality_buckets
                .iter()
                .map(|bucket| cache[bucket].as_slice()),
            &challenge_bytes,
            check,
        )?
        .into_iter()
        .any(|entry| {
            calculate_iterations_quality(
                constants.difficulty_constant_factor,
                c30_quality_candidate(entry, &challenge_bytes).into(),
                32,
                point.difficulty,
                point.sp_hash,
            ) < interval
        });
        if !eligible {
            continue;
        }
        let mut entries = Vec::new();
        for bucket in reader.c30_bucket_indices(index)? {
            check()?;
            if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(bucket) {
                let bitmap = reader.c30_bitmap(bucket)?;
                entry.insert(reconstruction.reconstruct(&plot.header.plot_id, &bitmap, check)?);
            }
            entries.extend_from_slice(&cache[&bucket]);
        }
        for proof_bytes in reconstruction.finish(entries, &challenge_bytes, check)? {
            check()?;
            if !emitted.insert(proof_bytes.clone()) {
                continue;
            }
            let proof = ProofOfSpace {
                challenge,
                pool_public_key: Some(plot.pool_public_key),
                pool_contract_puzzle_hash: None,
                plot_public_key: plot.plot_public_key.to_bytes().into(),
                size: 32,
                proof: ProofBytes::from(proof_bytes),
                version: 0,
                plot_index: 0,
                meta_group: 0,
                strength: 0,
            };
            let quality = dg_xch_pos::verify_and_get_quality_string_with_context(
                &proof,
                constants,
                point.challenge_hash,
                point.sp_hash,
                height,
                point.last_tx_height,
            )
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    "GigaHorse proof failed consensus verification",
                )
            })?;
            if calculate_iterations_quality(
                constants.difficulty_constant_factor,
                quality,
                32,
                point.difficulty,
                point.sp_hash,
            ) >= interval
            {
                continue;
            }
            proofs
                .try_send(NewProofOfSpace {
                    challenge_hash: point.challenge_hash,
                    sp_hash: point.sp_hash,
                    plot_identifier: format!("{}{}", hex::encode(quality), plot.path.identifier()),
                    proof,
                    signage_point_index: point.signage_point_index,
                    include_source_signature_data: false,
                    farmer_reward_address_override: None,
                    fee_info: None,
                })
                .map_err(Error::other)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::blockchain::proof_of_space::calculate_prefix_bits;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;

    #[test]
    fn gigahorse_configuration_round_trips() {
        use crate::farmer::config::HarvesterConfig;
        let config: HarvesterConfig = serde_json::from_str(
            r#"{"gigahorse":{"backend":"cuda","plot_directories":["/plots"]}}"#,
        )
        .unwrap();
        let backend = config.gigahorse.as_ref().unwrap();
        assert_eq!(backend.backend, GigahorseBackend::Cuda);
        assert_eq!(backend.plot_directories, ["/plots"]);
        assert!(config.custom_config.is_none());
        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["gigahorse"]["backend"], "cuda");
        assert_eq!(
            serde_json::from_value::<HarvesterConfig>(serialized).unwrap(),
            config
        );
    }

    #[test]
    fn accepts_legacy_configuration_and_validates_limits() {
        use crate::farmer::config::HarvesterConfig;
        let legacy: HarvesterConfig =
            serde_json::from_str(r#"{"druid_garden":{"plot_directories":["/plots"]}}"#).unwrap();
        let modern: HarvesterConfig =
            serde_json::from_str(r#"{"pos1":{"plot_directories":["/plots"]}}"#).unwrap();
        assert_eq!(legacy, modern);
        assert!(serde_json::to_value(&modern).unwrap().get("pos1").is_some());
        assert!(modern.gigahorse.is_none());
        let mut config = GigahorseHarvesterConfig::default();
        assert!(config.validate().is_ok());
        config.threads = 0;
        assert!(config.validate().is_err());
        config.threads = 2;
        config.deadline_ms = 0;
        assert!(config.validate().is_err());
        config.deadline_ms = 20_000;
        config.memory_mib = u64::MAX;
        assert!(config.validate().is_err());
        config.memory_mib = 12288;
        config.device = 1;
        assert!(config.validate().is_err());
        config.backend = GigahorseBackend::Cuda;
        assert!(config.validate().is_ok());
        config.backend = GigahorseBackend::Vulkan;
        assert!(config.validate().is_ok());
    }

    #[test]
    #[cfg(not(any(feature = "cuda", feature = "vulkan")))]
    fn explicit_gpu_selection_never_silently_falls_back_to_cpu() {
        for backend in [GigahorseBackend::Cuda, GigahorseBackend::Vulkan] {
            let config = GigahorseHarvesterConfig {
                backend,
                ..Default::default()
            };
            assert_eq!(
                ReconstructionBackend::new(&config).err().unwrap().kind(),
                ErrorKind::Unsupported
            );
        }
    }

    #[tokio::test]
    async fn queued_work_expires_without_releasing_an_active_workers_slot() {
        let backend = GigahorseHarvester::new(
            GigahorseHarvesterConfig {
                threads: 1,
                deadline_ms: 5,
                ..Default::default()
            },
            &[],
            Arc::new(FarmingKeys {
                farmer_public_keys: Vec::new(),
                pool_public_keys: Vec::new(),
                pool_contract_hashes: Vec::new(),
            }),
            ConsensusConstants::default(),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
        let permit = backend.slot.clone().acquire_owned().await.unwrap();
        let point = Arc::new(NewSignagePointHarvester {
            challenge_hash: [1; 32].into(),
            sp_hash: [2; 32].into(),
            difficulty: 1,
            sub_slot_iters: 64,
            signage_point_index: 0,
            pool_difficulties: Vec::new(),
            filter_prefix_bits: 9,
            last_tx_height: 0,
        });
        let (sender, _receiver) = mpsc::channel(1);
        assert_eq!(
            backend.farm(point, 0, sender).await.unwrap_err().kind(),
            ErrorKind::TimedOut
        );
        assert_eq!(backend.slot.available_permits(), 0);
        drop(permit);
        assert_eq!(backend.slot.available_permits(), 1);
        let cancelled = Arc::new(AtomicBool::new(false));
        drop(CancelOnDrop(cancelled.clone()));
        assert!(cancelled.load(Ordering::Relaxed));
    }

    #[tokio::test]
    #[ignore = "requires GH_C30_PLOT, release mode, several GiB of RAM and up to two minutes"]
    async fn farms_real_c30_signage_point_without_archives() {
        let path = PathBuf::from(std::env::var_os("GH_C30_PLOT").expect("set GH_C30_PLOT"));
        let memo = Gh3Header::read(&mut File::open(&path).unwrap())
            .unwrap()
            .decrypt_og_memo()
            .unwrap();
        let keys = Arc::new(FarmingKeys {
            farmer_public_keys: vec![memo.farmer_public_key.into()],
            pool_public_keys: vec![memo.pool_public_key.into()],
            pool_contract_hashes: Vec::new(),
        });
        let constants = ConsensusConstants::default();
        let selected_backend = match std::env::var("GH_TEST_BACKEND").as_deref() {
            Ok("cuda") => GigahorseBackend::Cuda,
            Ok("vulkan") => GigahorseBackend::Vulkan,
            Ok("cpu") | Err(_) => GigahorseBackend::Cpu,
            Ok(other) => panic!("unknown GigaHorse test backend: {other}"),
        };
        let backend = GigahorseHarvester::new(
            GigahorseHarvesterConfig {
                threads: 16,
                backend: selected_backend,
                deadline_ms: 120_000,
                ..Default::default()
            },
            &[path.parent().unwrap().to_path_buf()],
            keys,
            constants,
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
        let plot = backend
            .plots
            .read()
            .await
            .values()
            .find(|plot| plot.path.path == path)
            .unwrap()
            .clone();
        let mut reader = Gh3Reader::new(File::open(&path).unwrap()).unwrap();
        let point = (0_u64..100_000)
            .find_map(|counter| {
                let challenge_hash = Bytes32::from(hash_256(counter.to_le_bytes()));
                let sp_hash = Bytes32::from([42; 32]);
                let filter_prefix_bits = calculate_prefix_bits(&constants, 0);
                if !passes_plot_filter(
                    filter_prefix_bits,
                    plot.header.plot_id.into(),
                    challenge_hash,
                    sp_hash,
                ) {
                    return None;
                }
                let challenge =
                    calculate_pos_challenge(plot.header.plot_id.into(), challenge_hash, sp_hash);
                if reader
                    .matching_f7_indices(challenge.as_ref())
                    .unwrap()
                    .len()
                    != 1
                {
                    return None;
                }
                Some(NewSignagePointHarvester {
                    challenge_hash,
                    sp_hash,
                    difficulty: 0,
                    sub_slot_iters: constants.sub_slot_iters_starting,
                    signage_point_index: 0,
                    pool_difficulties: Vec::new(),
                    filter_prefix_bits,
                    last_tx_height: 0,
                })
            })
            .expect("find a signage point with one match");
        let point = Arc::new(point);
        let (sender, mut receiver) = mpsc::channel(16);
        backend.farm(point.clone(), 0, sender).await.unwrap();
        let proof = receiver
            .recv()
            .await
            .expect("a verified proof from the real plot");
        assert!(
            dg_xch_pos::verify_and_get_quality_string_with_context(
                &proof.proof,
                &constants,
                point.challenge_hash,
                point.sp_hash,
                0,
                0,
            )
            .is_some()
        );
        assert!(proof.plot_identifier.ends_with(plot.path.identifier()));
        let mut rejected = (*point).clone();
        rejected.difficulty = u64::MAX;
        let started = Instant::now();
        let (sender, mut receiver) = mpsc::channel(16);
        backend.farm(Arc::new(rejected), 0, sender).await.unwrap();
        assert!(receiver.recv().await.is_none());
        eprintln!(
            "GigaHorse rejected-candidate lookup: {:.3}s",
            started.elapsed().as_secs_f64()
        );
    }
}
