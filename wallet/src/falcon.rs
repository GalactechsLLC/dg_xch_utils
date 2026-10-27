//! Falcon wallet authorization for native coins and shared CAT/NFT builders.
use crate::{
    conditions::{Conditions, announcement_id},
    signing::TransactionWallet,
};
use async_trait::async_trait;
use dg_xch_core::{
    blockchain::{
        coin::Coin, coin_record::CoinRecord, coin_spend::CoinSpend, sized_bytes::Bytes32,
        spend_bundle::SpendBundle,
    },
    clvm::{program::Program, sexp::SExp},
    consensus::{
        block_generator::{conditions_from_spend_bundle, validate_block_authorization},
        constants::ConsensusConstants,
    },
    signatures::{
        SignatureSigner,
        extension::FalconAuthorization,
        falcon::{FalconPublicKey, FalconSigner},
        falcon_condition::transaction_reference,
    },
};
use std::{
    collections::{HashMap, HashSet},
    io::Error,
    sync::Arc,
};

fn quote(value: SExp<'static>) -> SExp<'static> {
    SExp::from(1u8).cons(value)
}
fn cons(first: SExp<'static>, rest: SExp<'static>) -> SExp<'static> {
    SExp::from(vec![SExp::from(4u8), first, rest])
}

/// Commit to the entire conditions tree, including outputs, fees and announcements.
/// The existing delegated-conditions module is key-format agnostic. The wrapper
/// adds Falcon's required empty direct flag to its first AGG_SIG_ME condition.
pub fn puzzle(public_key: &FalconPublicKey) -> Program<'static> {
    let delegated = dg_xch_puzzles::programs::P2_DELEGATED_CONDITIONS_PROGRAM
        .curry(&[Program::to(public_key.to_vec())]);
    // The intermediate environment is ((50 public_key tree_hash) . conditions).
    let signature = cons(
        4u8.into(),
        cons(
            10u8.into(),
            cons(22u8.into(), quote(SExp::from(vec![SExp::from(0)]))),
        ),
    );
    let adapt = cons(signature, 3u8.into());
    Program::to(vec![
        SExp::from(2),
        quote(adapt),
        SExp::from(vec![
            SExp::from(2),
            quote(delegated.sexp().to_owned()),
            SExp::from(1),
        ]),
    ])
}

/// A single signing identity. Keys retain FalconSigner's zeroizing storage.
/// Coin discovery/reservation is supplied by the caller, as in the asset builder.
pub struct FalconWallet {
    signer: Arc<FalconSigner>,
    constants: Arc<ConsensusConstants>,
    puzzle: Program<'static>,
}

#[derive(Clone, Copy, Debug)]
pub struct Payment {
    pub puzzle_hash: Bytes32,
    pub amount: u64,
}

pub struct WalletSnapshot {
    pub height: u32,
    pub native: Vec<CoinRecord>,
    pub assets: Vec<crate::assets::AssetCoin>,
}

impl FalconWallet {
    pub fn new(
        signer: impl Into<Arc<FalconSigner>>,
        constants: Arc<ConsensusConstants>,
    ) -> Result<Self, Error> {
        if constants.signature_profile.is_bls() {
            return Err(Error::other("Falcon wallet requires Falcon chain rules"));
        }
        let signer = signer.into();
        let puzzle = puzzle(&signer.public_key());
        Ok(Self {
            signer,
            constants,
            puzzle,
        })
    }

    pub fn puzzle_hash(&self) -> Bytes32 {
        self.puzzle.tree_hash()
    }

