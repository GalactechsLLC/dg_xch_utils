use blst::min_pk::{PublicKey, Signature};
use dg_xch_core::blockchain::signage_point_or_eos::SignagePointOrEOS;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_iterations_quality_for_proof, calculate_sp_interval_iters,
};
use dg_xch_core::constants::AUG_SCHEME_DST;
use dg_xch_core::protocols::pool::PostPartialPayload;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Error;

pub fn verify_partial_signature(
    payload: &PostPartialPayload,
    authentication_key: Bytes48,
    signature: Bytes96,
) -> Result<(), Error> {
    let keys = [payload.proof_of_space.plot_public_key, authentication_key];
    let public_keys = keys
        .iter()
        .map(|key| {
            PublicKey::key_validate(key.as_ref())
                .map_err(|error| Error::other(format!("invalid partial signing key: {error:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signature = Signature::sig_validate(signature.as_ref(), true)
        .map_err(|error| Error::other(format!("invalid partial signature: {error:?}")))?;
    let message = hash_256(payload.to_bytes(ChiaProtocolVersion::Chia0_0_37)?);
    let messages: Vec<_> = keys
        .iter()
        .map(|key| [key.as_ref(), message.as_slice()].concat())
        .collect();
    let status = signature.aggregate_verify(
        true,
        &messages.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        AUG_SCHEME_DST,
        &public_keys.iter().collect::<Vec<_>>(),
        true,
    );
    if status != blst::BLST_ERROR::BLST_SUCCESS {
        return Err(Error::other("invalid aggregate partial signature"));
    }
    Ok(())
}

pub struct PartialContext<'a> {
    pub constants: &'a ConsensusConstants,
    pub signage: &'a SignagePointOrEOS,
    pub next_height: u32,
    pub previous_transaction_height: u32,
    pub contract_puzzle_hash: Bytes32,
    pub difficulty: u64,
    pub now: u64,
    pub time_limit: u32,
}

pub fn verify_partial(
    payload: &PostPartialPayload,
    context: &PartialContext<'_>,
) -> Result<Bytes32, Error> {
    let proof = &payload.proof_of_space;
    if proof.pool_contract_puzzle_hash != Some(context.contract_puzzle_hash)
        || proof.pool_public_key.is_some()
        || context.difficulty == 0
    {
        return Err(Error::other(
            "partial does not match the registered pool contract",
        ));
    }
    let signage = context.signage;
    let now = context.now as f64;
    if signage.reverted
        || !signage.time_received.is_finite()
        || signage.time_received < 0.0
        || signage.time_received > now + 5.0
        || now - signage.time_received > f64::from(context.time_limit)
    {
        return Err(Error::other(
            "partial signage point is reverted, stale, or invalid",
        ));
    }
    let challenge = if payload.end_of_sub_slot {
        let eos = signage
            .eos
            .as_ref()
            .ok_or_else(|| Error::other("missing end of subslot"))?;
        let hash = eos.challenge_chain.hash()?;
        if hash != payload.sp_hash {
            return Err(Error::other("end of subslot hash mismatch"));
        }
        hash
    } else {
        let vdf = signage
            .signage_point
            .as_ref()
            .and_then(|point| point.cc_vdf.as_ref())
            .ok_or_else(|| Error::other("missing signage point VDF"))?;
        if vdf.output.hash()? != payload.sp_hash {
            return Err(Error::other("signage point hash mismatch"));
        }
        vdf.challenge
    };
    let quality = dg_xch_pos::verify_and_get_quality_string_with_context(
        proof,
        context.constants,
        challenge,
        payload.sp_hash,
        context.next_height,
        context.previous_transaction_height,
    )
    .ok_or_else(|| Error::other("invalid or inactive proof of space"))?;
    let required = calculate_iterations_quality_for_proof(
        context.constants,
        proof,
        quality,
        context.difficulty,
        payload.sp_hash,
    );
    let limit =
        calculate_sp_interval_iters(context.constants, context.constants.pool_sub_slot_iters)?;
    if required == 0 || required >= limit {
        return Err(Error::other("proof does not meet pool difficulty"));
    }
    let mut identity = proof.to_bytes(ChiaProtocolVersion::Chia0_0_37)?;
    identity.extend_from_slice(payload.sp_hash.as_ref());
    Ok(hash_256(identity).into())
}
