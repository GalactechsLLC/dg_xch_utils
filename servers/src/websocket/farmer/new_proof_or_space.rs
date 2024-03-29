use crate::websocket::farmer::{update_pool_farmer_info, FarmerServerConfig};
use async_trait::async_trait;
use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use blst::BLST_ERROR;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_clients::websocket::oneshot;
use dg_xch_core::blockchain::proof_of_space::{generate_plot_public_key, generate_taproot_sk};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use dg_xch_core::clvm::bls_bindings::{sign, sign_prepend, AUG_SCHEME_DST};
use dg_xch_core::consensus::constants::CONSENSUS_CONSTANTS_MAP;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality, calculate_sp_interval_iters,
};
#[cfg(feature = "metrics")]
use dg_xch_core::protocols::farmer::FarmerMetrics;
use dg_xch_core::protocols::farmer::{
    FarmerIdentifier, FarmerPoolState, NewSignagePoint, ProofsMap,
};
use dg_xch_core::protocols::harvester::{
    NewProofOfSpace, RequestSignatures, RespondSignatures, SignatureRequestSourceData,
    SigningDataKind,
};
use dg_xch_core::protocols::pool::{
    get_current_authentication_token, PoolErrorCode, PostPartialPayload, PostPartialRequest,
};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap, ProtocolMessageTypes};
use dg_xch_pos::verify_and_get_quality_string;
use dg_xch_serialize::hash_256;
use dg_xch_serialize::ChiaSerialize;
use hyper_tungstenite::tungstenite::Message;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

static ONE_SHOT_COUNTER: AtomicU16 = AtomicU16::new(0);

