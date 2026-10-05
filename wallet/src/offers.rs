mod encoding;

use crate::assets::{AssetCoin, AssetKind, ParsedAsset, parse_asset, validate_tree};
use crate::common::sign_coin_spends;
use crate::conditions::{Conditions, announcement_id};
use crate::memory_wallet::MemoryWallet;
use crate::{Wallet, WalletStore};
use dg_xch_core::blockchain::{
    coin::Coin, coin_record::CoinRecord, coin_spend::CoinSpend, sized_bytes::Bytes32,
    spend_bundle::SpendBundle,
};
use dg_xch_core::clvm::{program::Program, sexp::SExp};
use dg_xch_core::traits::SizedBytes;
use dg_xch_puzzles::{
    cats::{CatCoin, CatSpend, puzzle_for_cat, spend_ring},
    programs::SETTLEMENT_PAYMENT_PROGRAM,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Error;

const MAX_OFFER_BYTES: usize = 1024 * 1024;
const MAX_SPENDS: usize = 64;
const MAX_COST: u64 = 500_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OfferAsset {
    Xch,
    Cat2(Bytes32),
}

impl Ord for OfferAsset {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Xch, Self::Xch) => std::cmp::Ordering::Equal,
            (Self::Xch, Self::Cat2(_)) => std::cmp::Ordering::Less,
            (Self::Cat2(_), Self::Xch) => std::cmp::Ordering::Greater,
            (Self::Cat2(a), Self::Cat2(b)) => a.bytes().cmp(&b.bytes()),
        }
    }
}

impl PartialOrd for OfferAsset {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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

#[derive(Clone)]
struct Input {
    coin: Coin,
    asset: OfferAsset,
    owner: Bytes32,
    cat: Option<CatCoin>,
}

struct Offer {
    bundle: SpendBundle,
    requested: BTreeMap<OfferAsset, Vec<SExp<'static>>>,
    offered: Vec<Input>,
}

fn settlement(asset: OfferAsset) -> Program<'static> {
    match asset {
        OfferAsset::Xch => SETTLEMENT_PAYMENT_PROGRAM.to_owned(),
        OfferAsset::Cat2(id) => puzzle_for_cat(id, &SETTLEMENT_PAYMENT_PROGRAM),
    }
}

fn settlement_asset(puzzle: &Program<'_>) -> Result<OfferAsset, Error> {
    if puzzle.tree_hash() == SETTLEMENT_PAYMENT_PROGRAM.tree_hash() {
        return Ok(OfferAsset::Xch);
    }
    let (module, args) = puzzle.uncurry()?;
    if module.tree_hash() != dg_xch_puzzles::cats::CAT_2_TREE_HASH || !args.sexp().arg_count_is(3) {
        return Err(Error::other("offers currently support XCH and CAT2 only"));
    }
    let args = dg_xch_puzzles::cats::CatPuzzleCurriedArgs::try_from(args.sexp())?;
    if args.mod_hash != dg_xch_puzzles::cats::CAT_2_TREE_HASH
        || args.inner_puzzle.tree_hash() != SETTLEMENT_PAYMENT_PROGRAM.tree_hash()
    {
        return Err(Error::other("unsupported CAT settlement puzzle"));
    }
    Ok(OfferAsset::Cat2(args.tail_program_hash))
}

