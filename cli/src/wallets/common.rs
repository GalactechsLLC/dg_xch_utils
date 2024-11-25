use blst::min_pk::{AggregateSignature, PublicKey, SecretKey, Signature};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96, SizedBytes};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::utils::pkm_pairs_for_conditions_dict;
use dg_xch_core::blockchain::wallet_type::WalletType;
use dg_xch_core::clvm::bls_bindings;
use dg_xch_core::clvm::bls_bindings::{aggregate_verify_signature, verify_signature};
use dg_xch_core::clvm::condition_utils::{conditions_dict_for_solution};
use dg_xch_core::consensus::constants::ConsensusConstants;
use num_traits::cast::ToPrimitive;
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
    constants: &ConsensusConstants,
) -> Result<SpendBundle, Error>
where
    F: Fn(&Bytes48) -> Fut,
    Fut: Future<Output = Result<SecretKey, Error>>,
{
    sign_coin_spends(
        vec![coin_spend],
        key_fn,
        &constants.agg_sig_me_additional_data,
        constants.max_block_cost_clvm.to_u64().unwrap(),
    )
    .await
}

pub async fn sign_coin_spends<F, Fut>(
    coin_spends: Vec<CoinSpend>,
    key_fn: F,
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
    for coin_spend in &coin_spends {
        //Get AGG_SIG conditions
        let conditions_dict = conditions_dict_for_solution::<RandomState>(
            &coin_spend.puzzle_reveal,
            &coin_spend.solution,
            max_cost,
        )?
        .0;
        //Create signature
        for (pk_bytes, msg) in pkm_pairs_for_conditions_dict(
            &conditions_dict,
            coin_spend.coin,
            additional_data,
        )? {
            let pk = PublicKey::from_bytes(pk_bytes.as_slice()).map_err(|e| {
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
            assert_eq!(&secret_key.sk_to_pk(), &pk);
            let signature = bls_bindings::sign(&secret_key, &msg);
            assert!(verify_signature(&pk, &msg, &signature));
            pk_list.push(pk_bytes);
            msg_list.push(msg);
            signatures.push(signature);
        }
    }
    //Aggregate signatures
    let sig_refs: Vec<&Signature> = signatures.iter().collect();
    let pk_list: Vec<&Bytes48> = pk_list.iter().collect();
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
