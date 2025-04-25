use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_with_args::Message;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::utils::pkm_pairs_for_conditions_dict;
use dg_xch_core::blockchain::wallet_type::WalletType;
use dg_xch_core::clvm::bls_bindings;
use dg_xch_core::clvm::bls_bindings::{aggregate_verify_signature, verify_signature};
use dg_xch_core::clvm::condition_utils::conditions_dict_for_solution;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::traits::SizedBytes;
use log::{debug, info, warn};
use num_traits::cast::ToPrimitive;
use std::collections::HashMap;
use std::future::Future;
use std::hash::RandomState;
use std::io::{Error, ErrorKind};

pub struct DerivationRecord {
    pub index: u32,
    pub puzzle_hash: Bytes32,
    pub pubkey: Bytes48,
    pub wallet_type: WalletType,
    pub wallet_id: u32,
    pub hardened: bool,
}

pub async fn sign_coin_spend<F, Fut>(
    coin_spend: CoinSpend,
    key_fn: F,
    pre_calculated_signatures: HashMap<(Bytes48, Message), Bytes96>,
    constants: &ConsensusConstants,
) -> Result<SpendBundle, Error>
where
    F: Fn(&Bytes48) -> Fut,
    Fut: Future<Output = Result<SecretKey, Error>>,
{
    sign_coin_spends(
        vec![coin_spend],
        key_fn,
        pre_calculated_signatures,
        &constants.agg_sig_me_additional_data,
        constants.max_block_cost_clvm.to_u64().unwrap(),
    )
    .await
}

pub async fn sign_coin_spends<F, Fut>(
    coin_spends: Vec<CoinSpend>,
    key_fn: F,
    pre_calculated_signatures: HashMap<(Bytes48, Message), Bytes96>,
    additional_data: &[u8],
    max_cost: u64,
) -> Result<SpendBundle, Error>
where
    F: Fn(&Bytes48) -> Fut,
    Fut: Future<Output = Result<SecretKey, Error>>,
{
    let mut signatures: Vec<Signature> = vec![];
    let mut pk_list: Vec<Bytes48> = vec![];
    let mut msg_list: Vec<Vec<u8>> = vec![];
    debug!("Creating Signatures for Coin Spends");
    for coin_spend in &coin_spends {
        //Get AGG_SIG conditions
        let conditions_dict = conditions_dict_for_solution::<RandomState>(
            &coin_spend.puzzle_reveal,
            &coin_spend.solution,
            max_cost,
        )?
        .0;
        //Create signature
        for (pk_bytes, msg) in
            pkm_pairs_for_conditions_dict(&conditions_dict, coin_spend.coin, additional_data)?
        {
            let pk = PublicKey::from_bytes(pk_bytes.as_ref()).map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!(
                        "Failed to parse Public key: {}, {:?}",
                        hex::encode(pk_bytes),
                        e
                    ),
                )
            })?;
            let secret_key = (key_fn)(&pk_bytes).await?;
            let signature = if secret_key.sk_to_pk() != pk {
                //Found no Secret Key, Check if the Map Contains our Signature
                pre_calculated_signatures.get(&(pk_bytes, msg)).ok_or_else(|| {
                    info!("Failed to find ({pk_bytes}, {msg}) in map \n {pre_calculated_signatures:#?}");
                    Error::new(
                        ErrorKind::Other,
                        format!(
                            "Failed to find Secret Key for Public Key: {}",
                            Bytes48::new(pk.to_bytes())
                        ),
                    )
                })?.try_into()?
            } else {
                assert_eq!(&secret_key.sk_to_pk(), &pk);
                bls_bindings::sign(&secret_key, msg.as_ref())
            };
            assert!(verify_signature(&pk, msg.as_ref(), &signature));
            if !verify_signature(&pk, msg.as_ref(), &signature) {
                return Err(Error::new(
                    ErrorKind::Other,
                    format!(
                        "Failed to find Validate Signature for Message: {}",
                        UnsizedBytes::new(msg.as_ref())
                    ),
                ));
            }
            pk_list.push(pk_bytes);
            msg_list.push(msg.as_ref().to_vec());
            signatures.push(signature);
        }
    }
    debug!("Creating Aggregate signature");
    let sig_refs: Vec<&Signature> = signatures.iter().collect();
    let msg_list: Vec<&[u8]> = msg_list.iter().map(Vec::as_slice).collect();
    let aggsig = AggregateSignature::aggregate(&sig_refs, true)
        .map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Failed to aggregate signatures: {e:?}"),
            )
        })?
        .to_signature();
    assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig));
    Ok(SpendBundle {
        coin_spends,
        aggregated_signature: Bytes96::from(aggsig),
    })
}

