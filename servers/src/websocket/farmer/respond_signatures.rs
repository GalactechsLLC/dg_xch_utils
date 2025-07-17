use crate::websocket::farmer::FarmerServerConfig;
use async_trait::async_trait;
use blst::min_pk::{AggregateSignature, SecretKey};
use blst::BLST_ERROR;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::proof_of_space::{generate_plot_public_key, generate_taproot_sk};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::bls_bindings::{sign, sign_prepend};
use dg_xch_core::consensus::constants::{CONSENSUS_CONSTANTS_MAP, MAINNET};
use dg_xch_core::constants::AUG_SCHEME_DST;
#[cfg(feature = "metrics")]
use dg_xch_core::protocols::farmer::FarmerMetrics;
use dg_xch_core::protocols::farmer::{
    DeclareProofOfSpace, NewSignagePoint, ProofsMap, SignedValues,
};
use dg_xch_core::protocols::harvester::RespondSignatures;
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap, ProtocolMessageTypes};
use dg_xch_core::traits::SizedBytes;
use dg_xch_pos::verify_and_get_quality_string;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hyper_tungstenite::tungstenite::Message;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RespondSignaturesHandle<T> {
    pub signage_points: Arc<RwLock<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub proofs_of_space: ProofsMap,
    pub pool_public_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub farmer_private_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub full_node_client: Arc<RwLock<Option<FarmerClient<T>>>>,
    pub config: Arc<FarmerServerConfig>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<RwLock<Option<FarmerMetrics>>>,
}
#[async_trait]
impl<T: Sync + Send + 'static> MessageHandler for RespondSignaturesHandle<T> {
    #[allow(clippy::too_many_lines)]
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(&msg.data);
        let peer = peers.read().await.get(&peer_id).cloned();
        let protocol_version = if let Some(peer) = peer.as_ref() {
            *peer.protocol_version.read().await
        } else {
            ChiaProtocolVersion::default()
        };
        let response = RespondSignatures::from_bytes(&mut cursor, protocol_version)?;
        if let Some(sps) = self.signage_points.read().await.get(&response.sp_hash) {
            if sps.is_empty() {
                error!("Missing Signage Points for {}", &response.sp_hash);
            } else {
                let sp_index = sps
                    .first()
                    .expect("Sps was empty, Should have been caught above")
                    .signage_point_index;
                let mut is_sp_signatures = false;
                let mut found_sp_hash_debug = false;
                let peak_height = sps[0].peak_height;
                for sp_candidate in sps {
                    if response.sp_hash == response.message_signatures[0].0 {
                        found_sp_hash_debug = true;
                        if sp_candidate.reward_chain_sp == response.message_signatures[1].0 {
                            is_sp_signatures = true;
                        }
                    }
                }
                if found_sp_hash_debug {
                    assert!(is_sp_signatures);
                }
                let mut pospace = None;
                if let Some(proofs) = self.proofs_of_space.read().await.get(&response.sp_hash) {
                    for (plot_identifier, candidate_pospace) in proofs {
                        if *plot_identifier == response.plot_identifier {
                            pospace = Some(candidate_pospace.clone());
                            break;
                        }
                    }
                } else {
                    debug!("Failed to load farmer proofs for {}", &response.sp_hash);
                    return Ok(());
                }
                if let Some(pospace) = pospace {
                    let include_taproot = pospace.pool_contract_puzzle_hash.is_some();
                    let constants = CONSENSUS_CONSTANTS_MAP
                        .get(&self.config.network)
                        .unwrap_or(&MAINNET);
                    if let Some(computed_quality_string) = verify_and_get_quality_string(
                        &pospace,
                        constants,
                        response.challenge_hash,
                        response.sp_hash,
                        peak_height,
                    ) {
                        if is_sp_signatures {
                            let (challenge_chain_sp, challenge_chain_sp_harv_sig) =
                                &response.message_signatures[0];
                            let challenge_chain_sp_harv_sig =
                                challenge_chain_sp_harv_sig.try_into()?;
                            let (reward_chain_sp, reward_chain_sp_harv_sig) =
                                &response.message_signatures[1];
                            let reward_chain_sp_harv_sig = reward_chain_sp_harv_sig.try_into()?;
                            let local_pk = response.local_pk.into();
                            for sk in self.farmer_private_keys.values() {
                                let pk = sk.sk_to_pk();
                                if pk.to_bytes() == response.farmer_pk.bytes() {
                                    let agg_pk =
                                        generate_plot_public_key(&local_pk, &pk, include_taproot)?;
                                    if agg_pk.to_bytes() != pospace.plot_public_key.bytes() {
                                        warn!(
                                            "Key Mismatch {:?} != {:?}",
                                            pospace.plot_public_key, agg_pk
                                        );
                                        return Ok(());
                                    }
                                    let (taproot_share_cc_sp, taproot_share_rc_sp) =
                                        if include_taproot {
                                            let taproot_sk = generate_taproot_sk(&local_pk, &pk)?;
                                            (
                                                Some(sign_prepend(
                                                    &taproot_sk,
                                                    challenge_chain_sp.as_ref(),
                                                    &agg_pk,
                                                )),
                                                Some(sign_prepend(
                                                    &taproot_sk,
                                                    reward_chain_sp.as_ref(),
                                                    &agg_pk,
                                                )),
                                            )
                                        } else {
                                            (None, None)
                                        };
                                    let farmer_share_cc_sp =
                                        sign_prepend(sk, challenge_chain_sp.as_ref(), &agg_pk);
                                    let cc_sigs_to_agg =
                                        if let Some(taproot_share_cc_sp) = &taproot_share_cc_sp {
                                            vec![
                                                &challenge_chain_sp_harv_sig,
                                                &farmer_share_cc_sp,
                                                taproot_share_cc_sp,
                                            ]
                                        } else {
                                            vec![&challenge_chain_sp_harv_sig, &farmer_share_cc_sp]
                                        };
                                    let agg_sig_cc_sp =
                                        AggregateSignature::aggregate(&cc_sigs_to_agg, true)
                                            .map_err(|e| {
                                                Error::new(
                                                    ErrorKind::InvalidInput,
                                                    format!("{e:?}"),
                                                )
                                            })?;
                                    if agg_sig_cc_sp.to_signature().verify(
                                        true,
                                        challenge_chain_sp.as_ref(),
                                        AUG_SCHEME_DST,
                                        &agg_pk.to_bytes(),
                                        &agg_pk,
                                        true,
                                    ) != BLST_ERROR::BLST_SUCCESS
                                    {
                                        warn!(
                                            "Failed to validate cc signature {:?}",
                                            agg_sig_cc_sp.to_signature()
                                        );
                                        return Ok(());
                                    }

                                    let farmer_share_rc_sp =
                                        sign_prepend(sk, reward_chain_sp.as_ref(), &agg_pk);
                                    let rc_sigs_to_agg =
                                        if let Some(taproot_share_rc_sp) = &taproot_share_rc_sp {
                                            vec![
                                                &reward_chain_sp_harv_sig,
                                                &farmer_share_rc_sp,
                                                taproot_share_rc_sp,
                                            ]
                                        } else {
                                            vec![&reward_chain_sp_harv_sig, &farmer_share_rc_sp]
                                        };
                                    let agg_sig_rc_sp =
                                        AggregateSignature::aggregate(&rc_sigs_to_agg, true)
                                            .map_err(|e| {
                                                Error::new(
                                                    ErrorKind::InvalidInput,
                                                    format!("{e:?}"),
                                                )
                                            })?;
                                    if agg_sig_rc_sp.to_signature().verify(
                                        true,
                                        reward_chain_sp.as_ref(),
                                        AUG_SCHEME_DST,
                                        &agg_pk.to_bytes(),
                                        &agg_pk,
                                        true,
                                    ) != BLST_ERROR::BLST_SUCCESS
                                    {
                                        warn!(
                                            "Failed to validate rc signature {:?}",
                                            agg_sig_rc_sp.to_signature()
                                        );
                                        return Ok(());
                                    }
                                    let (pool_target, pool_target_signature) = if let Some(
                                        pool_public_key,
                                    ) =
                                        &pospace.pool_public_key
                                    {
                                        if let Some(sk) = self.pool_public_keys.get(pool_public_key)
                                        {
                                            let pool_target = PoolTarget {
                                                max_height: 0,
                                                puzzle_hash: self
                                                    .config
                                                    .pool_rewards_payout_address,
                                            };
                                            let pool_target_signature = sign(
                                                sk,
                                                &pool_target
                                                    .to_bytes(ChiaProtocolVersion::default())?,
                                            );
                                            (Some(pool_target), Some(pool_target_signature))
                                        } else {
                                            error!("Don't have the private key for the pool key used by harvester: {pool_public_key}");
                                            return Ok(());
                                        }
                                    } else {
                                        (None, None)
                                    };
                                    let request = DeclareProofOfSpace {
                                        challenge_hash: response.challenge_hash,
                                        challenge_chain_sp: *challenge_chain_sp,
                                        signage_point_index: sp_index,
                                        reward_chain_sp: *reward_chain_sp,
                                        proof_of_space: pospace.clone(),
                                        challenge_chain_sp_signature: agg_sig_cc_sp
                                            .to_signature()
                                            .to_bytes()
                                            .into(),
                                        reward_chain_sp_signature: agg_sig_rc_sp
                                            .to_signature()
                                            .to_bytes()
                                            .into(),
                                        farmer_puzzle_hash: if let Some(
                                            farmer_reward_address_override,
                                        ) =
                                            response.farmer_reward_address_override
                                        {
                                            farmer_reward_address_override
                                        } else {
                                            self.config.farmer_reward_payout_address
                                        },
                                        pool_target,
                                        pool_signature: pool_target_signature
                                            .map(|s| s.to_bytes().into()),
                                        include_signature_source_data: response
                                            .include_source_signature_data
                                            || response.farmer_reward_address_override.is_some(),
                                    };
                                    if let Some(client) =
                                        self.full_node_client.read().await.as_ref()
                                    {
                                        let _ = client
                                            .client
                                            .connection
                                            .write()
                                            .await
                                            .send(Message::Binary(
                                                ChiaMessage::new(
                                                    ProtocolMessageTypes::DeclareProofOfSpace,
                                                    client.client.client_config.protocol_version,
                                                    &request,
                                                    None,
                                                )?
                                                .to_bytes(
                                                    client.client.client_config.protocol_version,
                                                )?
                                                .into(),
                                            ))
                                            .await;
                                        info!("Declaring Proof of Space: {request:?}");
                                        #[cfg(feature = "metrics")]
                                        if let Some(r) = self.metrics.write().await.as_mut() {
                                            if let Some(c) = &mut r.proofs_declared {
                                                c.inc();
                                            }
                                        }
                                    } else {
                                        error!(
                                            "Failed to declare Proof of Space: {request:?} No Client"
                                        );
                                    }
                                }
                            }
                        } else if response.message_signatures.len() > 1 {
                            let (foliage_block_data_hash, foliage_sig_harvester) =
                                &response.message_signatures[0];
                            let foliage_sig_harvester = foliage_sig_harvester.try_into()?;
                            let (
                                foliage_transaction_block_hash,
                                foliage_transaction_block_sig_harvester,
                            ) = &response.message_signatures[1];
                            let foliage_transaction_block_sig_harvester =
                                foliage_transaction_block_sig_harvester.try_into()?;
                            let local_pk = response.local_pk.into();
                            for sk in self.farmer_private_keys.values() {
                                let pk = sk.sk_to_pk();
                                if pk.to_bytes() == response.farmer_pk.bytes() {
                                    let agg_pk =
                                        generate_plot_public_key(&local_pk, &pk, include_taproot)?;
                                    let (
                                        foliage_sig_taproot,
                                        foliage_transaction_block_sig_taproot,
                                    ) = if include_taproot {
                                        let taproot_sk = generate_taproot_sk(&local_pk, &pk)?;
                                        (
                                            Some(sign_prepend(
                                                &taproot_sk,
                                                foliage_block_data_hash.as_ref(),
                                                &agg_pk,
                                            )),
                                            Some(sign_prepend(
                                                &taproot_sk,
                                                foliage_transaction_block_hash.as_ref(),
                                                &agg_pk,
                                            )),
                                        )
                                    } else {
                                        (None, None)
                                    };
                                    let foliage_sig_farmer =
                                        sign_prepend(sk, foliage_block_data_hash.as_ref(), &agg_pk);
                                    let foliage_transaction_block_sig_farmer = sign_prepend(
                                        sk,
                                        foliage_transaction_block_hash.as_ref(),
                                        &agg_pk,
                                    );
                                    let foliage_sigs_to_agg =
                                        if let Some(foliage_sig_taproot) = &foliage_sig_taproot {
                                            vec![
                                                &foliage_sig_harvester,
                                                &foliage_sig_farmer,
                                                foliage_sig_taproot,
                                            ]
                                        } else {
                                            vec![&foliage_sig_harvester, &foliage_sig_farmer]
                                        };
                                    let foliage_agg_sig =
                                        AggregateSignature::aggregate(&foliage_sigs_to_agg, true)
                                            .map_err(|e| {
                                            Error::new(ErrorKind::InvalidInput, format!("{e:?}"))
                                        })?;

                                    let foliage_block_sigs_to_agg =
                                        if let Some(foliage_transaction_block_sig_taproot) =
                                            &foliage_transaction_block_sig_taproot
                                        {
                                            vec![
                                                &foliage_transaction_block_sig_harvester,
                                                &foliage_transaction_block_sig_farmer,
                                                foliage_transaction_block_sig_taproot,
                                            ]
                                        } else {
                                            vec![
                                                &foliage_transaction_block_sig_harvester,
                                                &foliage_transaction_block_sig_farmer,
                                            ]
                                        };
                                    let foliage_block_agg_sig = AggregateSignature::aggregate(
                                        &foliage_block_sigs_to_agg,
                                        true,
                                    )
                                    .map_err(|e| {
                                        Error::new(ErrorKind::InvalidInput, format!("{e:?}"))
                                    })?;
                                    if foliage_agg_sig.to_signature().verify(
                                        true,
                                        foliage_block_data_hash.as_ref(),
                                        AUG_SCHEME_DST,
                                        &agg_pk.to_bytes(),
                                        &agg_pk,
                                        true,
                                    ) != BLST_ERROR::BLST_SUCCESS
                                    {
                                        warn!(
                                            "Failed to validate foliage signature {:?}",
                                            foliage_agg_sig.to_signature()
                                        );
                                        return Ok(());
                                    }
                                    if foliage_block_agg_sig.to_signature().verify(
                                        true,
                                        foliage_transaction_block_hash.as_ref(),
                                        AUG_SCHEME_DST,
                                        &agg_pk.to_bytes(),
                                        &agg_pk,
                                        true,
                                    ) != BLST_ERROR::BLST_SUCCESS
                                    {
                                        warn!(
                                            "Failed to validate foliage_block signature {:?}",
                                            foliage_block_agg_sig.to_signature()
                                        );
                                        return Ok(());
                                    }
                                    let request = SignedValues {
                                        quality_string: computed_quality_string,
                                        foliage_block_data_signature: foliage_agg_sig
                                            .to_signature()
                                            .to_bytes()
                                            .into(),
                                        foliage_transaction_block_signature: foliage_block_agg_sig
                                            .to_signature()
                                            .to_bytes()
                                            .into(),
                                    };

                                    if let Some(client) =
                                        self.full_node_client.read().await.as_ref()
                                    {
                                        let _ = client
                                            .client
                                            .connection
                                            .write()
                                            .await
                                            .send(Message::Binary(
                                                ChiaMessage::new(
                                                    ProtocolMessageTypes::SignedValues,
                                                    client.client.client_config.protocol_version,
                                                    &request,
                                                    None,
                                                )?
                                                .to_bytes(
                                                    client.client.client_config.protocol_version,
                                                )?
                                                .into(),
                                            ))
                                            .await;
                                        info!("Sending Signed Values: {request:?}");
                                    } else {
                                        error!(
                                            "Failed to Sending Signed Values: {request:?} No Client"
                                        );
                                    }
                                }
                            }
                        } else if msg.id.is_some() {
                            debug!("Detected Partial Signatures Request {pospace:?}");
                            return Ok(());
                        } else {
                            warn!("Detected Possible invalid PoSpace {pospace:?}");
                            return Ok(());
                        }
                    } else {
                        warn!("Have invalid PoSpace {pospace:?}");
                        return Ok(());
                    }
                } else {
                    debug!("Failed to find Proof for {}", &response.sp_hash);
                    return Ok(());
                }
            }
        } else {
            error!("Do not have challenge hash {}", &response.challenge_hash);
        }
        Ok(())
    }
}
