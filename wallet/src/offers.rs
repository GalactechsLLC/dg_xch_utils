use crate::assets::{
    AssetCoin, AssetKind, ParsedAsset, native_coin, native_hash, parse_asset, sdk_coin, sdk_hash,
    standard_layer, validate_tree,
};
use crate::common::sign_coin_spends;
use crate::memory_wallet::MemoryWallet;
use crate::{Wallet, WalletStore};
use chia_protocol::SpendBundle as SdkBundle;
use chia_puzzle_types::offer::{NotarizedPayment, Payment};
use chia_sdk_driver::{
    Action, AssetInfo, Id, Offer, Relation, RequestedPayments, SpendContext, Spends,
};
use chia_sdk_types::Mod;
use chia_sdk_types::conditions::Memos;
use dg_xch_core::blockchain::{
    coin_record::CoinRecord, coin_spend::CoinSpend, sized_bytes::Bytes32, spend_bundle::SpendBundle,
};
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::traits::SizedBytes;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Error;

const MAX_OFFER_BYTES: usize = 1024 * 1024;
const MAX_SPENDS: usize = 64;
const MAX_COST: u64 = 500_000_000;
type SigningKeys = IndexMap<chia_protocol::Bytes32, chia_bls::PublicKey>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OfferAsset {
    Xch,
    Cat2(Bytes32),
}

