use crate::farmer::config::Config;
use crate::farmer::{FarmerSharedState, load_client_id};
use crate::harvesters::{Harvester, ProofHandler, SignatureHandler};
use crate::{HEADERS, PROTOCOL_VERSION};
use async_trait::async_trait;
use blst::BLST_ERROR;
use blst::min_pk::{AggregateSignature, PublicKey, Signature};
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::proof_of_space::{generate_plot_public_key, generate_taproot_sk};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::bls_bindings::{sign, sign_prepend};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality_for_proof, calculate_sp_interval_iters,
};
use dg_xch_core::constants::AUG_SCHEME_DST;
use dg_xch_core::protocols::farmer::{FarmerIdentifier, NewSignagePoint};
use dg_xch_core::protocols::harvester::{
    NewProofOfSpace, RequestSignatures, RespondSignatures, SignatureRequestSourceData,
    SigningDataKind,
};
use dg_xch_core::protocols::pool::{
    PoolErrorCode, PostPartialPayload, PostPartialRequest, get_current_authentication_token,
};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_pos::verify_and_get_quality_string_with_context;
use dg_xch_serialize::ChiaSerialize;
use log::{debug, error, info, warn};
use std::collections::hash_map::Entry;
use std::io::{Error, ErrorKind};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::RwLock;

pub struct NewProofOfSpaceHandle<P, S, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    pub pool_client: Arc<P>,
    pub shared_state: Arc<FarmerSharedState<T>>,
    pub harvester: Arc<H>,
    pub constants: ConsensusConstants,
    pub config: Arc<RwLock<Config<C>>>,
    pub client: Arc<RwLock<Option<FarmerClient<T>>>>,
    phantom_data: PhantomData<S>,
}

#[async_trait]
impl<P, S, T, H, C> ProofHandler<T, H, C> for NewProofOfSpaceHandle<P, S, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
        harvester: Arc<H>,
        client: Arc<RwLock<Option<FarmerClient<T>>>>,
    ) -> Result<Arc<Self>, Error> {
        let constants = config.read().await.constants()?;
        let s = Self {
            pool_client: Arc::new(P::default()),
            shared_state: shared_state.clone(),
            harvester: harvester.clone(),
            constants,
            config,
            client,
            phantom_data: PhantomData {},
        };
        Ok(Arc::new(s))
    }

    async fn handle_proof(&self, new_pos: NewProofOfSpace) -> Result<(), Error> {
        debug!("Got NewProofOfSpace, Searching for SP: {}", new_pos.sp_hash);
        if let Some(sps) = self
            .shared_state
            .signage_points
            .read()
            .await
            .get(&new_pos.sp_hash)
        {
            debug!("Found SignagePoint List ");
            if sps.is_empty() {
                warn!("Empty Signage Point List");
            }
            let mut partial_submitted = false;
            for sp in sps.iter().filter(|point| {
                proof_matches_signage_point(
                    point,
                    new_pos.challenge_hash,
                    new_pos.sp_hash,
                    new_pos.signage_point_index,
                )
            }) {
                if let Some(qs) = verify_and_get_quality_string_with_context(
                    &new_pos.proof,
                    &self.constants,
                    new_pos.challenge_hash,
                    new_pos.sp_hash,
                    sp.peak_height,
                    sp.last_tx_height,
                ) {
                    let required_iters = calculate_iterations_quality_for_proof(
                        &self.constants,
                        &new_pos.proof,
                        qs,
                        sp.difficulty,
                        new_pos.sp_hash,
                    );
                    if required_iters
                        < calculate_sp_interval_iters(&self.constants, sp.sub_slot_iters)?
                    {
                        if let Some(fee_info) = &new_pos.fee_info {
                            let mut to_hash: Vec<u8> = vec![];
                            to_hash.extend_from_slice(new_pos.proof.proof.as_ref());
                            to_hash.extend_from_slice(new_pos.proof.challenge.as_ref());
                            let condition_hash = hash_256(to_hash);
                            let value = u32::from_be_bytes(
                                condition_hash[28..32].try_into().map_err(Error::other)?,
                            );
                            if value < fee_info.applied_fee_threshold {
                                info!(
                                    "Using 3rd Party Harvester Fee for challenge {}, Threshold {:.3}%/{:.3}% ({}/{})",
                                    sp.challenge_hash,
                                    value as f64 / 0xFFFFFFFFu32 as f64 * 100f64,
                                    fee_info.applied_fee_threshold as f64 / 0xFFFFFFFFu32 as f64
                                        * 100f64,
                                    format_big_number(value),
                                    format_big_number(fee_info.applied_fee_threshold),
                                );
                            } else {
                                info!(
                                    "No Fee Used for challenge {}, {:.3}%/{:.3}% ({}/{})",
                                    sp.challenge_hash,
                                    value as f64 / 0xFFFFFFFFu32 as f64 * 100f64,
                                    fee_info.applied_fee_threshold as f64 / 0xFFFFFFFFu32 as f64
                                        * 100f64,
                                    format_big_number(value),
                                    format_big_number(fee_info.applied_fee_threshold),
                                );
                            }
                        } else {
                            info!("No Fee applied for challenge {}", sp.challenge_hash,);
                        }
                        self._handle_proof(sp, &qs, &new_pos).await?;
                    }
                    if let Some(p2_singleton_puzzle_hash) = &new_pos.proof.pool_contract_puzzle_hash
                        && !partial_submitted
                    {
                        self.handle_partial(p2_singleton_puzzle_hash, &qs, new_pos.clone())
                            .await?;
                        partial_submitted = true;
                    } else if new_pos.proof.pool_contract_puzzle_hash.is_none() {
                        warn!("Not a pooling proof of space");
                    }
                } else {
                    warn!("Invalid proof of space {new_pos:?}");
                }
            }
        } else {
            warn!(
                "Received response for a signage point that we do not have {}",
                new_pos.sp_hash
            );
        }
        Ok(())
    }
}

