use crate::PROTOCOL_VERSION;
use crate::farmer::config::Config;
use crate::farmer::{PathInfo, PlotInfo};
use crate::harvesters::{FarmingKeys, Harvester, ProofHandler, SignatureHandler, count_plots};
use crate::utils::load_client_id;
use async_trait::async_trait;
use blst::min_pk::{PublicKey, SecretKey};
use dg_xch_core::blockchain::proof_of_space::{
    ProofBytes, ProofOfSpace, calculate_pos_challenge, generate_plot_public_key, passes_plot_filter,
};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::bls_bindings::sign_prepend;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality, calculate_sp_interval_iters,
};
use dg_xch_core::plots::PlotHeader;
use dg_xch_core::protocols::farmer::{FarmerMetrics, FarmerSharedState, PlotPassCounts};
use dg_xch_core::protocols::harvester::{
    NewProofOfSpace, NewSignagePointHarvester, RequestSignatures, RespondSignatures,
};
use dg_xch_keys::master_sk_to_local_sk;
use dg_xch_pos::plots::decompressor::DecompressorPool;
use dg_xch_pos::plots::disk_plot::DiskPlot;
use dg_xch_pos::plots::plot_reader::{PlotReader, read_all_plot_headers_async};
use dg_xch_pos::verifier::proof_to_bytes;
use dg_xch_serialize::ChiaSerialize;
use futures_util::stream::FuturesUnordered;
use futures_util::{StreamExt, TryStreamExt};
use hex::encode;
use log::{debug, error, info, warn};
use rand::random;
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::available_parallelism;
use std::time::{Duration, Instant, SystemTime};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock};
use tokio::time::timeout;

struct AbortTask<T>(tokio::task::JoinHandle<T>);

impl<T> std::future::Future for AbortTask<T> {
    type Output = Result<T, tokio::task::JoinError>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(context)
    }
}

impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct DruidGardenHarvester<T: Send + Sync + 'static> {
    pub plots: Arc<Mutex<HashMap<PathInfo, Arc<PlotInfo>>>>,
    pub plot_dirs: Arc<Vec<PathBuf>>,
    pub decompressor_pool: Arc<DecompressorPool>,
    pub plots_ready: Arc<AtomicBool>,
    pub farming_keys: Arc<FarmingKeys>,
    pub constants: ConsensusConstants,
    pub uuid: Bytes32,
    pub client_id: Bytes32,
    pub shared_state: Arc<FarmerSharedState<T>>,
}
#[async_trait]
impl<T: Send + Sync + 'static, C: Send + Sync + Clone + 'static>
    Harvester<T, DruidGardenHarvester<T>, C> for DruidGardenHarvester<T>
{
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
    ) -> Result<Arc<DruidGardenHarvester<T>>, Error> {
        let mut farmer_public_keys = vec![];
        let mut pool_public_keys = vec![];
        let client_id = load_client_id::<C>(config.clone()).await?;
        let config = config.read().await;
        for farmer_info in &config.farmer_info {
            let f_sk: SecretKey = farmer_info.farmer_secret_key.into();
            farmer_public_keys.push(f_sk.sk_to_pk().to_bytes().into());
            if let Some(pk) = farmer_info.pool_secret_key {
                let p_sk: SecretKey = pk.into();
                pool_public_keys.push(p_sk.sk_to_pk().to_bytes().into());
            }
        }
        let pool_contract_hashes = config
            .pool_info
            .iter()
            .map(|w| w.p2_singleton_puzzle_hash)
            .collect::<Vec<Bytes32>>();
        let mut sum = 0;
        let mut total_size = 0;
        let farming_keys = Arc::new(FarmingKeys {
            farmer_public_keys,
            pool_public_keys,
            pool_contract_hashes,
        });
        let dg_config = &config
            .harvester_configs
            .druid_garden
            .clone()
            .unwrap_or_default();
        for dir in &dg_config.plot_directories {
            if let Err(e) = count_plots(Path::new(&dir), &mut sum, &mut total_size).await {
                error!("Error Counting Plots: {e:?}")
            }
        }
        let harvester = DruidGardenHarvester::new(
            dg_config
                .plot_directories
                .iter()
                .map(|s| Path::new(s).to_path_buf())
                .collect(),
            farming_keys.clone(),
            shared_state.signal.clone(),
            config.constants()?,
            client_id,
            shared_state.clone(),
        )
        .await?;
        Ok(Arc::new(harvester))
    }
    async fn new_signage_point<O>(
        &self,
        signage_point: Arc<NewSignagePointHarvester>,
        proof_handle: O,
    ) -> Result<(), Error>
    where
        O: ProofHandler<T, DruidGardenHarvester<T>, C> + Sync + Send,
    {
        let start = self
            .shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .map(|m| m.signage_point_processing_latency.start_timer());
        let plot_counts = Arc::new(PlotPassCounts {
            og_passed: Arc::new(Default::default()),
            og_total: Arc::new(Default::default()),
            pool_total: Arc::new(Default::default()),
            pool_passed: Arc::new(Default::default()),
            compressed_passed: Arc::new(Default::default()),
            compressed_total: Arc::new(Default::default()),
            proofs_found: Arc::new(Default::default()),
            timestamp: OffsetDateTime::now_utc(),
        });
        let harvester_point = Arc::new(signage_point);
        let constants = Arc::new(self.constants);
        let mut jobs = FuturesUnordered::new();
        self.plots.lock().await.iter().map(|(path_info, plot_info)|{
            (path_info.clone(), plot_info.clone())
        }).for_each(|(path, plot_info)| {
            let data_arc = harvester_point.clone();
            let constants_arc = constants.clone();
            let plot_counts = plot_counts.clone();
            let metrics = self.shared_state.metrics.clone();
            let mut responses = vec![];
            let plot_handle = timeout(Duration::from_secs(20), AbortTask(tokio::spawn(async move {
                let (plot_id, k, memo, c_level) = match plot_info.reader.header() {
                    PlotHeader::V1(h) => (h.id, h.k, h.memo, 0),
                    PlotHeader::V2(h) => (h.id, h.k, h.memo, h.compression_level),
                    PlotHeader::GHv2_5(_) => {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "To Farm Gigahorse Plots you need to enable the Gigahorse Harvester",
                        ));
                    }
                };
                if plot_info.pool_public_key.is_some(){
                    plot_counts.og_total.fetch_add(1, Ordering::Relaxed);
                } else if c_level > 0 {
                    plot_counts.compressed_total.fetch_add(1, Ordering::Relaxed);
                } else {
                    plot_counts.pool_total.fetch_add(1, Ordering::Relaxed);
                }
                if passes_plot_filter(
                    data_arc.filter_prefix_bits,
                    plot_id,
                    data_arc.challenge_hash,
                    data_arc.sp_hash,
                ) {
                    if plot_info.pool_public_key.is_some() {
                        plot_counts.og_passed.fetch_add(1, Ordering::Relaxed);
                    } else if c_level > 0 {
                        plot_counts.compressed_passed.fetch_add(1, Ordering::Relaxed);
                    } else {
                        plot_counts.pool_passed.fetch_add(1, Ordering::Relaxed);
                    }
                    let sp_challenge_hash = calculate_pos_challenge(
                        plot_id,
                        data_arc.challenge_hash,
                        data_arc.sp_hash,
                    );
                    debug!("Starting Search for challenge {sp_challenge_hash} in plot {}", path.file_name);
                    let qualities = match plot_info
                        .reader
                        .fetch_qualities_for_challenge(sp_challenge_hash.as_ref()).await {
                        Ok(qualities) => {
                            qualities
                        }
                        Err(e) => {
                            debug!("Plot({:?}) - Error for Hash: {}", path.file_name, sp_challenge_hash);
                            return Err(e);
                        }
                    };
                    if !qualities.is_empty() {
                        debug!("Plot: {} Qualities Found: {}", path.file_name, qualities.len());
                        let mut dif = data_arc.difficulty;
                        let mut sub_slot_iters = data_arc.sub_slot_iters;
                        let mut is_partial = false;
                        if let Some(pool_contract_puzzle_hash) =
                            &memo.pool_contract_puzzle_hash
                        {
                            if let Some(p_dif) = data_arc.pool_difficulties.iter().find(|p| {
                                p.pool_contract_puzzle_hash == *pool_contract_puzzle_hash
                            }) {
                                debug!("Setting Difficulty for pool: {dif}");
                                dif = p_dif.difficulty;
                                sub_slot_iters = p_dif.sub_slot_iters;
                                is_partial = true;
                            } else if memo.pool_contract_puzzle_hash.is_some() {
                                warn!("Failed to find Pool Contract Difficulties for PH: {pool_contract_puzzle_hash} ");
                            }
                        }
                        for (index, quality) in qualities.into_iter() {
                            let start = metrics
                                .read()
                                .await
                                .as_ref()
                                .map(|m| m.qualities_latency
                                    .start_timer());
                            let required_iters = calculate_iterations_quality(
                                constants_arc.difficulty_constant_factor,
                                quality,
                                k,
                                dif,
                                data_arc.sp_hash,
                            );
                            if let Some(start) = start {
                                start.stop_and_record();
                            }
                            if let Ok(sp_interval_iters) =
                                calculate_sp_interval_iters(&constants_arc, sub_slot_iters)
                            {
                                if required_iters < sp_interval_iters {
                                    let start = metrics
                                        .read()
                                        .await
                                        .as_ref()
                                        .map(|m| m.proof_latency
                                            .start_timer());
                                    match plot_info.reader.fetch_ordered_proof(index).await {
                                        Ok(proof) => {
                                            let proof_bytes = proof_to_bytes(&proof);
                                            debug!(
                                                "File: {:?} Plot ID: {}, challenge: {sp_challenge_hash}, Quality Str: {}, proof_xs: {}",
                                                path,
                                                plot_id,
                                                encode(quality.to_bytes(PROTOCOL_VERSION)?),
                                                encode(&proof_bytes)
                                            );
                                            responses.push((
                                                quality,
                                                ProofOfSpace {
                                                    challenge: sp_challenge_hash,
                                                    pool_contract_puzzle_hash: plot_info
                                                        .pool_contract_puzzle_hash,
                                                    plot_public_key: plot_info
                                                        .plot_public_key,
                                                    pool_public_key: plot_info
                                                        .pool_public_key,
                                                    proof: ProofBytes::from(proof_bytes),
                                                    size: k,
                                                    version: 0,
                                                    plot_index: 0,
                                                    meta_group: 0,
                                                    strength: 0,
                                                },
                                                (is_partial, c_level)
                                            ));
                                        }
                                        Err(e) => {
                                            error!("Failed to read Proof: {e:?}");
                                        }
                                    }
                                    if let Some(start) = start {
                                        start.stop_and_record();
                                    }
                                } else {
                                    debug!(
                                        "Not Enough Iterations: {required_iters} > {sp_interval_iters}"
                                    );
                                }
                            }
                        }
                    }
                }
                Ok((path.clone(), responses))
            })));
            jobs.push(plot_handle);
        });
        let nft_partials = AtomicU64::new(0);
        let compressed_partials = AtomicU64::new(0);
        while let Some(timeout_result) = jobs.next().await {
            match timeout_result {
                Ok(join_result) => match join_result {
                    Ok(read_result) => match read_result {
                        Ok((path, responses)) => {
                            for (quality, proof, (is_partial, c_level)) in responses {
                                if let Err(e) = proof_handle
                                    .handle_proof(NewProofOfSpace {
                                        challenge_hash: harvester_point.challenge_hash,
                                        sp_hash: harvester_point.sp_hash,
                                        plot_identifier: encode(
                                            quality.to_bytes(PROTOCOL_VERSION)?,
                                        ) + path.identifier(),
                                        proof,
                                        signage_point_index: harvester_point.signage_point_index,
                                        include_source_signature_data: false,
                                        farmer_reward_address_override: None,
                                        fee_info: None,
                                    })
                                    .await
                                {
                                    error!("Failed to send proof to handler: {e:?}");
                                } else if is_partial {
                                    if c_level > 0 {
                                        compressed_partials.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        nft_partials.fetch_add(1, Ordering::Relaxed);
                                    }
                                } else {
                                    plot_counts.proofs_found.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Failed to read plot: {e:?}");
                        }
                    },
                    Err(e) => {
                        error!("Failed to join reader thread: {e:?}");
                    }
                },
                Err(e) => {
                    error!("Failed to read qualities due to Timeout: {e:?}");
                }
            }
        }
        let finished = if let Some(start) = start {
            start.stop_and_record()
        } else {
            0.0
        };
        debug!(
            "Finished Processing SP({}/{}) in {:.3} seconds",
            harvester_point.signage_point_index, harvester_point.sp_hash, finished
        );
        info!(
            "Index: {}, Passed Filter - OG: {}/{}. NFT: {}/{}. Compressed: {}/{}. Proofs Found: {}. Partials Found: NFT({}), Compressed({}), Took: {:.3} seconds",
            harvester_point.signage_point_index,
            plot_counts.og_passed.load(Ordering::Relaxed),
            plot_counts.og_total.load(Ordering::Relaxed),
            plot_counts.pool_passed.load(Ordering::Relaxed),
            plot_counts.pool_total.load(Ordering::Relaxed),
            plot_counts.compressed_passed.load(Ordering::Relaxed),
            plot_counts.compressed_total.load(Ordering::Relaxed),
            plot_counts.proofs_found.load(Ordering::Relaxed),
            nft_partials.load(Ordering::Relaxed),
            compressed_partials.load(Ordering::Relaxed),
            finished
        );
        self.shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .inspect(|m| {
                m.total_proofs_found
                    .inc_by(plot_counts.proofs_found.load(Ordering::Relaxed));
            });
        self.shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .inspect(|m| {
                m.last_proofs_found
                    .set(plot_counts.proofs_found.load(Ordering::Relaxed))
            });
        let total_partials =
            nft_partials.load(Ordering::Relaxed) + compressed_partials.load(Ordering::Relaxed);
        self.shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .inspect(|m| m.total_partials_found.inc_by(total_partials));
        self.shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .inspect(|m| m.last_partials_found.set(total_partials));
        let total_passed = plot_counts.og_passed.load(Ordering::Relaxed)
            + plot_counts.pool_passed.load(Ordering::Relaxed)
            + plot_counts.compressed_passed.load(Ordering::Relaxed);
        self.shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .inspect(|m| m.total_passed_filter.inc_by(total_passed));
        self.shared_state
            .metrics
            .read()
            .await
            .as_ref()
            .inspect(|m| m.last_passed_filter.set(total_passed));
        self.shared_state.recent_plot_stats.write().await.insert(
            (harvester_point.sp_hash, harvester_point.challenge_hash),
            plot_counts.as_ref().into(),
        );
        Ok(())
    }

    async fn request_signatures<H>(
        &self,
        request_signatures: RequestSignatures,
        response_handle: H,
    ) -> Result<(), Error>
    where
        H: SignatureHandler<T, DruidGardenHarvester<T>, C> + Sync + Send,
    {
        let file_name = request_signatures
            .plot_identifier
            .get(64..)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "malformed plot identifier"))?;
        let memo = match self.plots.lock().await.get(file_name) {
            None => {
                error!("Failed to find plot info for plot: {file_name}");
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!("Failed to find plot info for plot: {file_name}"),
                ));
            }
            Some(info) => match info.reader.header() {
                PlotHeader::V1(h) => h.memo,
                PlotHeader::V2(h) => h.memo,
                PlotHeader::GHv2_5(_) => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!(
                            "To Farm Gigahorse Plots you need to enable the Gigahorse Harvester: {file_name}"
                        ),
                    ));
                }
            },
        };
        let local_master_secret = SecretKey::from_bytes(memo.local_master_secret_key.as_ref())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
        let local_sk = master_sk_to_local_sk(&local_master_secret)?;
        let agg_pk = generate_plot_public_key(
            &local_sk.sk_to_pk(),
            &PublicKey::from_bytes(memo.farmer_public_key.as_ref())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?,
            memo.pool_contract_puzzle_hash.is_some(),
        )?;
        let mut message_signatures = vec![];
        for msg in request_signatures.messages {
            let sig = sign_prepend(&local_sk, msg.as_ref(), &agg_pk);
            let sig_bytes = sig.to_bytes().into();
            message_signatures.push((msg, sig_bytes));
        }
        response_handle
            .handle_signature(RespondSignatures {
                plot_identifier: request_signatures.plot_identifier,
                challenge_hash: request_signatures.challenge_hash,
                sp_hash: request_signatures.sp_hash,
                local_pk: local_sk.sk_to_pk().to_bytes().into(),
                farmer_pk: memo.farmer_public_key,
                message_signatures,
                include_source_signature_data: false,
                farmer_reward_address_override: None,
            })
            .await
    }

    fn uuid(&self) -> Bytes32 {
        self.uuid
    }
}