impl OfferAsset {
    fn id(self) -> Id {
        match self {
            Self::Xch => Id::Xch,
            Self::Cat2(hash) => Id::Existing(sdk_hash(hash)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferAmount {
    pub asset: OfferAsset,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OfferTerms {
    pub offered: Vec<OfferAmount>,
    pub requested: Vec<OfferAmount>,
}

pub struct PreparedOffer {
    pub text: String,
    pub maker_bundle: SpendBundle,
}

fn native_bundle(bundle: &SdkBundle) -> SpendBundle {
    SpendBundle {
        coin_spends: bundle
            .coin_spends
            .iter()
            .map(|spend| CoinSpend {
                coin: native_coin(spend.coin),
                puzzle_reveal: SerializedProgram::from_bytes(spend.puzzle_reveal.as_ref()),
                solution: SerializedProgram::from_bytes(spend.solution.as_ref()),
            })
            .collect(),
        aggregated_signature: bundle.aggregated_signature.to_bytes().into(),
    }
}

fn sdk_bundle(bundle: &SpendBundle) -> Result<SdkBundle, Error> {
    Ok(SdkBundle::new(
        bundle
            .coin_spends
            .iter()
            .map(|spend| {
                chia_protocol::CoinSpend::new(
                    sdk_coin(spend.coin),
                    spend.puzzle_reveal.to_bytes().into(),
                    spend.solution.to_bytes().into(),
                )
            })
            .collect(),
        chia_bls::Signature::from_bytes(&bundle.aggregated_signature.bytes())
            .map_err(Error::other)?,
    ))
}

fn decode(ctx: &mut SpendContext, text: &str) -> Result<Offer, Error> {
    if text.len() > MAX_OFFER_BYTES {
        return Err(Error::other("offer exceeds 1 MiB"));
    }
    let bundle = chia_sdk_driver::decode_offer(text.trim()).map_err(Error::other)?;
    if bundle.coin_spends.is_empty() || bundle.coin_spends.len() > MAX_SPENDS {
        return Err(Error::other("offer must contain 1 to 64 spends"));
    }
    let mut cost = 0u64;
    let mut output_amount = 0u64;
    bundle.coin_spends.iter().try_fold(0u64, |total, spend| {
        total
            .checked_add(spend.coin.amount)
            .ok_or_else(|| Error::other("offer input amounts exceed supported range"))
    })?;
    for spend in &native_bundle(&bundle).coin_spends {
        if spend.puzzle_reveal.to_bytes().len() > 256 * 1024
            || spend.solution.to_bytes().len() > 256 * 1024
        {
            return Err(Error::other("offer puzzle exceeds size limit"));
        }
        for bytes in [spend.puzzle_reveal.to_bytes(), spend.solution.to_bytes()] {
            let root = clvmr::serde::node_from_bytes(ctx, &bytes).map_err(Error::other)?;
            validate_tree(ctx, root)?;
        }
        if spend.coin.parent_coin_info != Bytes32::default() {
            if spend.puzzle_reveal.to_program()?.tree_hash() != spend.coin.puzzle_hash {
                return Err(Error::other("offer input puzzle hash mismatch"));
            }
            let (additions, spent_cost) =
                spend.compute_additions_with_cost(MAX_COST.saturating_sub(cost))?;
            for addition in additions {
                output_amount = output_amount
                    .checked_add(addition.amount)
                    .ok_or_else(|| Error::other("offer output amounts exceed supported range"))?;
            }
            cost = cost
                .checked_add(spent_cost)
                .ok_or_else(|| Error::other("offer cost overflow"))?;
            if cost >= MAX_COST {
                return Err(Error::other("offer exceeds execution budget"));
            }
        }
    }
    let offer = Offer::from_spend_bundle(ctx, &bundle).map_err(Error::other)?;
    if offer.asset_info().cats().any(|asset| {
        offer
            .asset_info()
            .cat(*asset)
            .is_some_and(|info| info.hidden_puzzle_hash.is_some())
    }) {
        return Err(Error::other("restricted CAT offers are not supported"));
    }
    let requested = offer.requested_payments();
    if !requested.nfts.is_empty()
        || !requested.options.is_empty()
        || !offer.offered_coins().nfts.is_empty()
        || !offer.offered_coins().options.is_empty()
    {
        return Err(Error::other("offers currently support XCH and CAT2 only"));
    }
    for payments in std::iter::once(&requested.xch).chain(requested.cats.values()) {
        payments
            .iter()
            .flat_map(|payment| &payment.payments)
            .try_fold(0u64, |total, payment| {
                total
                    .checked_add(payment.amount)
                    .ok_or_else(|| Error::other("requested offer amount overflow"))
            })?;
    }
    Ok(offer)
}

pub fn review(text: &str) -> Result<OfferTerms, Error> {
    let offer = decode(&mut SpendContext::new(), text)?;
    let arbitrage = offer.arbitrage();
    let side = |side: chia_sdk_driver::ArbitrageSide| {
        let mut amounts = Vec::new();
        if side.xch > 0 {
            amounts.push(OfferAmount {
                asset: OfferAsset::Xch,
                amount: side.xch,
            });
        }
        amounts.extend(side.cats.into_iter().map(|(asset, amount)| OfferAmount {
            asset: OfferAsset::Cat2(native_hash(asset)),
            amount,
        }));
        amounts
    };
    Ok(OfferTerms {
        offered: side(arbitrage.requested),
        requested: side(arbitrage.offered),
    })
}

pub fn inputs(text: &str) -> Result<Vec<dg_xch_core::blockchain::coin::Coin>, Error> {
    let offer = decode(&mut SpendContext::new(), text)?;
    let removals = native_bundle(offer.spend_bundle()).removals();
    let identities: HashSet<_> = removals.iter().map(|coin| coin.name()).collect();
    if identities.len() != removals.len() {
        return Err(Error::other("offer repeats an input coin"));
    }
    let inputs: Vec<_> = removals
        .into_iter()
        .filter(|coin| !identities.contains(&coin.parent_coin_info))
        .collect();
    if inputs.is_empty() {
        return Err(Error::other("offer has no external input coins"));
    }
    Ok(inputs)
}

pub(crate) struct OfferInputs<'a> {
    pub wallet: &'a MemoryWallet,
    pub coins: &'a [CoinRecord],
    pub assets: &'a [AssetCoin],
    pub reserved: &'a HashSet<Bytes32>,
    pub change: Bytes32,
}

impl OfferInputs<'_> {
    async fn populate(
        &self,
        ctx: &mut SpendContext,
        spends: &mut Spends,
        amounts: &[OfferAmount],
        fee: u64,
    ) -> Result<(SigningKeys, Vec<chia_protocol::Bytes32>), Error> {
        let mut keys = IndexMap::new();
        keys.insert(
            sdk_hash(self.change),
            standard_layer(self.wallet, ctx, self.change)
                .await?
                .synthetic_key,
        );
        let mut inputs = Vec::new();
        let mut needs: HashMap<Option<Bytes32>, u64> = HashMap::from([(None, fee)]);
        for amount in amounts {
            let asset = match amount.asset {
                OfferAsset::Xch => None,
                OfferAsset::Cat2(hash) => Some(hash),
            };
            let total = needs.entry(asset).or_default();
            *total = total
                .checked_add(amount.amount)
                .ok_or_else(|| Error::other("offer funding overflow"))?;
        }
        let mut count = 0;
        for (asset, needed) in needs {
            let mut total = 0u64;
            if let Some(asset_id) = asset {
                for entry in self.assets.iter().filter(|asset| {
                    asset.kind == AssetKind::Cat2
                        && asset.asset_id == asset_id
                        && !asset.record.spent
                        && !self.reserved.contains(&asset.record.coin.name())
                }) {
                    if total >= needed {
                        break;
                    }
                    let Some(ParsedAsset::Cat(cat)) = parse_asset(
                        ctx,
                        &entry.record,
                        &entry.parent_spend,
                        &HashSet::from([entry.owner_puzzle_hash]),
                    )?
                    else {
                        return Err(Error::other("invalid CAT2 lineage"));
                    };
                    keys.insert(
                        sdk_hash(entry.owner_puzzle_hash),
                        standard_layer(self.wallet, ctx, entry.owner_puzzle_hash)
                            .await?
                            .synthetic_key,
                    );
                    spends.add(cat);
                    inputs.push(cat.coin.coin_id());
                    total = total
                        .checked_add(entry.record.coin.amount)
                        .ok_or_else(|| Error::other("CAT funding overflow"))?;
                    count += 1;
                    if count > MAX_SPENDS {
                        return Err(Error::other("offer needs too many inputs"));
                    }
                }
            } else {
                for record in self
                    .coins
                    .iter()
                    .filter(|record| !record.spent && !self.reserved.contains(&record.coin.name()))
                {
                    if total >= needed {
                        break;
                    }
                    keys.insert(
                        sdk_hash(record.coin.puzzle_hash),
                        standard_layer(self.wallet, ctx, record.coin.puzzle_hash)
                            .await?
                            .synthetic_key,
                    );
                    spends.add(sdk_coin(record.coin));
                    inputs.push(sdk_hash(record.coin.name()));
                    total = total
                        .checked_add(record.coin.amount)
                        .ok_or_else(|| Error::other("XCH funding overflow"))?;
                    count += 1;
                    if count > MAX_SPENDS {
                        return Err(Error::other("offer needs too many inputs"));
                    }
                }
            }
            if total < needed {
                return Err(Error::other("insufficient unreserved coins for offer"));
            }
        }
        Ok((keys, inputs))
    }

    async fn sign(&self, ctx: &mut SpendContext) -> Result<SpendBundle, Error> {
        let spends = ctx
            .take()
            .into_iter()
            .map(|spend| CoinSpend {
                coin: native_coin(spend.coin),
                puzzle_reveal: SerializedProgram::from_bytes(spend.puzzle_reveal.as_ref()),
                solution: SerializedProgram::from_bytes(spend.solution.as_ref()),
            })
            .collect();
        let store = self.wallet.wallet_store();
        sign_coin_spends(
            spends,
            |key| {
                let key = *key;
                let store = store.clone();
                async move { store.lock().await.secret_key_for_public_key(&key).await }
            },
            HashMap::new(),
            self.wallet
                .wallet_info()
                .constants
                .agg_sig_me_additional_data
                .as_ref(),
            MAX_COST,
        )
        .await
    }

    pub async fn make(
        &self,
        give: OfferAmount,
        receive: OfferAmount,
        fee: u64,
    ) -> Result<PreparedOffer, Error> {
        if give.amount == 0 || receive.amount == 0 || give.asset == receive.asset {
            return Err(Error::other(
                "offer needs positive amounts of two different assets",
            ));
        }
        let mut ctx = SpendContext::new();
        let mut spends = Spends::new(sdk_hash(self.change));
        let (keys, inputs) = self
            .populate(&mut ctx, &mut spends, std::slice::from_ref(&give), fee)
            .await?;
        let actions = [
            Action::send(
                give.asset.id(),
                chia_sdk_types::puzzles::SettlementPayment::mod_hash().into(),
                give.amount,
                Memos::None,
            ),
            Action::fee(fee),
        ];
        let deltas = spends.apply(&mut ctx, &actions).map_err(Error::other)?;
        let nonce = Offer::nonce(inputs);
        let payment = NotarizedPayment::new(
            nonce,
            vec![Payment::new(
                sdk_hash(self.change),
                receive.amount,
                Memos::None,
            )],
        );
        let mut requested = RequestedPayments::new();
        match receive.asset {
            OfferAsset::Xch => requested.xch.push(payment),
            OfferAsset::Cat2(hash) => {
                requested.cats.insert(sdk_hash(hash), vec![payment]);
            }
        }
        let info = AssetInfo::new();
        spends.conditions.required = spends.conditions.required.extend(
            requested
                .assertions(&mut ctx, &info)
                .map_err(Error::other)?,
        );
        spends
            .finish_with_keys(&mut ctx, &deltas, Relation::AssertConcurrent, &keys)
            .map_err(Error::other)?;
        let maker_bundle = self.sign(&mut ctx).await?;
        let offer =
            Offer::from_input_spend_bundle(&mut ctx, sdk_bundle(&maker_bundle)?, requested, info)
                .map_err(Error::other)?;
        let text =
            chia_sdk_driver::encode_offer(&offer.to_spend_bundle(&mut ctx).map_err(Error::other)?)
                .map_err(Error::other)?;
        review(&text)?;
        Ok(PreparedOffer { text, maker_bundle })
    }

    pub async fn take(&self, text: &str, fee: u64) -> Result<SpendBundle, Error> {
        let terms = review(text)?;
        if terms.offered.is_empty() {
            return Err(Error::other("offer gives the taker no assets"));
        }
        let mut ctx = SpendContext::new();
        let offer = decode(&mut ctx, text)?;
        let mut spends = Spends::new(sdk_hash(self.change));
        spends.add(offer.offered_coins().clone());
        let (keys, _) = self
            .populate(&mut ctx, &mut spends, &terms.requested, fee)
            .await?;
        let mut actions = offer.requested_payments().actions();
        actions.push(Action::fee(fee));
        let deltas = spends.apply(&mut ctx, &actions).map_err(Error::other)?;
        spends
            .finish_with_keys(&mut ctx, &deltas, Relation::AssertConcurrent, &keys)
            .map_err(Error::other)?;
        let signed = self.sign(&mut ctx).await?;
        let completed = native_bundle(&offer.take(sdk_bundle(&signed)?));
        completed.validate(
            Some(MAX_COST),
            0,
            &self.wallet.wallet_info().constants,
            false,
        )?;
        Ok(completed)
    }

    pub async fn cancel(&self, maker: &SpendBundle, fee: u64) -> Result<SpendBundle, Error> {
        let removals: HashSet<_> = maker.removals().iter().map(|coin| coin.name()).collect();
        let coins: Vec<_> = self
            .coins
            .iter()
            .filter(|record| removals.contains(&record.coin.name()) && !record.spent)
            .cloned()
            .collect();
        let assets: Vec<_> = self
            .assets
            .iter()
            .filter(|asset| removals.contains(&asset.record.coin.name()) && !asset.record.spent)
            .cloned()
            .collect();
        let expected: HashSet<_> = maker
            .removals()
            .iter()
            .filter(|coin| !removals.contains(&coin.parent_coin_info))
            .map(|coin| coin.name())
            .collect();
        let available: HashSet<_> = coins
            .iter()
            .map(|record| record.coin.name())
            .chain(assets.iter().map(|asset| asset.record.coin.name()))
            .collect();
        if available != expected {
            return Err(Error::other(
                "offer inputs changed or are missing; reconcile before cancellation",
            ));
        }
        let mut amounts = Vec::new();
        amounts.extend(coins.iter().map(|record| OfferAmount {
            asset: OfferAsset::Xch,
            amount: record.coin.amount,
        }));
        amounts.extend(assets.iter().map(|asset| OfferAmount {
            asset: OfferAsset::Cat2(asset.asset_id),
            amount: asset.record.coin.amount,
        }));
        if amounts.is_empty() {
            return Err(Error::other("offer has no unspent owned inputs to cancel"));
        }
        let mut ctx = SpendContext::new();
        let mut spends = Spends::new(sdk_hash(self.change));
        let reserved = HashSet::new();
        let inputs = OfferInputs {
            coins: &coins,
            assets: &assets,
            reserved: &reserved,
            ..*self
        };
        let (keys, _) = inputs.populate(&mut ctx, &mut spends, &amounts, 0).await?;
        let deltas = spends
            .apply(&mut ctx, &[Action::fee(fee)])
            .map_err(Error::other)?;
        spends
            .finish_with_keys(&mut ctx, &deltas, Relation::AssertConcurrent, &keys)
            .map_err(Error::other)?;
        let bundle = self.sign(&mut ctx).await?;
        bundle.validate(
            Some(MAX_COST),
            0,
            &self.wallet.wallet_info().constants,
            false,
        )?;
        Ok(bundle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{AssetAction, build_transaction, discover_asset};
    use dg_xch_clients::rpc::full_node::FullnodeClient;
    use dg_xch_core::consensus::constants::{ConsensusConstants, TESTNET_11};
    use std::sync::Arc;

    fn wallet(seed: u8) -> MemoryWallet {
        MemoryWallet::new(
            blst::min_pk::SecretKey::key_gen_v3(&[seed; 32], &[]).unwrap(),
            &FullnodeClient::new_simulator("127.0.0.1", 1, 1).unwrap(),
            Arc::new(ConsensusConstants {
                simulated: true,
                ..TESTNET_11
            }),
        )
        .unwrap()
    }

    fn record(coin: chia_protocol::Coin) -> CoinRecord {
        CoinRecord {
            coin: native_coin(coin),
            confirmed_block_index: 1,
            spent_block_index: 0,
            spent: false,
            coinbase: false,
            timestamp: 1,
        }
    }

    #[tokio::test]
    async fn xch_cat_offer_round_trip_passes_reference_consensus() {
        let maker = wallet(81);
        let taker = wallet(82);
        let maker_hash = maker.get_puzzle_hash(false).await.unwrap();
        let taker_hash = taker.get_puzzle_hash(false).await.unwrap();
        let mut simulator = chia_sdk_test::Simulator::new();
        let maker_coins = [record(simulator.new_coin(sdk_hash(maker_hash), 1000))];
        let mint_coin = record(simulator.new_coin(sdk_hash(taker_hash), 1000));
        let mint = build_transaction(
            &taker,
            &[mint_coin],
            &[],
            &HashSet::new(),
            &AssetAction::LaunchCat2 { amount: 100 },
            0,
            taker_hash,
        )
        .await
        .unwrap();
        simulator
            .new_transaction(sdk_bundle(&mint).unwrap())
            .unwrap();
        let mut assets = Vec::new();
        for spend in &mint.coin_spends {
            for addition in spend.compute_additions_with_cost(MAX_COST).unwrap().0 {
                if let Some(asset) = discover_asset(
                    record(sdk_coin(addition)),
                    spend.clone(),
                    &HashSet::from([taker_hash]),
                )
                .unwrap()
                {
                    assets.push(asset);
                }
            }
        }
        let cat = assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Cat2)
            .unwrap()
            .asset_id;
        let empty = HashSet::new();
        let maker_inputs = OfferInputs {
            wallet: &maker,
            coins: &maker_coins,
            assets: &[],
            reserved: &empty,
            change: maker_hash,
        };
        let give = OfferAmount {
            asset: OfferAsset::Xch,
            amount: 50,
        };
        let receive = OfferAmount {
            asset: OfferAsset::Cat2(cat),
            amount: 10,
        };
        let prepared = maker_inputs
            .make(give.clone(), receive.clone(), 1)
            .await
            .unwrap();
        let terms = review(&prepared.text).unwrap();
        assert_eq!(terms.offered, vec![give.clone()]);
        assert_eq!(terms.requested, vec![receive.clone()]);
        assert!(
            simulator
                .clone()
                .new_transaction(sdk_bundle(&prepared.maker_bundle).unwrap())
                .is_err()
        );
        let taker_inputs = OfferInputs {
            wallet: &taker,
            coins: &[],
            assets: &assets,
            reserved: &empty,
            change: taker_hash,
        };
        let completed = taker_inputs.take(&prepared.text, 0).await.unwrap();
        let reverse = taker_inputs.make(receive, give, 0).await.unwrap();
        let reverse_completed = maker_inputs.take(&reverse.text, 1).await.unwrap();
        simulator
            .clone()
            .new_transaction(sdk_bundle(&reverse_completed).unwrap())
            .unwrap();
        let reverse_cancellation = taker_inputs.cancel(&reverse.maker_bundle, 0).await.unwrap();
        let mut reverse_cancelled = simulator.clone();
        reverse_cancelled
            .new_transaction(sdk_bundle(&reverse_cancellation).unwrap())
            .unwrap();
        assert!(
            reverse_cancelled
                .new_transaction(sdk_bundle(&reverse_completed).unwrap())
                .is_err()
        );
        let cancellation = maker_inputs
            .cancel(&prepared.maker_bundle, 1)
            .await
            .unwrap();
        let mut cancelled = simulator.clone();
        cancelled
            .new_transaction(sdk_bundle(&cancellation).unwrap())
            .unwrap();
        assert!(
            cancelled
                .new_transaction(sdk_bundle(&completed).unwrap())
                .is_err()
        );
        simulator
            .new_transaction(sdk_bundle(&completed).unwrap())
            .unwrap();
        assert!(review("offer1invalid").is_err());
        assert!(review(&"a".repeat(MAX_OFFER_BYTES + 1)).is_err());
        let reserved = HashSet::from([maker_coins[0].coin.name()]);
        let blocked = OfferInputs {
            reserved: &reserved,
            ..maker_inputs
        };
        assert!(
            blocked
                .make(
                    OfferAmount {
                        asset: OfferAsset::Xch,
                        amount: 50
                    },
                    OfferAmount {
                        asset: OfferAsset::Cat2(cat),
                        amount: 10
                    },
                    0
                )
                .await
                .is_err()
        );
    }
}
