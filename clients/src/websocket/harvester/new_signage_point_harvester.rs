use async_trait::async_trait;
use dg_xch_core::blockchain::proof_of_space::{
    calculate_pos_challenge, passes_plot_filter, ProofBytes, ProofOfSpace,
};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality, calculate_sp_interval_iters,
};
use dg_xch_core::protocols::harvester::{NewProofOfSpace, NewSignagePointHarvester};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap, ProtocolMessageTypes};
use dg_xch_pos::verifier::proof_to_bytes;
use dg_xch_pos::PlotManagerAsync;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use hex::encode;
use log::{debug, error, info, trace, warn};
use std::io::{Cursor, Error};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct PlotCounts {
    og_passed: Arc<AtomicUsize>,
    og_total: Arc<AtomicUsize>,
    pool_total: Arc<AtomicUsize>,
    pool_passed: Arc<AtomicUsize>,
    compressed_passed: Arc<AtomicUsize>,
    compressed_total: Arc<AtomicUsize>,
}
pub struct NewSignagePointHarvesterHandle<T: PlotManagerAsync> {
    pub constants: &'static ConsensusConstants,
    pub plot_manager: Arc<RwLock<T>>,
    pub plots_ready: Arc<AtomicBool>,
}
#[async_trait]
impl<T: PlotManagerAsync + Send + Sync> MessageHandler for NewSignagePointHarvesterHandle<T> {
    #[allow(clippy::too_many_lines)]
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        if !self.plots_ready.load(Ordering::Relaxed) {
            info!("Plots Not Ready Yet, skipping");
            return Ok(());
        }
        let peer = peers.read().await.get(&peer_id).cloned();
        let protocol_version = if let Some(peer) = peer.as_ref() {
            *peer.protocol_version.read().await
        } else {
            ChiaProtocolVersion::default()
        };
        let mut cursor = Cursor::new(msg.data.clone());
        let harvester_point = NewSignagePointHarvester::from_bytes(&mut cursor, protocol_version)?;
        trace!("{:#?}", &harvester_point);
        let plot_counts = Arc::new(PlotCounts::default());
        let harvester_point = Arc::new(harvester_point);
        let constants = Arc::new(self.constants);
        let mut jobs = FuturesUnordered::new();
        self.plot_manager.read().await.plots().iter().map(|(path_info, plot_info)|{
            (path_info.clone(), plot_info.clone())
        }).for_each(|(path, plot_info)| {
            let data_arc = harvester_point.clone();
            let constants_arc = constants.clone();
            let plot_counts = plot_counts.clone();
            let mut responses = vec![];
            let plot_handle = timeout(Duration::from_secs(15), tokio::spawn(async move {
                let plot_id = plot_info.reader.header().id();
                let k = plot_info.reader.header().k();
                let memo = plot_info.reader.header().memo();
                let c_level = plot_info.reader.header().compression_level();
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
                        debug!("Plot: {} Qualities Found: {}", &path.file_name, qualities.len());
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
                        for (index, quality) in qualities {
                            let required_iters = calculate_iterations_quality(
                                constants_arc.difficulty_constant_factor,
                                quality,
                                k,
                                dif,
                                data_arc.sp_hash,
                            );
                            if let Ok(sp_interval_iters) =
                                calculate_sp_interval_iters(&constants_arc, sub_slot_iters)
                            {
                                if required_iters < sp_interval_iters {
                                    info!("Plot: {}, Passed Required Iterations, Loading Index: {}", path.file_name, index);
                                    match plot_info.reader.fetch_ordered_proof(index).await {
                                        Ok(proof) => {
                                            let proof_bytes = proof_to_bytes(&proof);
                                            debug!(
                                                "File: {:?} Plot ID: {}, challenge: {sp_challenge_hash}, Quality Str: {}, proof: {:?}",
                                                path,
                                                &plot_id,
                                                encode(quality.to_bytes(protocol_version)),
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
                                                },
                                                (is_partial, c_level)
                                            ));
                                        }
                                        Err(e) => {
                                            error!("Failed to read Proof: {e:?}");
                                        }
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
            }));
            jobs.push(plot_handle);
        });
        let proofs = AtomicU64::new(0);
        let nft_partials = AtomicU64::new(0);
        let compressed_partials = AtomicU64::new(0);
        while let Some(timeout_result) = jobs.next().await {
            match timeout_result {
                Ok(join_result) => match join_result {
                    Ok(read_result) => match read_result {
                        Ok((path, responses)) => {
                            if let Some(client) = peers.read().await.get(&peer_id).cloned() {
                                for (quality, proof, (is_partial, c_level)) in responses {
                                    let _ = client
                                        .websocket
                                        .write()
                                        .await
                                        .send(Message::Binary(
                                            ChiaMessage::new(
                                                ProtocolMessageTypes::NewProofOfSpace,
                                                protocol_version,
                                                &NewProofOfSpace {
                                                    challenge_hash: harvester_point.challenge_hash,
                                                    sp_hash: harvester_point.sp_hash,
                                                    plot_identifier: encode(
                                                        quality.to_bytes(protocol_version),
                                                    ) + path.file_name.as_str(),
                                                    proof,
                                                    signage_point_index: harvester_point
                                                        .signage_point_index,
                                                    include_source_signature_data: false,
                                                    farmer_reward_address_override: None,
                                                    fee_info: None,
                                                },
                                                None,
                                            )
                                            .to_bytes(protocol_version)
                                            .into(),
                                        ))
                                        .await;
                                    if is_partial {
                                        if c_level > 0 {
                                            compressed_partials.fetch_add(1, Ordering::Relaxed);
                                        } else {
                                            nft_partials.fetch_add(1, Ordering::Relaxed);
                                        }
                                    } else {
                                        proofs.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            } else {
                                error!("No Connection to send Proof");
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
        info!(
            "Passed Filter - OG: {}/{}. NFT: {}/{}. Compressed: {}/{}. Proofs Found: {}. Partials Found: NFT({}), Compressed({})",
            plot_counts.og_passed.load(Ordering::Relaxed),
            plot_counts.og_total.load(Ordering::Relaxed),
            plot_counts.pool_passed.load(Ordering::Relaxed),
            plot_counts.pool_total.load(Ordering::Relaxed),
            plot_counts.compressed_passed.load(Ordering::Relaxed),
            plot_counts.compressed_total.load(Ordering::Relaxed),
            proofs.load(Ordering::Relaxed),
            nft_partials.load(Ordering::Relaxed),
            compressed_partials.load(Ordering::Relaxed),
        );
        Ok(())
    }
}