    /// Discover this identity's native coins and hinted assets through node RPC.
    /// Asset ownership is checked against the executed parent, not just the hint.
    pub async fn sync(
        &self,
        client: &dg_xch_clients::rpc::full_node::FullnodeClient,
    ) -> Result<WalletSnapshot, Error> {
        use dg_xch_clients::rpc::full_node::FullnodeAPI;
        let genesis = client.get_block_record_by_height(0).await?;
        if genesis.prev_hash != self.constants.genesis_challenge {
            return Err(Error::other("wallet node is on a different chain"));
        }
        let height = client
            .get_blockchain_state()
            .await?
            .peak
            .ok_or_else(|| Error::other("wallet node has no peak"))?
            .height;
        let owner = self.puzzle_hash();
        let native = client
            .get_coin_records_by_puzzle_hash(&owner, Some(false), None, None)
            .await?;
        let mut assets = Vec::new();
        for record in client
            .get_coin_records_by_hint(&owner, Some(false), None, None)
            .await?
        {
            if record.coin.puzzle_hash == owner {
                continue;
            }
            let parent = client
                .get_coin_record_by_name(&record.coin.parent_coin_info)
                .await?
                .ok_or_else(|| Error::other("asset parent is missing"))?;
            if !parent.spent {
                return Err(Error::other("asset parent is unspent"));
            }
            let parent_spend = client
                .get_puzzle_and_solution(&parent.coin.name(), parent.spent_block_index)
                .await?;
            if let Some(asset) =
                crate::assets::discover_asset(record, parent_spend, &HashSet::from([owner]))?
            {
                assets.push(asset);
            }
        }
        Ok(WalletSnapshot {
            height,
            native,
            assets,
        })
    }

    /// Move rewards from the original development wallet into the full wallet.
    pub fn import_reward(&self, coin: Coin, fee: u64, height: u32) -> Result<SpendBundle, Error> {
        let amount = coin
            .amount
            .checked_sub(fee)
            .filter(|amount| *amount > 0)
            .ok_or_else(|| Error::other("reward does not cover fee"))?;
        Ok(dg_xch_core::signatures::falcon_wallet::spend(
            &self.signer,
            coin,
            self.puzzle_hash(),
            amount,
            &self.constants,
            height,
        )?
        .bundle)
    }

    /// Spend selected native coins, creating all requested outputs and exact change.
    pub async fn payment(
        &self,
        inputs: &[CoinRecord],
        outputs: &[Payment],
        fee: u64,
        height: u32,
    ) -> Result<SpendBundle, Error> {
        if inputs.is_empty() || outputs.is_empty() || outputs.iter().any(|p| p.amount == 0) {
            return Err(Error::other("payment needs inputs and positive outputs"));
        }
        let mut ids = HashSet::new();
        let mut total = 0u128;
        for record in inputs {
            if record.spent
                || record.coin.puzzle_hash != self.puzzle_hash()
                || !ids.insert(record.coin.name())
            {
                return Err(Error::other("spent, duplicate or unowned payment input"));
            }
            total += u128::from(record.coin.amount);
        }
        let needed = outputs.iter().map(|p| u128::from(p.amount)).sum::<u128>() + u128::from(fee);
        let change = total
            .checked_sub(needed)
            .ok_or_else(|| Error::other("insufficient funds"))?;
        let change = u64::try_from(change).map_err(|_| Error::other("change exceeds one coin"))?;
        let mut conditions = Conditions::new().reserve_fee(fee);
        for output in outputs {
            // Native coins are indexed by their puzzle hash; asset hints would
            // repeat the same hash without helping wallet discovery.
            conditions = conditions.create_coin(output.puzzle_hash, output.amount, None);
        }
        if change > 0 {
            conditions = conditions.create_coin(self.puzzle_hash(), change, None);
        }
        let mut spends = Vec::with_capacity(inputs.len());
        for (index, record) in inputs.iter().enumerate() {
            let next = inputs[(index + 1) % inputs.len()].coin.name();
            // AGG_SIG_ME already binds a single input. An announcement ring is
            // necessary only when multiple inputs must be spent together.
            let mut bound = Conditions::new();
            if inputs.len() > 1 {
                bound = bound
                    .create_coin_announcement(b"dgx-wallet-inputs".to_vec())
                    .assert_coin_announcement(announcement_id(next, b"dgx-wallet-inputs"));
            }
            if index == 0 {
                bound = bound.extend(conditions.clone());
            }
            spends.push(CoinSpend {
                coin: record.coin,
                puzzle_reveal: self.puzzle.serialized()?,
                solution: self.conditions_solution(bound.program())?.serialized()?,
            });
        }
        self.authorize(spends, height).await
    }
}