pub struct NewProofOfSpaceHandle<T: PoolClient + Sized + Sync + Send + 'static> {
    pub pool_client: Arc<T>,
    pub signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
    pub proofs_of_space: ProofsMap,
    pub cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
    pub farmer_private_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub owner_secret_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
    pub config: Arc<FarmerServerConfig>,
    pub headers: Arc<HashMap<String, String>>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<Mutex<Option<FarmerMetrics>>>,
}
#[async_trait]
impl<T: PoolClient + Sized + Sync + Send + 'static> MessageHandler for NewProofOfSpaceHandle<T> {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        let exists;
        {
            exists = peers.lock().await.get(&peer_id).is_some();
        }
        if exists {
            let mut cursor = Cursor::new(&msg.data);
            let new_pos = NewProofOfSpace::from_bytes(&mut cursor)?;
            if let Some(sps) = self.signage_points.lock().await.get(&new_pos.sp_hash) {
                let constants = CONSENSUS_CONSTANTS_MAP
                    .get(&self.config.network)
                    .cloned()
                    .unwrap_or_default();
                for sp in sps {
                    if let Some(qs) = verify_and_get_quality_string(
                        &new_pos.proof,
                        &constants,
                        &new_pos.challenge_hash,
                        &new_pos.sp_hash,
                        sp.peak_height,
                    ) {
                        let required_iters = calculate_iterations_quality(
                            constants.difficulty_constant_factor,
                            &qs,
                            new_pos.proof.size,
                            sp.difficulty,
                            &new_pos.sp_hash,
                        );
                        if required_iters
                            < calculate_sp_interval_iters(&constants, sp.sub_slot_iters)?
                        {
                            let sp_src_data = {
                                if new_pos.include_source_signature_data
                                    || new_pos.farmer_reward_address_override.is_some()
                                {
                                    if let Some(sp_data) = sp.sp_source_data.as_ref() {
                                        let (cc, rc) = if let Some(vdf) = sp_data.vdf_data.as_ref()
                                        {
                                            (
                                                SignatureRequestSourceData {
                                                    kind: SigningDataKind::ChallengeChainVdf,
                                                    data: vdf.cc_vdf.to_bytes(),
                                                },
                                                SignatureRequestSourceData {
                                                    kind: SigningDataKind::RewardChainVdf,
                                                    data: vdf.rc_vdf.to_bytes(),
                                                },
                                            )
                                        } else if let Some(vdf) = sp_data.sub_slot_data.as_ref() {
                                            (
                                                SignatureRequestSourceData {
                                                    kind: SigningDataKind::ChallengeChainSubSlot,
                                                    data: vdf.cc_sub_slot.to_bytes(),
                                                },
                                                SignatureRequestSourceData {
                                                    kind: SigningDataKind::RewardChainSubSlot,
                                                    data: vdf.rc_sub_slot.to_bytes(),
                                                },
                                            )
                                        } else {
                                            return Err(Error::new(ErrorKind::InvalidInput, "Source Signature Did not contain any data, Cannot Sign Proof"));
                                        };
                                        Some(vec![Some(cc), Some(rc)])
                                    } else {
                                        return Err(Error::new(ErrorKind::InvalidInput, "Source Signature Data Request But was Null, Cannot Sign Proof"));
                                    }
                                } else {
                                    None
                                }
                            };
                            let request = RequestSignatures {
                                plot_identifier: new_pos.plot_identifier.clone(),
                                challenge_hash: new_pos.challenge_hash,
                                sp_hash: new_pos.sp_hash,
                                messages: vec![sp.challenge_chain_sp, sp.reward_chain_sp],
                                message_data: sp_src_data,
                                rc_block_unfinished: None,
                            };
                            let mut farmer_pos = self.proofs_of_space.lock().await;
                            if farmer_pos.get(&new_pos.sp_hash).is_none() {
                                farmer_pos.insert(new_pos.sp_hash, vec![]);
                            }
                            farmer_pos
                                .get_mut(&new_pos.sp_hash)
                                .expect("Should not happen, item created above")
                                .push((new_pos.plot_identifier.clone(), new_pos.proof.clone()));
                            self.cache_time
                                .lock()
                                .await
                                .insert(new_pos.sp_hash, Instant::now());
                            self.quality_to_identifiers.lock().await.insert(
                                qs,
                                FarmerIdentifier {
                                    plot_identifier: new_pos.plot_identifier.clone(),
                                    challenge_hash: new_pos.challenge_hash,
                                    sp_hash: new_pos.sp_hash,
                                    peer_node_id: *peer_id,
                                },
                            );
                            self.cache_time.lock().await.insert(qs, Instant::now());
                            if let Some(p) = peers.lock().await.get(&peer_id).cloned() {
                                let _ = p
                                    .websocket
                                    .lock()
                                    .await
                                    .send(Message::Binary(
                                        ChiaMessage::new(
                                            ProtocolMessageTypes::RequestSignatures,
                                            &request,
                                            None,
                                        )
                                        .to_bytes(),
                                    ))
                                    .await;
                            }
                        }
                        if let Some(p2_singleton_puzzle_hash) =
                            &new_pos.proof.pool_contract_puzzle_hash
                        {
                            if let Some(pool_state) = self
                                .pool_state
                                .lock()
                                .await
                                .get_mut(p2_singleton_puzzle_hash)
                            {
                                if let Some(pool_config) = pool_state.pool_config.clone() {
                                    let (pool_url, launcher_id) =
                                        (pool_config.pool_url.as_str(), pool_config.launcher_id);
                                    if pool_url.is_empty() {
                                        return Ok(());
                                    }
                                    if let Some(pool_dif) = pool_state.current_difficulty {
                                        let required_iters = calculate_iterations_quality(
                                            constants.difficulty_constant_factor,
                                            &qs,
                                            new_pos.proof.size,
                                            pool_dif,
                                            &new_pos.sp_hash,
                                        );
                                        if required_iters
                                            >= calculate_sp_interval_iters(
                                                &constants,
                                                constants.pool_sub_slot_iters,
                                            )?
                                        {
                                            debug!(
                                                "Proof of space not good enough for pool {}: {:?}",
                                                pool_url, pool_state.current_difficulty
                                            );
                                            return Ok(());
                                        }
                                        if let Some(auth_token_timeout) =
                                            pool_state.authentication_token_timeout
                                        {
                                            let is_eos = new_pos.signage_point_index == 0;
                                            let payload = PostPartialPayload {
                                                launcher_id,
                                                authentication_token:
                                                    get_current_authentication_token(
                                                        auth_token_timeout,
                                                    ),
                                                proof_of_space: new_pos.proof.clone(),
                                                sp_hash: new_pos.sp_hash,
                                                end_of_sub_slot: is_eos,
                                                harvester_id: *peer_id,
                                            };
                                            let to_sign = hash_256(payload.to_bytes());
                                            let sp_src_data = {
                                                if new_pos.include_source_signature_data
                                                    || new_pos
                                                        .farmer_reward_address_override
                                                        .is_some()
                                                {
                                                    Some(vec![Some(SignatureRequestSourceData {
                                                        kind: SigningDataKind::Partial,
                                                        data: payload.to_bytes(),
                                                    })])
                                                } else {
                                                    None
                                                }
                                            };
                                            let request = RequestSignatures {
                                                plot_identifier: new_pos.plot_identifier.clone(),
                                                challenge_hash: new_pos.challenge_hash,
                                                sp_hash: new_pos.sp_hash,
                                                messages: vec![Bytes32::new(&to_sign)],
                                                message_data: sp_src_data,
                                                rc_block_unfinished: None,
                                            };
                                            if let Some(peer) =
                                                peers.lock().await.get(&peer_id).cloned()
                                            {
                                                let msg_id = Some(
                                                    ONE_SHOT_COUNTER.fetch_add(1, Ordering::SeqCst),
                                                );
                                                let respond_sigs: RespondSignatures = oneshot(
                                                    peer.websocket.clone(),
                                                    ChiaMessage::new(
                                                        ProtocolMessageTypes::RequestSignatures,
                                                        &request,
                                                        msg_id,
                                                    ),
                                                    Some(ProtocolMessageTypes::RespondSignatures),
                                                    msg_id,
                                                    Some(15000),
                                                )
                                                .await?;
                                                let response_msg_sig = if let Some(f) =
                                                    respond_sigs.message_signatures.first()
                                                {
                                                    Signature::from_bytes(f.1.to_sized_bytes())
                                                        .map_err(|e| {
                                                            Error::new(
                                                                ErrorKind::InvalidInput,
                                                                format!("{:?}", e),
                                                            )
                                                        })?
                                                } else {
                                                    return Err(Error::new(
                                                        ErrorKind::InvalidInput,
                                                        "No Signature in Response",
                                                    ));
                                                };
                                                let mut plot_sig = None;
                                                let local_pk = PublicKey::from_bytes(
                                                    respond_sigs.local_pk.to_sized_bytes(),
                                                )
                                                .map_err(|e| {
                                                    Error::new(
                                                        ErrorKind::InvalidInput,
                                                        format!("{:?}", e),
                                                    )
                                                })?;
                                                for sk in self.farmer_private_keys.values() {
                                                    let pk = sk.sk_to_pk();
                                                    if pk.to_bytes()
                                                        == *respond_sigs.farmer_pk.to_sized_bytes()
                                                    {
                                                        let agg_pk = generate_plot_public_key(
                                                            &local_pk, &pk, true,
                                                        )?;
                                                        if agg_pk.to_bytes()
                                                            != *new_pos
                                                                .proof
                                                                .plot_public_key
                                                                .to_sized_bytes()
                                                        {
                                                            return Err(Error::new(
                                                                ErrorKind::InvalidInput,
                                                                "Key Mismatch",
                                                            ));
                                                        }
                                                        let sig_farmer =
                                                            sign_prepend(sk, &to_sign, &agg_pk);
                                                        let taproot_sk =
                                                            generate_taproot_sk(&local_pk, &pk)?;
                                                        let taproot_sig = sign_prepend(
                                                            &taproot_sk,
                                                            &to_sign,
                                                            &agg_pk,
                                                        );

                                                        let p_sig = AggregateSignature::aggregate(
                                                            &[
                                                                &sig_farmer,
                                                                &response_msg_sig,
                                                                &taproot_sig,
                                                            ],
                                                            true,
                                                        )
                                                        .map_err(|e| {
                                                            Error::new(
                                                                ErrorKind::InvalidInput,
                                                                format!("{:?}", e),
                                                            )
                                                        })?;
                                                        if p_sig.to_signature().verify(
                                                            true,
                                                            to_sign.as_ref(),
                                                            AUG_SCHEME_DST,
                                                            &agg_pk.to_bytes(),
                                                            &agg_pk,
                                                            true,
                                                        ) != BLST_ERROR::BLST_SUCCESS
                                                        {
                                                            warn!(
                                                            "Failed to validate partial signature {:?}",
                                                            p_sig.to_signature()
                                                        );
                                                            continue;
                                                        }
                                                        plot_sig = Some(p_sig);
                                                    }
                                                }
                                                if let Some(auth_key) = self
                                                    .owner_secret_keys
                                                    .get(&pool_config.owner_public_key)
                                                {
                                                    let auth_sig = sign(auth_key, &to_sign);
                                                    if let Some(plot_sig) = plot_sig {
                                                        let agg_sig =
                                                            AggregateSignature::aggregate(
                                                                &[
                                                                    &plot_sig.to_signature(),
                                                                    &auth_sig,
                                                                ],
                                                                true,
                                                            )
                                                            .map_err(|e| {
                                                                Error::new(
                                                                    ErrorKind::InvalidInput,
                                                                    format!("{:?}", e),
                                                                )
                                                            })?;
                                                        let post_request = PostPartialRequest {
                                                            payload,
                                                            aggregate_signature: agg_sig
                                                                .to_signature()
                                                                .to_bytes()
                                                                .into(),
                                                        };
                                                        debug!(
                                                            "Submitting partial for {} to {}",
                                                            post_request
                                                                .payload
                                                                .launcher_id
                                                                .to_string(),
                                                            pool_url
                                                        );
                                                        pool_state.points_found_since_start +=
                                                            pool_state
                                                                .current_difficulty
                                                                .unwrap_or_default();
                                                        pool_state.points_found_24h.push((
                                                            Instant::now(),
                                                            pool_state
                                                                .current_difficulty
                                                                .unwrap_or_default(),
                                                        ));
                                                        #[cfg(feature = "metrics")]
                                                        if let Some(r) =
                                                            self.metrics.lock().await.as_mut()
                                                        {
                                                            use std::time::Duration;
                                                            let now = Instant::now();
                                                            if let Some(c) = &mut r.points_found_24h
                                                            {
                                                                c.set(
                                                                    pool_state
                                                                        .points_found_24h.iter().filter(|v| now.duration_since(v.0) < Duration::from_secs(60 * 60 * 24) ).map(|v| v.1).sum()
                                                                )
                                                            }
                                                        }
                                                        debug!(
                                                            "POST /partial request {:?}",
                                                            &post_request
                                                        );
                                                        match self
                                                            .pool_client
                                                            .post_partial(
                                                                pool_url,
                                                                post_request,
                                                                &Some(
                                                                    self.headers.as_ref().clone(),
                                                                ),
                                                            )
                                                            .await
                                                        {
                                                            Ok(resp) => {
                                                                pool_state
                                                                    .points_acknowledged_since_start +=
                                                                    resp.new_difficulty;
                                                                pool_state.current_points +=
                                                                    resp.new_difficulty;
                                                                pool_state
                                                                    .points_acknowledged_24h
                                                                    .push((
                                                                        Instant::now(),
                                                                        pool_state
                                                                            .current_difficulty
                                                                            .unwrap_or_default(),
                                                                    ));
                                                                #[cfg(feature = "metrics")]
                                                                if let Some(r) = self
                                                                    .metrics
                                                                    .lock()
                                                                    .await
                                                                    .as_mut()
                                                                {
                                                                    use std::time::Duration;
                                                                    let now = Instant::now();
                                                                    if let Some(c) = &mut r
                                                                        .points_acknowledged_24h
                                                                    {
                                                                        c.set(
                                                                            pool_state
                                                                                .points_acknowledged_24h.iter().filter(|v| now.duration_since(v.0) < Duration::from_secs(60 * 60 * 24) ).map(|v| v.1).sum()
                                                                        )
                                                                    }
                                                                }
                                                                if pool_state
                                                                    .current_difficulty
                                                                    .unwrap_or_default()
                                                                    != resp.new_difficulty
                                                                {
                                                                    info!(
                                                                        "New Pool Difficulty: {:?} ",
                                                                        pool_state.current_difficulty
                                                                    );
                                                                }
                                                                pool_state.current_difficulty =
                                                                    Some(resp.new_difficulty);
                                                                #[cfg(feature = "metrics")]
                                                                if let Some(r) = self
                                                                    .metrics
                                                                    .lock()
                                                                    .await
                                                                    .as_mut()
                                                                {
                                                                    if let Some(c) =
                                                                        &mut r.current_difficulty
                                                                    {
                                                                        c.set(resp.new_difficulty);
                                                                    }
                                                                }
                                                                debug!(
                                                                    "Current Points: {:?} ",
                                                                    pool_state.current_points
                                                                );
                                                            }
                                                            Err(e) => {
                                                                error!("Error in pooling: {:?}", e);
                                                                pool_state.pool_errors_24h.push((
                                                                    Instant::now(),
                                                                    format!("{:?}", e),
                                                                ));
                                                                if e.error_code
                                                                    == PoolErrorCode::ProofNotGoodEnough as u8
                                                                {
                                                                    error!("Partial not good enough, forcing pool farmer update to get our current difficulty.");
                                                                    pool_state.next_farmer_update = Instant::now();
                                                                    let _ = update_pool_farmer_info(
                                                                            pool_state,
                                                                            &pool_config,
                                                                            auth_token_timeout,
                                                                            auth_key,
                                                                            self.pool_client.clone(),
                                                                            self.headers.clone()
                                                                        )
                                                                        .await;
                                                                }
                                                                if e.error_code
                                                                    == PoolErrorCode::InvalidSignature as u8
                                                                {
                                                                    error!("Invalid Signature, Forcing Pool Update");
                                                                    pool_state.next_farmer_update = Instant::now();
                                                                }
                                                                return Ok(());
                                                            }
                                                        }
                                                    }
                                                } else {
                                                    warn!("No authentication sk for {p2_singleton_puzzle_hash}");
                                                    return Ok(());
                                                }
                                            } else {
                                                warn!("No peer to sign partial");
                                            }
                                        } else {
                                            warn!("No pool specific authentication_token_timeout has been set for {p2_singleton_puzzle_hash}, check communication with the pool.");
                                            return Ok(());
                                        }
                                    } else {
                                        warn!("No pool specific difficulty has been set for {p2_singleton_puzzle_hash}, check communication with the pool, skipping this partial to {}.", pool_url);
                                        return Ok(());
                                    }
                                } else {
                                    warn!("No Pool Config for {p2_singleton_puzzle_hash}");
                                    return Ok(());
                                }
                            } else {
                                warn!("Did not find pool info for {p2_singleton_puzzle_hash}");
                                return Ok(());
                            }
                        } else {
                            debug!("Not a pooling proof of space");
                        }
                    } else {
                        warn!("Invalid proof of space {:?}", new_pos);
                    }
                }
            } else {
                warn!(
                    "Received response for a signage point that we do not have {}",
                    &new_pos.sp_hash
                );
            }
        }
        Ok(())
    }
}