impl<T: Send + Sync + 'static> DruidGardenHarvester<T> {
    pub async fn new(
        plot_dirs: Vec<PathBuf>,
        farming_keys: Arc<FarmingKeys>,
        shutdown_signal: Arc<AtomicBool>,
        constants: ConsensusConstants,
        client_id: Bytes32,
        shared_state: Arc<FarmerSharedState<T>>,
    ) -> Result<Self, Error> {
        let decompressor_pool = Arc::new(DecompressorPool::new(
            1,
            available_parallelism().map(|u| u.get()).unwrap_or(4) as u8,
        ));
        let plot_dirs = Arc::new(plot_dirs);
        let plots = Arc::new(Mutex::new(
            load_plots(
                plot_dirs.clone(),
                &farming_keys.farmer_public_keys,
                &farming_keys.pool_public_keys,
                &farming_keys.pool_contract_hashes,
                vec![],
                decompressor_pool.clone(),
                shared_state.metrics.clone(),
            )
            .await?,
        ));
        let plot_sync_mutex = plots.clone();
        let plot_sync_dirs = plot_dirs.clone();
        let plot_sync_farming_keys = farming_keys.clone();
        let plot_sync_decompressor_pool = decompressor_pool.clone();
        let plot_sync_shared_state = shared_state.clone();
        let _plot_sync = tokio::spawn(async move {
            let mut last_sync = Instant::now();
            loop {
                if !shutdown_signal.load(Ordering::Relaxed) {
                    break;
                }
                if last_sync.elapsed() > Duration::from_secs(30) {
                    let existing_plot_paths: Arc<Vec<PathBuf>> = Arc::new(
                        plot_sync_mutex
                            .lock()
                            .await
                            .keys()
                            .map(|info| info.path.clone())
                            .collect(),
                    );
                    match load_plots(
                        plot_sync_dirs.clone(),
                        &plot_sync_farming_keys.farmer_public_keys,
                        &plot_sync_farming_keys.pool_public_keys,
                        &plot_sync_farming_keys.pool_contract_hashes,
                        existing_plot_paths.as_ref().clone(),
                        plot_sync_decompressor_pool.clone(),
                        plot_sync_shared_state.metrics.clone(),
                    )
                    .await
                    {
                        Ok(plots) => {
                            plot_sync_mutex.lock().await.extend(plots);
                            last_sync = Instant::now();
                        }
                        Err(e) => {
                            error!("Failed to load plots: {e:?}");
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        Ok(Self {
            farming_keys,
            plot_dirs,
            plots,
            plots_ready: Default::default(),
            decompressor_pool,
            constants,
            client_id,
            uuid: Bytes32::from(random::<[u8; 32]>()),
            shared_state,
        })
    }
}

async fn load_plots(
    plot_dirs: Arc<Vec<PathBuf>>,
    farmer_public_keys: &[Bytes48],
    pool_public_keys: &[Bytes48],
    pool_contract_hashes: &[Bytes32],
    existing_plot_paths: Vec<PathBuf>,
    decompressor_pool: Arc<DecompressorPool>,
    metrics: Arc<RwLock<Option<FarmerMetrics>>>,
) -> Result<HashMap<PathInfo, Arc<PlotInfo>>, Error> {
    debug!("Started Loading Plots");
    if farmer_public_keys.is_empty() {
        error!("No Public Keys Available");
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("Keys not available: {farmer_public_keys:?}, {pool_public_keys:?}"),
        ));
    }
    let farmer_public_keys = Arc::new(farmer_public_keys.to_vec());
    let pool_public_keys = Arc::new(pool_public_keys.to_vec());
    let pool_contract_hashes = Arc::new(pool_contract_hashes.to_vec());
    let existing_paths: Arc<Vec<PathBuf>> = Arc::new(existing_plot_paths);
    let futures = FuturesUnordered::new();
    for dir in plot_dirs.iter() {
        let farmer_public_keys = farmer_public_keys.clone();
        let pool_public_keys = pool_public_keys.clone();
        let pool_contract_hashes = pool_contract_hashes.clone();
        let existing_paths = existing_paths.clone();
        let decompressor_pool = decompressor_pool.clone();
        let metrics = metrics.clone();
        let dir = dir.clone();
        debug!("Validating Plot Directory: {:?}", dir);
        futures.push(timeout(
            Duration::from_secs(30),
            tokio::spawn(async move {
                match read_all_plot_headers_async(
                    &dir,
                    existing_paths
                        .iter()
                        .map(|p| p.as_path())
                        .collect::<Vec<&Path>>()
                        .as_slice(),
                )
                .await
                {
                    Ok((headers, failed)) => {
                        debug!(
                            "Plot Headers Processed: {}, Failed: {}",
                            headers.len(),
                            failed.len()
                        );
                        let mut results = vec![];
                        let mut missing_keys = HashSet::new();
                        for (path, header) in headers.into_iter() {
                            let (
                                local_master_secret_key,
                                farmer_public_key,
                                pool_contract_puzzle_hash,
                                pool_public_key,
                            ) = match load_headers(
                                &header,
                                farmer_public_keys.as_ref(),
                                pool_public_keys.as_ref(),
                                &pool_contract_hashes,
                            )
                            .await
                            {
                                Ok(headers) => headers,
                                Err(e) => {
                                    error!("Error for Plot: {path:?}, {e:?}");
                                    missing_keys.insert(path);
                                    continue;
                                }
                            };
                            let plot_load = metrics
                                .read()
                                .await
                                .as_ref()
                                .map(|v| v.plot_load_latency.start_timer());
                            let plot_file = match DiskPlot::new(&path).await {
                                Ok(plot_file) => plot_file,
                                Err(e) => {
                                    error!("Failed to load plot file {:?}: {:?}", path, e);
                                    results.push(Err(path));
                                    continue;
                                }
                            };
                            if let Some(v) = plot_load {
                                v.stop_and_record();
                            }
                            match PlotReader::new(
                                plot_file,
                                Some(decompressor_pool.clone()),
                                Some(decompressor_pool.clone()),
                            )
                            .await
                            {
                                Ok(reader) => {
                                    let local_master_secret = local_master_secret_key.into();
                                    let (size, modified) = tokio::fs::metadata(&path)
                                        .await
                                        .map(|me| {
                                            (me.len(), me.modified().unwrap_or(SystemTime::now()))
                                        })
                                        .unwrap_or_else(|_| (0, SystemTime::now()));
                                    let local_sk = match master_sk_to_local_sk(&local_master_secret)
                                    {
                                        Ok(key) => key,
                                        Err(e) => {
                                            error!("Failed to load local secret key: {e:?}");
                                            results.push(Err(path));
                                            continue;
                                        }
                                    };
                                    match generate_plot_public_key(
                                        &local_sk.sk_to_pk(),
                                        &farmer_public_key.into(),
                                        pool_contract_puzzle_hash.is_some(),
                                    ) {
                                        Ok(plot_public_key) => {
                                            let file_name = path
                                                .file_name()
                                                .map(|s| s.to_str().unwrap_or_default())
                                                .unwrap_or_default()
                                                .to_string();
                                            if file_name.is_empty() {
                                                error!(
                                                    "Failed to Load file_name for plot {:?}",
                                                    path
                                                );
                                                results.push(Err(path));
                                            } else {
                                                results.push(Ok((
                                                    PathInfo::new(path),
                                                    PlotInfo {
                                                        reader,
                                                        pool_public_key,
                                                        pool_contract_puzzle_hash,
                                                        plot_public_key: plot_public_key
                                                            .to_bytes()
                                                            .into(),
                                                        file_size: size,
                                                        time_modified: modified
                                                            .duration_since(SystemTime::UNIX_EPOCH)
                                                            .map(|d| d.as_secs())
                                                            .unwrap_or_default(),
                                                    },
                                                )));
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to create plot public key: {e:?}");
                                            results.push(Err(path));
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to create disk prover: {e:?}");
                                    results.push(Err(path));
                                }
                            }
                        }
                        Some((results, failed, missing_keys))
                    }
                    Err(e) => {
                        error!("Failed to validate plot dir: {dir:?}, {e:?}");
                        None
                    }
                }
            }),
        ));
    }
    let mut stream = futures.into_stream();
    let mut plots = HashMap::new();
    let mut failed_count = 0;
    let mut missing_keys_count = 0;
    while let Some(join_handle) = stream.next().await {
        match join_handle {
            Ok(Ok(o)) => {
                if let Some((results, failed, missing_keys)) = o {
                    failed_count += failed.len();
                    missing_keys_count += missing_keys.len();
                    for result in results {
                        match result {
                            Ok((k, v)) => {
                                plots.insert(k, Arc::new(v));
                            }
                            Err(e) => {
                                error!("Failed to read plot: {e:?}");
                            }
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                error!("Failed to Join plot reader thread: {e:?}");
            }
            Err(e) => {
                error!("Timeout in plot reader thread: {e:?}");
            }
        }
    }
    let mut og_count = 0;
    let mut pool_count = 0;
    let mut compressed_count = 0;
    for info in plots.values() {
        if info.reader.compression_level() > 0 {
            compressed_count += 1;
        } else if info.pool_contract_puzzle_hash.is_some() {
            pool_count += 1;
        } else if info.pool_public_key.is_some() {
            og_count += 1;
        }
    }
    info!(
        "Loaded {og_count} og plots, {pool_count} pooling plots and {compressed_count} compressed plots, failed to load {failed_count}, missing keys for {missing_keys_count}"
    );
    Ok(plots)
}

async fn load_headers(
    header: &PlotHeader,
    farmer_public_keys: &[Bytes48],
    pool_public_keys: &[Bytes48],
    pool_contract_hashes: &[Bytes32],
) -> Result<(Bytes32, Bytes48, Option<Bytes32>, Option<Bytes48>), Error> {
    let memo = match &header {
        PlotHeader::V1(header) => &header.memo,
        PlotHeader::V2(header) => &header.memo,
        PlotHeader::GHv2_5(_) => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Use the Gigahorse Harvester to Farm Gigahorse Plots",
            ));
        }
    };
    if let Some(key) = &memo.pool_public_key
        && !pool_public_keys.contains(key)
    {
        return Err(Error::new(
            ErrorKind::NotFound,
            "Missing Pool key in provided keys",
        ));
    } else if let Some(pool_contract_hash) = &memo.pool_contract_puzzle_hash
        && !pool_contract_hashes.contains(pool_contract_hash)
    {
        return Err(Error::new(
            ErrorKind::NotFound,
            "Missing pool contract address in provided puzzle_hashes",
        ));
    }
    if !farmer_public_keys.contains(&memo.farmer_public_key) {
        return Err(Error::new(
            ErrorKind::NotFound,
            "Missing Farmer key in provided keys",
        ));
    }
    Ok((
        memo.local_master_secret_key,
        memo.farmer_public_key,
        memo.pool_contract_puzzle_hash,
        memo.pool_public_key,
    ))
}