#[async_trait]
impl TransactionWallet for FalconWallet {
    fn consensus_constants(&self) -> &ConsensusConstants {
        &self.constants
    }
    async fn owner_puzzle(&self, puzzle_hash: &Bytes32) -> Result<Program<'static>, Error> {
        if *puzzle_hash != self.puzzle_hash() {
            return Err(Error::other("wallet does not own puzzle"));
        }
        Ok(self.puzzle.clone())
    }

    fn conditions_solution(&self, conditions: Program<'static>) -> Result<Program<'static>, Error> {
        Ok(Program::to(vec![conditions.sexp().to_owned()]))
    }

    async fn authorize(
        &self,
        coin_spends: Vec<CoinSpend>,
        height: u32,
    ) -> Result<SpendBundle, Error> {
        let mut bundle = SpendBundle {
            coin_spends,
            ..SpendBundle::empty()
        };
        let conditions =
            conditions_from_spend_bundle(&bundle, height, &self.constants).map_err(Error::other)?;
        let signatures = conditions
            .falcon_signatures
            .iter()
            .map(|requirement| {
                if requirement.public_key.const_bytes() != self.signer.public_key() {
                    return Err(Error::other("wallet does not own a required signing key"));
                }
                self.signer
                    .sign_message(requirement.message.as_slice())
                    .map_err(Error::other)
            })
            .collect::<Result<Vec<_>, _>>()?;
        bundle.aggregated_signature = transaction_reference(
            self.constants.genesis_challenge,
            &conditions.falcon_signatures,
        );
        bundle.authorization = FalconAuthorization {
            genesis: self.constants.genesis_challenge,
            header: Vec::new(),
            transactions: signatures,
            labrador: None,
        }
        .into();
        Ok(bundle)
    }
}

/// Finish a signed wallet bundle locally, replacing its Falcon witnesses with a
/// ZK proof before RPC submission. The private signing key never enters the prover.
#[cfg(feature = "labrador")]
pub async fn prove_zk(
    mut bundle: SpendBundle,
    constants: &ConsensusConstants,
    height: u32,
) -> Result<SpendBundle, Error> {
    use dg_xch_core::signatures::labrador::{self, LabradorProfile};
    if constants.falcon_aggregation != Some(LabradorProfile::Packed11ZkV2) {
        return Err(Error::other(
            "wallet ZK requires the explicit ZK chain profile",
        ));
    }
    let conditions =
        conditions_from_spend_bundle(&bundle, height, constants).map_err(Error::other)?;
    let auth = bundle
        .authorization
        .as_ref()
        .ok_or_else(|| Error::other("missing wallet authorization"))?;
    if auth.genesis != constants.genesis_challenge
        || !auth.header.is_empty()
        || auth.labrador.is_some()
        || bundle.aggregated_signature
            != transaction_reference(constants.genesis_challenge, &conditions.falcon_signatures)
    {
        return Err(Error::other("wallet ZK statement does not match bundle"));
    }
    let requirements = conditions
        .falcon_signatures
        .iter()
        .map(|c| c.requirement())
        .collect::<Vec<_>>();
    let signatures = auth.transactions.clone();
    let genesis = constants.genesis_challenge;
    let proof = tokio::task::spawn_blocking(move || {
        labrador::prove_profile(
            LabradorProfile::Packed11ZkV2,
            genesis,
            &requirements,
            &signatures,
        )
    })
    .await
    .map_err(Error::other)??;
    let auth = bundle
        .authorization
        .as_mut()
        .ok_or_else(|| Error::other("wallet authorization disappeared"))?;
    // Zeroize the removed witness bytes; there is no direct-signature fallback.
    use zeroize::Zeroize;
    auth.transactions.zeroize();
    auth.transactions.clear();
    auth.labrador = Some(proof);
    Ok(bundle)
}