fn proof_matches_signage_point(
    point: &NewSignagePoint,
    challenge_hash: Bytes32,
    sp_hash: Bytes32,
    signage_point_index: u8,
) -> bool {
    point.challenge_hash == challenge_hash
        && point.challenge_chain_sp == sp_hash
        && point.signage_point_index == signage_point_index
}

fn format_big_number(number: u32) -> String {
    number
        .to_string()
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(",")
}

impl<P, S, T, H, C> NewProofOfSpaceHandle<P, S, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    async fn _handle_proof(
        &self,
        sp: &NewSignagePoint,
        qs: &Bytes32,
        new_pos: &NewProofOfSpace,
    ) -> Result<(), Error> {
        match self
            .shared_state
            .proofs_of_space
            .write()
            .await
            .entry(new_pos.sp_hash)
        {
            Entry::Occupied(mut e) => {
                let proofs = e.get_mut();
                if !proofs.iter().any(|(identifier, proof)| {
                    identifier == &new_pos.plot_identifier && proof == &new_pos.proof
                }) {
                    proofs.push((new_pos.plot_identifier.clone(), new_pos.proof.clone()));
                }
            }
            Entry::Vacant(e) => {
                e.insert(vec![(
                    new_pos.plot_identifier.clone(),
                    new_pos.proof.clone(),
                )]);
            }
        }
        self.shared_state
            .cache_time
            .write()
            .await
            .insert(new_pos.sp_hash, Instant::now());
        self.shared_state
            .quality_to_identifiers
            .write()
            .await
            .insert(
                *qs,
                FarmerIdentifier {
                    plot_identifier: new_pos.plot_identifier.clone(),
                    challenge_hash: new_pos.challenge_hash,
                    sp_hash: new_pos.sp_hash,
                    peer_node_id: self.harvester.uuid(),
                },
            );
        self.shared_state
            .cache_time
            .write()
            .await
            .insert(*qs, Instant::now());
        let sig_handle = S::load(
            self.shared_state.clone(),
            self.config.clone(),
            self.harvester.clone(),
            self.client.clone(),
        )
        .await;
        let sp_src_data = {
            if new_pos.include_source_signature_data
                || new_pos.farmer_reward_address_override.is_some()
            {
                if let Some(sp_data) = sp.sp_source_data.as_ref() {
                    let (cc, rc) = if let Some(vdf) = sp_data.vdf_data.as_ref() {
                        (
                            SignatureRequestSourceData {
                                kind: SigningDataKind::ChallengeChainVdf,
                                data: vdf.cc_vdf.to_bytes(PROTOCOL_VERSION)?,
                            },
                            SignatureRequestSourceData {
                                kind: SigningDataKind::RewardChainVdf,
                                data: vdf.rc_vdf.to_bytes(PROTOCOL_VERSION)?,
                            },
                        )
                    } else if let Some(sub_slot_data) = sp_data.sub_slot_data.as_ref() {
                        (
                            SignatureRequestSourceData {
                                kind: SigningDataKind::ChallengeChainSubSlot,
                                data: sub_slot_data.cc_sub_slot.to_bytes(PROTOCOL_VERSION)?,
                            },
                            SignatureRequestSourceData {
                                kind: SigningDataKind::RewardChainSubSlot,
                                data: sub_slot_data.rc_sub_slot.to_bytes(PROTOCOL_VERSION)?,
                            },
                        )
                    } else {
                        error!("Source Signature Did not contain any data, Cannot Sign Proof");
                        return Ok(());
                    };
                    Some(vec![Some(cc), Some(rc)])
                } else {
                    error!("Source Signature Data Request But was Null, Cannot Sign Proof");
                    return Ok(());
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
        let harvester = self.harvester.clone();
        tokio::spawn(async move {
            if let Err(e) = harvester.request_signatures(request, sig_handle?).await {
                error!("Error Requesting Signature: {e}");
            }
            Ok::<(), Error>(())
        });
        Ok(())
    }

    async fn handle_partial(
        &self,
        p2_singleton_puzzle_hash: &Bytes32,
        qs: &Bytes32,
        new_pos: NewProofOfSpace,
    ) -> Result<(), Error> {
        if self
            .shared_state
            .pool_states
            .read()
            .await
            .get(p2_singleton_puzzle_hash)
            .is_none()
        {
            warn!("Did not find pool info for {p2_singleton_puzzle_hash}");
            return Ok(());
        }
        let (pool_url, launcher_id) = if let Some(Some(config)) = self
            .shared_state
            .pool_states
            .read()
            .await
            .get(p2_singleton_puzzle_hash)
            .map(|v| v.pool_config.as_ref())
        {
            (config.pool_url.clone(), config.launcher_id)
        } else {
            warn!("No Pool Config for {p2_singleton_puzzle_hash}");
            return Ok(());
        };
        if pool_url.is_empty() || new_pos.proof.pool_contract_puzzle_hash.is_none() {
            return Ok(());
        }

        let (required_iters, pool_dif) = if let Some(Some(pool_dif)) = self
            .shared_state
            .pool_states
            .read()
            .await
            .get(p2_singleton_puzzle_hash)
            .map(|v| v.current_difficulty)
        {
            (
                calculate_iterations_quality_for_proof(
                    &self.constants,
                    &new_pos.proof,
                    *qs,
                    pool_dif,
                    new_pos.sp_hash,
                ),
                pool_dif,
            )
        } else {
            warn!(
                "No pool specific difficulty has been set for {p2_singleton_puzzle_hash}, check communication with the pool, skipping this partial to {pool_url}."
            );
            return Ok(());
        };
        let pool_required_iters =
            calculate_sp_interval_iters(&self.constants, self.constants.pool_sub_slot_iters)?;
        if required_iters > pool_required_iters {
            warn!("Proof of space not good enough for pool {pool_url}: {pool_dif:?} {qs:?}");
            return Ok(());
        }
        let auth_token_timeout = if let Some(Some(auth_token_timeout)) = self
            .shared_state
            .pool_states
            .read()
            .await
            .get(p2_singleton_puzzle_hash)
            .map(|v| v.authentication_token_timeout)
        {
            auth_token_timeout
        } else {
            warn!(
                "No pool specific authentication_token_timeout has been set for {p2_singleton_puzzle_hash}, check communication with the pool."
            );
            return Ok(());
        };
        let payload = PostPartialPayload {
            launcher_id,
            authentication_token: get_current_authentication_token(auth_token_timeout)?,
            proof_of_space: new_pos.proof.clone(),
            sp_hash: new_pos.sp_hash,
            end_of_sub_slot: new_pos.signage_point_index == 0,
            harvester_id: load_client_id::<C>(self.config.clone()).await?,
        };
        let payload_bytes = hash_256(payload.to_bytes(PROTOCOL_VERSION)?);
        let sp_src_data = {
            if new_pos.include_source_signature_data
                || new_pos.farmer_reward_address_override.is_some()
            {
                Some(vec![Some(SignatureRequestSourceData {
                    kind: SigningDataKind::Partial,
                    data: payload.to_bytes(PROTOCOL_VERSION)?,
                })])
            } else {
                None
            }
        };
        let request = RequestSignatures {
            plot_identifier: new_pos.plot_identifier.clone(),
            challenge_hash: new_pos.challenge_hash,
            sp_hash: new_pos.sp_hash,
            messages: vec![Bytes32::new(payload_bytes)],
            message_data: sp_src_data,
            rc_block_unfinished: None,
        };
        let handler = PartialHandler {
            config: self.config.clone(),
            pool_client: self.pool_client.clone(),
            shared_state: self.shared_state.clone(),
            p2_singleton_puzzle_hash: *p2_singleton_puzzle_hash,
            new_pos,
            auth_token_timeout,
            payload,
            payload_bytes: payload_bytes.to_vec(),
            pool_dif,
        };
        let harvester = self.harvester.clone();
        tokio::spawn(async move {
            if let Err(e) = harvester.request_signatures(request, handler).await {
                error!("Error Requesting Signature: {e}");
            }
        });
        Ok(())
    }
}

pub struct PartialHandler<
    T: Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
    P: PoolClient + Default + Sized + Sync + Send + 'static,
> {
    pub pool_client: Arc<P>,
    pub shared_state: Arc<FarmerSharedState<T>>,
    pub config: Arc<RwLock<Config<C>>>,
    pub auth_token_timeout: u8,
    pub p2_singleton_puzzle_hash: Bytes32,
    pub new_pos: NewProofOfSpace,
    pub payload: PostPartialPayload,
    pub payload_bytes: Vec<u8>,
    pub pool_dif: u64,
}
#[async_trait]
impl<
    T: Sync + Send + 'static,
    H: Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
    P: PoolClient + Default + Sized + Sync + Send + 'static,
> SignatureHandler<T, H, C> for PartialHandler<T, C, P>
{
    async fn load(
        _shared_state: Arc<FarmerSharedState<T>>,
        _config: Arc<RwLock<Config<C>>>,
        _harvester: Arc<H>,
        _client: Arc<RwLock<Option<FarmerClient<T>>>>,
    ) -> Result<Arc<Self>, Error> {
        Err(Error::other(
            "Do not Create Partial Handler with Load. Create it Directly with PartialHandler { .. }",
        ))
    }

    async fn handle_signature(&self, respond_sigs: RespondSignatures) -> Result<(), Error> {
        let response_msg_sig = if let Some(f) = respond_sigs.message_signatures.first() {
            Signature::from_bytes(&f.1.bytes())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?
        } else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "No Signature in Response",
            ));
        };
        let mut plot_sig = None;
        let local_pk = PublicKey::from_bytes(&respond_sigs.local_pk.bytes())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
        for (pk, sk) in self.shared_state.farmer_private_keys.iter() {
            if *pk == respond_sigs.farmer_pk {
                let agg_pk = generate_plot_public_key(&local_pk, &pk.into(), true)?;
                if agg_pk.to_bytes() != self.new_pos.proof.plot_public_key.bytes() {
                    return Err(Error::new(ErrorKind::InvalidInput, "Key Mismatch"));
                }
                let sig_farmer = sign_prepend(sk, &self.payload_bytes, &agg_pk);
                let taproot_sk = generate_taproot_sk(&local_pk, &pk.into())?;
                let taproot_sig = sign_prepend(&taproot_sk, &self.payload_bytes, &agg_pk);
                let p_sig = AggregateSignature::aggregate(
                    &[&sig_farmer, &response_msg_sig, &taproot_sig],
                    true,
                )
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
                if p_sig.to_signature().verify(
                    true,
                    &self.payload_bytes,
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
                break;
            }
        }
        if self
            .shared_state
            .pool_states
            .read()
            .await
            .get(&self.p2_singleton_puzzle_hash)
            .is_none()
        {
            warn!(
                "Did not find pool info for {}",
                self.p2_singleton_puzzle_hash
            );
            return Ok(());
        }
        let (pool_url, owner_public_key) = if let Some(Some(config)) = self
            .shared_state
            .pool_states
            .read()
            .await
            .get(&self.p2_singleton_puzzle_hash)
            .map(|v| v.pool_config.as_ref())
        {
            (config.pool_url.clone(), config.owner_public_key)
        } else {
            warn!("No Pool Config for {}", self.p2_singleton_puzzle_hash);
            return Ok(());
        };
        if let Some(auth_key) = self
            .shared_state
            .owner_public_keys_to_auth_secret_keys
            .get(&owner_public_key)
        {
            let auth_sig = sign(auth_key, &self.payload_bytes);
            if let Some(plot_sig) = plot_sig {
                let agg_sig =
                    AggregateSignature::aggregate(&[&plot_sig.to_signature(), &auth_sig], true)
                        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
                let post_request = PostPartialRequest {
                    payload: self.payload.clone(),
                    aggregate_signature: agg_sig.to_signature().to_bytes().into(),
                };
                debug!(
                    "Submitting partial for {} to {}",
                    post_request.payload.launcher_id, pool_url
                );
                if let Some(v) = self
                    .shared_state
                    .pool_states
                    .write()
                    .await
                    .get_mut(&self.p2_singleton_puzzle_hash)
                {
                    v.points_found_since_start += self.pool_dif;
                    v.points_found_24h.push((Instant::now(), self.pool_dif));
                }
                if let Some(r) = self.shared_state.metrics.read().await.as_ref() {
                    use std::time::Duration;
                    let now = Instant::now();
                    if let Some(v) = self
                        .shared_state
                        .pool_states
                        .write()
                        .await
                        .get_mut(&self.p2_singleton_puzzle_hash)
                    {
                        r.points_found_24h
                            .with_label_values(&[&self.p2_singleton_puzzle_hash.to_string()])
                            .set(
                                v.points_found_24h
                                    .iter()
                                    .filter(|v| {
                                        now.duration_since(v.0) < Duration::from_secs(60 * 60 * 24)
                                    })
                                    .map(|v| v.1)
                                    .sum(),
                            )
                    }
                }
                let mut headers = self.shared_state.additional_headers.as_ref().clone();
                if let Some(v) = &*self.shared_state.upstream_handshake.read().await {
                    headers.insert(String::from("X-chia-version"), v.software_version.clone());
                    headers.insert(
                        String::from("chia-node-version"),
                        v.software_version.clone(),
                    );
                    headers.insert(
                        String::from("chia-farmer-version"),
                        v.software_version.clone(),
                    );
                    headers.insert(
                        String::from("chia-harvester-version"),
                        v.software_version.clone(),
                    );
                }
                headers.extend(HEADERS.clone());
                match self
                    .pool_client
                    .post_partial(&pool_url, post_request, &Some(headers))
                    .await
                {
                    Ok(resp) => {
                        if let Some(v) = self
                            .shared_state
                            .pool_states
                            .write()
                            .await
                            .get_mut(&self.p2_singleton_puzzle_hash)
                        {
                            v.current_points += resp.new_difficulty;
                            if v.current_difficulty != Some(resp.new_difficulty) {
                                info!("New Pool Difficulty: {:?} ", v.current_difficulty);
                                v.current_difficulty = Some(resp.new_difficulty);
                            }
                            debug!("Current Points: {:?} ", v.current_points);
                        }
                    }
                    Err(e) => {
                        error!("Error in pooling: {e:?}");
                        if e.error_code == PoolErrorCode::ProofNotGoodEnough as u8 {
                            error!(
                                "Partial not good enough, forcing pool farmer update to get our current difficulty."
                            );
                            self.shared_state
                                .force_pool_update
                                .store(true, Ordering::Relaxed);
                        }
                        if e.error_code == PoolErrorCode::InvalidSignature as u8 {
                            error!("Invalid Signature, Forcing Pool Update");
                            if let Some(v) = self
                                .shared_state
                                .pool_states
                                .write()
                                .await
                                .get_mut(&self.p2_singleton_puzzle_hash)
                            {
                                v.next_farmer_update = Instant::now();
                            }
                        }
                    }
                }
            } else {
                warn!("invalid plot Sig");
            }
        } else {
            warn!("No authentication sk for {}", self.p2_singleton_puzzle_hash);
        }
        Ok(())
    }
}

#[cfg(test)]
mod proof_binding_tests {
    use super::{NewSignagePoint, proof_matches_signage_point};

    #[test]
    fn proofs_match_challenge_hash_signage_hash_and_index() {
        let point = NewSignagePoint {
            challenge_hash: [1; 32].into(),
            challenge_chain_sp: [2; 32].into(),
            reward_chain_sp: [3; 32].into(),
            difficulty: 1,
            sub_slot_iters: 64,
            signage_point_index: 4,
            peak_height: 100,
            last_tx_height: 100,
            sp_source_data: None,
        };
        assert!(proof_matches_signage_point(
            &point,
            [1; 32].into(),
            [2; 32].into(),
            4
        ));
        assert!(!proof_matches_signage_point(
            &point,
            [9; 32].into(),
            [2; 32].into(),
            4
        ));
        assert!(!proof_matches_signage_point(
            &point,
            [1; 32].into(),
            [9; 32].into(),
            4
        ));
        assert!(!proof_matches_signage_point(
            &point,
            [1; 32].into(),
            [2; 32].into(),
            5
        ));
    }
}