fn payment_total(payments: &[SExp<'static>]) -> Result<u64, Error> {
    let mut amount = 0u64;
    for payment in payments {
        let nonce = payment.first()?;
        Bytes32::try_from(nonce)?;
        let entries = payment.rest()?;
        for entry in entries.ref_list() {
            let fields = entry.ref_list();
            if !(2..=3).contains(&fields.len()) || !entry.arg_count_is(fields.len()) {
                return Err(Error::other("invalid offer payment"));
            }
            Bytes32::try_from(fields[0])?;
            let value = fields[1]
                .as_int()?
                .to_u64()
                .filter(|v| *v > 0)
                .ok_or_else(|| Error::other("invalid offer amount"))?;
            amount = amount
                .checked_add(value)
                .ok_or_else(|| Error::other("requested offer amount overflow"))?;
        }
        if !entries.arg_count_is(entries.ref_list().len()) {
            return Err(Error::other("invalid payment list"));
        }
    }
    Ok(amount)
}

fn offered_coins(bundle: &SpendBundle) -> Result<Vec<Input>, Error> {
    let spent: HashSet<_> = bundle.removals().iter().map(Coin::name).collect();
    if spent.len() != bundle.coin_spends.len() {
        return Err(Error::other("offer repeats an input coin"));
    }
    let mut offered = Vec::new();
    let owners = HashSet::from([SETTLEMENT_PAYMENT_PROGRAM.tree_hash()]);
    let mut remaining = MAX_COST;
    for parent in &bundle.coin_spends {
        let puzzle = parent.puzzle_reveal.to_program()?;
        if puzzle.tree_hash() != parent.coin.puzzle_hash {
            return Err(Error::other("offer input puzzle hash mismatch"));
        }
        let (module, _) = puzzle.uncurry()?;
        let (additions, cost) = parent.compute_additions_with_cost(remaining)?;
        remaining = remaining
            .checked_sub(cost)
            .ok_or_else(|| Error::other("offer exceeds execution budget"))?;
        if additions.len() > 4096 {
            return Err(Error::other("offer has too many outputs"));
        }
        for coin in additions {
            if spent.contains(&coin.name()) {
                continue;
            }
            if coin.puzzle_hash == SETTLEMENT_PAYMENT_PROGRAM.tree_hash() {
                offered.push(Input {
                    coin,
                    asset: OfferAsset::Xch,
                    owner: coin.puzzle_hash,
                    cat: None,
                });
            } else if module.tree_hash() == dg_xch_puzzles::cats::CAT_2_TREE_HASH
                && let Some(cat) = CatCoin::parse_child(coin, parent, &owners, MAX_COST)?
            {
                offered.push(Input {
                    coin,
                    asset: OfferAsset::Cat2(cat.asset_id),
                    owner: cat.inner_puzzle_hash,
                    cat: Some(cat),
                });
            }
        }
    }
    if offered.len() > MAX_SPENDS {
        return Err(Error::other("offer has too many settlement coins"));
    }
    Ok(offered)
}

fn decode(text: &str) -> Result<Offer, Error> {
    let mut bundle = encoding::decode(text)?;
    if bundle.coin_spends.is_empty() || bundle.coin_spends.len() > MAX_SPENDS {
        return Err(Error::other("offer must contain 1 to 64 spends"));
    }
    let mut requested: BTreeMap<OfferAsset, Vec<SExp<'static>>> = BTreeMap::new();
    let mut actual = Vec::new();
    for spend in bundle.coin_spends {
        if spend.puzzle_reveal.as_ref().len() > 256 * 1024
            || spend.solution.as_ref().len() > 256 * 1024
        {
            return Err(Error::other("offer puzzle exceeds size limit"));
        }
        let puzzle = spend.puzzle_reveal.to_program()?;
        let solution = spend.solution.to_program()?;
        validate_tree(puzzle.sexp())?;
        validate_tree(solution.sexp())?;
        if spend.coin.parent_coin_info == Bytes32::default() {
            if spend.coin.amount != 0 || spend.coin.puzzle_hash != puzzle.tree_hash() {
                return Err(Error::other("invalid requested payment placeholder"));
            }
            let asset = settlement_asset(&puzzle)?;
            let entries = solution.sexp().ref_list();
            if !solution.sexp().arg_count_is(entries.len()) {
                return Err(Error::other("invalid notarized payments"));
            }
            requested
                .entry(asset)
                .or_default()
                .extend(entries.into_iter().map(SExp::to_owned));
        } else {
            drop(puzzle);
            drop(solution);
            actual.push(spend);
        }
    }
    bundle.coin_spends = actual;
    if bundle.coin_spends.is_empty() {
        return Err(Error::other("offer has no inputs"));
    }
    for payments in requested.values() {
        payment_total(payments)?;
    }
    let offered = offered_coins(&bundle)?;
    Ok(Offer {
        bundle,
        requested,
        offered,
    })
}

impl Offer {
    fn terms(&self) -> Result<OfferTerms, Error> {
        let mut totals = BTreeMap::<OfferAsset, i128>::new();
        for input in &self.offered {
            *totals.entry(input.asset).or_default() += i128::from(input.coin.amount);
        }
        for (asset, payments) in &self.requested {
            *totals.entry(*asset).or_default() -= i128::from(payment_total(payments)?);
        }
        let mut result = OfferTerms {
            offered: Vec::new(),
            requested: Vec::new(),
        };
        for (asset, total) in totals {
            if total == 0 {
                continue;
            }
            let amount = u64::try_from(total.abs()).map_err(Error::other)?;
            if total > 0 {
                result.offered.push(OfferAmount { asset, amount });
            } else {
                result.requested.push(OfferAmount { asset, amount });
            }
        }
        Ok(result)
    }
}

pub fn review(text: &str) -> Result<OfferTerms, Error> {
    decode(text)?.terms()
}

pub fn inputs(text: &str) -> Result<Vec<Coin>, Error> {
    let offer = decode(text)?;
    let removed: HashSet<_> = offer.bundle.removals().iter().map(Coin::name).collect();
    let inputs: Vec<_> = offer
        .bundle
        .removals()
        .into_iter()
        .filter(|coin| !removed.contains(&coin.parent_coin_info))
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
    fn select(&self, amounts: &[OfferAmount], fee: u64) -> Result<Vec<Input>, Error> {
        let mut needs = BTreeMap::from([(OfferAsset::Xch, fee)]);
        for amount in amounts {
            let total = needs.entry(amount.asset).or_default();
            *total = total
                .checked_add(amount.amount)
                .ok_or_else(|| Error::other("offer funding overflow"))?;
        }
        let mut inputs = Vec::new();
        for (asset, needed) in needs {
            let mut total = 0u64;
            match asset {
                OfferAsset::Xch => {
                    for record in self
                        .coins
                        .iter()
                        .filter(|r| !r.spent && !self.reserved.contains(&r.coin.name()))
                    {
                        if total >= needed {
                            break;
                        }
                        total = total
                            .checked_add(record.coin.amount)
                            .ok_or_else(|| Error::other("XCH funding overflow"))?;
                        inputs.push(Input {
                            coin: record.coin,
                            asset,
                            owner: record.coin.puzzle_hash,
                            cat: None,
                        });
                    }
                }
                OfferAsset::Cat2(id) => {
                    for entry in self.assets.iter().filter(|a| {
                        a.kind == AssetKind::Cat2
                            && a.asset_id == id
                            && !a.record.spent
                            && !self.reserved.contains(&a.record.coin.name())
                    }) {
                        if total >= needed {
                            break;
                        }
                        let Some(ParsedAsset::Cat(cat)) = parse_asset(
                            &entry.record,
                            &entry.parent_spend,
                            &HashSet::from([entry.owner_puzzle_hash]),
                        )?
                        else {
                            return Err(Error::other("invalid CAT lineage"));
                        };
                        if cat.asset_id != id {
                            return Err(Error::other("CAT asset mismatch"));
                        }
                        total = total
                            .checked_add(cat.coin.amount)
                            .ok_or_else(|| Error::other("CAT funding overflow"))?;
                        inputs.push(Input {
                            coin: cat.coin,
                            asset,
                            owner: cat.inner_puzzle_hash,
                            cat: Some(cat),
                        });
                    }
                }
            }
            if total < needed {
                return Err(Error::other("insufficient unreserved coins for offer"));
            }
        }
        if inputs.len() > MAX_SPENDS
            || inputs
                .iter()
                .map(|i| i.coin.name())
                .collect::<HashSet<_>>()
                .len()
                != inputs.len()
        {
            return Err(Error::other("invalid offer input count or duplicate coin"));
        }
        Ok(inputs)
    }

    async fn sign(&self, spends: Vec<CoinSpend>) -> Result<SpendBundle, Error> {
        if spends.is_empty() {
            return Ok(SpendBundle::empty());
        }
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

    async fn build(
        &self,
        inputs: Vec<Input>,
        outputs: &[(OfferAsset, Bytes32, u64)],
        fee: u64,
        required: Vec<SExp<'static>>,
    ) -> Result<SpendBundle, Error> {
        let ids: Vec<_> = inputs.iter().map(|i| i.coin.name()).collect();
        let mut groups: BTreeMap<OfferAsset, Vec<Input>> = BTreeMap::new();
        for input in inputs {
            groups.entry(input.asset).or_default().push(input);
        }
        let mut spends = Vec::new();
        for (asset, group) in groups {
            let total = group.iter().try_fold(0u64, |sum, input| {
                sum.checked_add(input.coin.amount)
                    .ok_or_else(|| Error::other("input amount overflow"))
            })?;
            let mut payments = Conditions::new();
            let mut used = if asset == OfferAsset::Xch { fee } else { 0 };
            for (_, destination, amount) in outputs.iter().filter(|(kind, _, _)| *kind == asset) {
                used = used
                    .checked_add(*amount)
                    .ok_or_else(|| Error::other("output amount overflow"))?;
                payments = payments.create_coin(*destination, *amount, None);
            }
            let change = total
                .checked_sub(used)
                .ok_or_else(|| Error::other("insufficient offer funding"))?;
            if change > 0 {
                payments = payments.create_coin(self.change, change, Some(self.change));
            }
            if asset == OfferAsset::Xch {
                payments = payments.reserve_fee(fee);
            }
            let mut cats = Vec::new();
            for (index, input) in group.iter().enumerate() {
                let mut conditions = if index == 0 {
                    payments.clone()
                } else {
                    Conditions::new()
                };
                for id in &ids {
                    if *id != input.coin.name() {
                        conditions =
                            conditions.with(SExp::from(vec![SExp::from(64), (*id).into()]));
                    }
                }
                conditions = conditions.extend(required.clone());
                let puzzle = self
                    .wallet
                    .puzzle_for_puzzle_hash(&input.owner)
                    .await?
                    .to_owned();
                let solution =
                    dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(
                        conditions.program().sexp().to_owned(),
                    )?;
                if let Some(cat) = input.cat {
                    cats.push(CatSpend {
                        coin: input.coin,
                        asset_id: cat.asset_id,
                        lineage_proof: cat.lineage_proof(),
                        inner_puzzle: puzzle,
                        inner_solution: solution,
                    });
                } else {
                    spends.push(CoinSpend {
                        coin: input.coin,
                        puzzle_reveal: puzzle.serialized()?,
                        solution: solution.serialized()?,
                    });
                }
            }
            if !cats.is_empty() {
                spends.extend(spend_ring(&cats, MAX_COST)?);
            }
        }
        self.sign(spends).await
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
        let inputs = self.select(std::slice::from_ref(&give), fee)?;
        let mut ids: Vec<_> = inputs.iter().map(|input| input.coin.name()).collect();
        ids.sort_by_key(|a| a.bytes());
        let nonce = Program::to(ids.into_iter().map(SExp::from).collect::<Vec<_>>()).tree_hash();
        let payment = SExp::from(nonce).cons(SExp::from(vec![SExp::from(vec![
            SExp::from(self.change),
            SExp::from(receive.amount),
            SExp::from(vec![SExp::from(self.change)]),
        ])]));
        let requested_puzzle = settlement(receive.asset);
        let assertion = announcement_id(requested_puzzle.tree_hash(), payment.tree_hash().as_ref());
        let required = vec![SExp::from(vec![SExp::from(63), assertion.into()])];
        let maker_bundle = self
            .build(
                inputs,
                &[(
                    give.asset,
                    SETTLEMENT_PAYMENT_PROGRAM.tree_hash(),
                    give.amount,
                )],
                fee,
                required,
            )
            .await?;
        let mut exported = maker_bundle.clone();
        exported.coin_spends.push(CoinSpend {
            coin: Coin {
                parent_coin_info: Bytes32::default(),
                puzzle_hash: requested_puzzle.tree_hash(),
                amount: 0,
            },
            puzzle_reveal: requested_puzzle.serialized()?,
            solution: Program::to(vec![payment]).serialized()?,
        });
        let text = encoding::encode(&exported)?;
        review(&text)?;
        Ok(PreparedOffer { text, maker_bundle })
    }

    pub async fn take(&self, text: &str, fee: u64) -> Result<SpendBundle, Error> {
        let mut offer = decode(text)?;
        let terms = offer.terms()?;
        if terms.offered.is_empty() {
            return Err(Error::other("offer gives the taker no assets"));
        }
        let selected = self.select(&terms.requested, fee)?;
        let outputs: Vec<_> = terms
            .requested
            .iter()
            .map(|a| (a.asset, SETTLEMENT_PAYMENT_PROGRAM.tree_hash(), a.amount))
            .collect();
        let own = self.build(selected, &outputs, fee, Vec::new()).await?;
        offer.offered.extend(offered_coins(&own)?);
        let mut groups: BTreeMap<OfferAsset, Vec<Input>> = BTreeMap::new();
        for input in offer.offered {
            groups.entry(input.asset).or_default().push(input);
        }
        let mut settlement_spends = Vec::new();
        for (asset, group) in groups {
            let mut payments = offer.requested.remove(&asset).unwrap_or_default();
            if let Some(profit) = terms.offered.iter().find(|a| a.asset == asset) {
                let nonce = Program::to(
                    group
                        .iter()
                        .map(|input| SExp::from(input.coin.name()))
                        .collect::<Vec<_>>(),
                )
                .tree_hash();
                payments.push(SExp::from(nonce).cons(SExp::from(vec![SExp::from(vec![
                    SExp::from(self.change),
                    SExp::from(profit.amount),
                    SExp::from(vec![SExp::from(self.change)]),
                ])])));
            }
            let mut cats = Vec::new();
            for (index, input) in group.into_iter().enumerate() {
                let solution = Program::to(if index == 0 {
                    payments.clone()
                } else {
                    Vec::new()
                });
                if let Some(cat) = input.cat {
                    cats.push(CatSpend {
                        coin: input.coin,
                        asset_id: cat.asset_id,
                        lineage_proof: cat.lineage_proof(),
                        inner_puzzle: SETTLEMENT_PAYMENT_PROGRAM.to_owned(),
                        inner_solution: solution,
                    });
                } else {
                    settlement_spends.push(CoinSpend {
                        coin: input.coin,
                        puzzle_reveal: SETTLEMENT_PAYMENT_PROGRAM.serialized()?,
                        solution: solution.serialized()?,
                    });
                }
            }
            if !cats.is_empty() {
                settlement_spends.extend(spend_ring(&cats, MAX_COST)?);
            }
        }
        if !offer.requested.is_empty() {
            return Err(Error::other("unfunded offer payments"));
        }
        let mut completed = SpendBundle::aggregate(vec![offer.bundle, own])?;
        completed.coin_spends.extend(settlement_spends);
        completed.validate(
            Some(MAX_COST),
            0,
            &self.wallet.wallet_info().constants,
            false,
        )?;
        Ok(completed)
    }

    pub async fn cancel(&self, maker: &SpendBundle, fee: u64) -> Result<SpendBundle, Error> {
        let removed: HashSet<_> = maker.removals().iter().map(Coin::name).collect();
        let expected: HashSet<_> = maker
            .removals()
            .iter()
            .filter(|coin| !removed.contains(&coin.parent_coin_info))
            .map(Coin::name)
            .collect();
        let coins: Vec<_> = self
            .coins
            .iter()
            .filter(|r| !r.spent && expected.contains(&r.coin.name()))
            .copied()
            .collect();
        let assets: Vec<_> = self
            .assets
            .iter()
            .filter(|a| !a.record.spent && expected.contains(&a.record.coin.name()))
            .cloned()
            .collect();
        let available: HashSet<_> = coins
            .iter()
            .map(|r| r.coin.name())
            .chain(assets.iter().map(|a| a.record.coin.name()))
            .collect();
        if available != expected || available.is_empty() {
            return Err(Error::other(
                "offer inputs changed or are missing; reconcile before cancellation",
            ));
        }
        let amounts: Vec<_> = coins
            .iter()
            .map(|r| OfferAmount {
                asset: OfferAsset::Xch,
                amount: r.coin.amount,
            })
            .chain(assets.iter().map(|a| OfferAmount {
                asset: OfferAsset::Cat2(a.asset_id),
                amount: a.record.coin.amount,
            }))
            .collect();
        let reserved = HashSet::new();
        let source = OfferInputs {
            coins: &coins,
            assets: &assets,
            reserved: &reserved,
            ..*self
        };
        let selected = source.select(&amounts, 0)?;
        if fee > 0 && !selected.iter().any(|i| i.asset == OfferAsset::Xch) {
            return Err(Error::other("cancellation fee needs XCH inputs"));
        }
        let bundle = source.build(selected, &[], fee, Vec::new()).await?;
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

    fn record(coin: dg_xch_core::blockchain::coin::Coin) -> CoinRecord {
        CoinRecord {
            coin,
            confirmed_block_index: 1,
            spent_block_index: 0,
            spent: false,
            coinbase: false,
            timestamp: 1,
        }
    }

    #[tokio::test]
    async fn xch_cat_offer_round_trip_passes_native_consensus() {
        let maker = wallet(81);
        let taker = wallet(82);
        let maker_hash = maker.get_puzzle_hash(false).await.unwrap();
        let taker_hash = taker.get_puzzle_hash(false).await.unwrap();
        let mut simulator = dg_xch_simulator_lib::coinset::CoinsetSimulator::new();
        let maker_coins = [record(simulator.new_coin(maker_hash, 1000))];
        let mint_coin = record(simulator.new_coin(taker_hash, 1000));
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
        simulator.new_transaction(mint.clone()).unwrap();
        let mut assets = Vec::new();
        for spend in &mint.coin_spends {
            for addition in spend.compute_additions_with_cost(MAX_COST).unwrap().0 {
                if let Some(asset) = discover_asset(
                    record(addition),
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
                .new_transaction(prepared.maker_bundle.clone())
                .is_err()
        );
        let taker_inputs = OfferInputs {
            wallet: &taker,
            coins: &[],
            assets: &assets,
            reserved: &empty,
            change: taker_hash,
        };
        let references: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/reference_offers.json")).unwrap();
        for (text, inputs) in [
            (references["forward"].as_str().unwrap(), &taker_inputs),
            (references["reverse"].as_str().unwrap(), &maker_inputs),
        ] {
            let completed = inputs.take(text, 0).await.unwrap();
            simulator.clone().new_transaction(completed).unwrap();
        }
        let completed = taker_inputs.take(&prepared.text, 0).await.unwrap();
        let received = completed
            .additions()
            .unwrap()
            .into_iter()
            .find(|coin| {
                coin.puzzle_hash == crate::assets::cat_puzzle_hash(AssetKind::Cat2, cat, maker_hash)
            })
            .unwrap();
        let memos: HashMap<_, _> = completed
            .coin_spends
            .iter()
            .flat_map(|spend| crate::compute_memos_for_spend(spend).unwrap())
            .collect();
        assert_eq!(memos[&received.name()], vec![maker_hash.bytes().to_vec()]);
        let reverse = taker_inputs.make(receive, give, 0).await.unwrap();
        let reverse_completed = maker_inputs.take(&reverse.text, 1).await.unwrap();
        simulator
            .clone()
            .new_transaction(reverse_completed.clone())
            .unwrap();
        let reverse_cancellation = taker_inputs.cancel(&reverse.maker_bundle, 0).await.unwrap();
        let mut reverse_cancelled = simulator.clone();
        reverse_cancelled
            .new_transaction(reverse_cancellation.clone())
            .unwrap();
        assert!(
            reverse_cancelled
                .new_transaction(reverse_completed.clone())
                .is_err()
        );
        let cancellation = maker_inputs
            .cancel(&prepared.maker_bundle, 1)
            .await
            .unwrap();
        let mut cancelled = simulator.clone();
        cancelled.new_transaction(cancellation.clone()).unwrap();
        assert!(cancelled.new_transaction(completed.clone()).is_err());
        simulator.new_transaction(completed.clone()).unwrap();
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