/// Combine independently authorized bundles in execution order, preserving each
/// wallet's ZK statement when proofs have already replaced its signatures.
pub fn combine(
    bundles: &[SpendBundle],
    constants: &ConsensusConstants,
    height: u32,
) -> Result<SpendBundle, Error> {
    if constants.signature_profile.is_bls() || bundles.is_empty() {
        return Err(Error::other("direct Falcon bundles required"));
    }
    let mut witnesses = HashMap::new();
    #[cfg(feature = "labrador")]
    let mut evidence = dg_xch_core::signatures::labrador::Evidence::default();
    let mut combined = SpendBundle::empty();
    for bundle in bundles {
        let auth = bundle
            .authorization
            .as_ref()
            .ok_or_else(|| Error::other("missing Falcon authorization"))?;
        let conditions =
            conditions_from_spend_bundle(bundle, height, constants).map_err(Error::other)?;
        if auth.genesis != constants.genesis_challenge
            || !auth.header.is_empty()
            || bundle.aggregated_signature
                != transaction_reference(constants.genesis_challenge, &conditions.falcon_signatures)
        {
            return Err(Error::other(
                "bundle authorization does not match chain and execution",
            ));
        }
        validate_block_authorization(&conditions, &auth.transaction_witness()?, constants)
            .map_err(Error::other)?;
        #[cfg(feature = "labrador")]
        if constants.falcon_aggregation
            == Some(dg_xch_core::signatures::labrador::LabradorProfile::Packed11ZkV2)
        {
            evidence.add(
                &conditions
                    .falcon_signatures
                    .iter()
                    .map(|c| c.requirement())
                    .collect::<Vec<_>>(),
                auth.transaction_witness()?,
            )?;
        } else if auth.labrador.is_some() {
            return Err(Error::other("only ZK wallet proofs can be composed"));
        }
        for (requirement, signature) in conditions.falcon_signatures.iter().zip(&auth.transactions)
        {
            if witnesses
                .insert(
                    (requirement.coin_id, requirement.condition_index),
                    *signature,
                )
                .is_some()
            {
                return Err(Error::other("duplicate Falcon signature occurrence"));
            }
        }
        combined.coin_spends.extend(bundle.coin_spends.clone());
    }
    let conditions =
        conditions_from_spend_bundle(&combined, height, constants).map_err(Error::other)?;
    combined.aggregated_signature =
        transaction_reference(constants.genesis_challenge, &conditions.falcon_signatures);
    #[cfg(feature = "labrador")]
    if constants.falcon_aggregation
        == Some(dg_xch_core::signatures::labrador::LabradorProfile::Packed11ZkV2)
    {
        let auth = evidence.finish(
            constants.genesis_challenge,
            &conditions
                .falcon_signatures
                .iter()
                .map(|c| c.requirement())
                .collect::<Vec<_>>(),
        )?;
        validate_block_authorization(&conditions, &auth.transaction_witness()?, constants)
            .map_err(Error::other)?;
        combined.authorization = auth.into();
        return Ok(combined);
    }
    let signatures = conditions
        .falcon_signatures
        .iter()
        .map(|requirement| {
            witnesses
                .remove(&(requirement.coin_id, requirement.condition_index))
                .ok_or_else(|| Error::other("missing Falcon signature occurrence"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !witnesses.is_empty() {
        return Err(Error::other("unused Falcon signatures"));
    }
    combined.aggregated_signature =
        transaction_reference(constants.genesis_challenge, &conditions.falcon_signatures);
    combined.authorization = FalconAuthorization {
        genesis: constants.genesis_challenge,
        header: Vec::new(),
        transactions: signatures,
        labrador: None,
    }
    .into();
    Ok(combined)
}