pub async fn partial_signature<F, Fut>(
    coin_spends: Vec<CoinSpend>,
    key_fn: F,
    pre_calculated_signatures: HashMap<(Bytes48, Message), Bytes96>,
    additional_data: &[u8],
    max_cost: u64,
) -> Result<SpendBundle, Error>
where
    F: Fn(&Bytes48) -> Fut,
    Fut: Future<Output = Result<SecretKey, Error>>,
{
    let mut signatures: Vec<Signature> = vec![];
    let mut pk_list: Vec<Bytes48> = vec![];
    let mut msg_list: Vec<Vec<u8>> = vec![];
    debug!("Creating Signatures for Coin Spends");
    for coin_spend in &coin_spends {
        //Get AGG_SIG conditions
        let conditions_dict = conditions_dict_for_solution::<RandomState>(
            &coin_spend.puzzle_reveal,
            &coin_spend.solution,
            max_cost,
        )?
        .0;
        //Create signature
        let mut total_messages = 0;
        let mut signed_messages = 0;
        for (pk_bytes, msg) in
            pkm_pairs_for_conditions_dict(&conditions_dict, coin_spend.coin, additional_data)?
        {
            let pk = PublicKey::from_bytes(pk_bytes.as_ref()).map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!(
                        "Failed to parse Public key: {}, {:?}",
                        hex::encode(pk_bytes),
                        e
                    ),
                )
            })?;
            total_messages += 1;
            if let Ok(secret_key) = (key_fn)(&pk_bytes).await {
                let signature = if secret_key.sk_to_pk() != pk {
                    if let Some(signature) = pre_calculated_signatures.get(&(pk_bytes, msg)) {
                        Some(signature.try_into()?)
                    } else {
                        None
                    }
                } else {
                    Some(bls_bindings::sign(&secret_key, msg.as_ref()))
                };
                if let Some(signature) = signature {
                    info!("Signing Partial Message");
                    if !verify_signature(&pk, msg.as_ref(), &signature) {
                        return Err(Error::new(
                            ErrorKind::Other,
                            format!(
                                "Failed to find Validate Signature for Message: {}",
                                UnsizedBytes::new(msg.as_ref())
                            ),
                        ));
                    }
                    signed_messages += 1;
                    pk_list.push(pk_bytes);
                    msg_list.push(msg.as_ref().to_vec());
                    signatures.push(signature);
                } else {
                    warn!("Got Secret Key but No Signature for Partial Message");
                }
            }
        }
        info!("Signed {}/{} messages", signed_messages, total_messages);
    }
    let spend_bundle = if !signatures.is_empty() {
        info!("Creating Aggregate signature");
        let sig_refs: Vec<&Signature> = signatures.iter().collect();
        let msg_list: Vec<&[u8]> = msg_list.iter().map(Vec::as_slice).collect();
        let aggsig = AggregateSignature::aggregate(&sig_refs, true)
            .map_err(|e| {
                Error::new(
                    ErrorKind::Other,
                    format!("Failed to aggregate signatures: {e:?}"),
                )
            })?
            .to_signature();
        if !aggregate_verify_signature(&pk_list, &msg_list, &aggsig) {
            return Err(Error::new(
                ErrorKind::Other,
                "Failed to Validate Aggregate Signature",
            ));
        }
        assert!(aggregate_verify_signature(&pk_list, &msg_list, &aggsig));
        SpendBundle {
            coin_spends,
            aggregated_signature: Bytes96::from(aggsig),
        }
    } else {
        info!("Empty Signature List");
        SpendBundle {
            coin_spends,
            aggregated_signature: Bytes96::default(),
        }
    };
    Ok(spend_bundle)
}
