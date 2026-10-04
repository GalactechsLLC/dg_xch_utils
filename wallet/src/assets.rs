use crate::common::sign_coin_spends;
use crate::conditions::{Conditions, announcement_id};
use crate::memory_wallet::MemoryWallet;
use crate::{Wallet, WalletStore};
use dg_xch_core::blockchain::{
    coin_record::CoinRecord, coin_spend::CoinSpend, sized_bytes::Bytes32, spend_bundle::SpendBundle,
};
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::clvm::{program::Program, sexp::SExp};
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Error;

const MAX_PUZZLE_BYTES: usize = 256 * 1024;
const ASSET_COST_LIMIT: u64 = 500_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    Cat1,
    Cat2,
    Nft1,
    Did(DidType),
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DidType {
    Cni,
    Julia,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetCoin {
    pub kind: AssetKind,
    pub asset_id: Bytes32,
    pub record: CoinRecord,
    #[serde(default)]
    pub reserved: bool,
    pub owner_puzzle_hash: Bytes32,
    pub parent_spend: CoinSpend,
    pub metadata: Option<String>,
    #[serde(default)]
    pub metadata_summary: Option<String>,
    pub royalty_basis_points: Option<u16>,
    pub royalty_puzzle_hash: Option<Bytes32>,
    pub did_owner: Option<Bytes32>,
}

#[derive(Clone, Debug)]
pub struct NftLaunch {
    pub data_uri: String,
    pub data_hash: Bytes32,
    pub royalty_basis_points: u16,
    pub royalty_puzzle_hash: Bytes32,
    pub edition_number: u64,
    pub edition_total: u64,
}

pub fn parse_asset_hash(input: &str) -> Result<Bytes32, Error> {
    let input = input.trim();
    let input = input.strip_prefix("0x").unwrap_or(input);
    if input.len() != 64 {
        return Err(Error::other("enter exactly 64 hexadecimal characters"));
    }
    let bytes: [u8; 32] = hex::decode(input)
        .map_err(Error::other)?
        .try_into()
        .map_err(|_| Error::other("hash must be 32 bytes"))?;
    Ok(bytes.into())
}

impl NftLaunch {
    pub fn validate(&self) -> Result<(), Error> {
        if self.data_uri.len() > 2048
            || !(self.data_uri.starts_with("https://") || self.data_uri.starts_with("ipfs://"))
            || self.data_uri.len() <= 8
            || self
                .data_uri
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || self.edition_number == 0
            || self.edition_number > self.edition_total
            || self.royalty_basis_points > 10_000
        {
            return Err(Error::other(
                "invalid NFT URI, edition or royalty (maximum 10000 basis points)",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum AssetAction {
    LaunchCat2 {
        amount: u64,
    },
    LaunchNft(NftLaunch),
    LaunchDid(DidType),
    Transfer {
        coin_id: Bytes32,
        destination: Bytes32,
        amount: u64,
    },
}

pub(crate) enum ParsedAsset {
    Cat(dg_xch_puzzles::cats::CatCoin),
    Nft(dg_xch_puzzles::nft::NftCoin),
    Did(dg_xch_puzzles::dids::DidCoin),
    Legacy(Bytes32, Bytes32),
}

pub(crate) fn validate_tree(root: &dg_xch_core::clvm::sexp::SExp<'_>) -> Result<(), Error> {
    let mut pending = vec![(root, 0u16)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 65_536 || depth > 256 {
            return Err(Error::other("asset puzzle exceeds structural limits"));
        }
        if let dg_xch_core::clvm::sexp::SExp::Pair(pair) = node {
            pending.push((pair.first(), depth + 1));
            pending.push((pair.rest(), depth + 1));
        }
    }
    Ok(())
}

pub(crate) fn cat_puzzle_hash(kind: AssetKind, asset_id: Bytes32, owner: Bytes32) -> Bytes32 {
    use dg_xch_core::curry_and_treehash::{
        calculate_hash_of_quoted_mod_hash, curry_and_treehash, shatree_atom,
    };
    let module = if kind == AssetKind::Cat1 {
        dg_xch_puzzles::cats::CAT_1_TREE_HASH
    } else {
        dg_xch_puzzles::cats::CAT_2_TREE_HASH
    };
    curry_and_treehash(
        &calculate_hash_of_quoted_mod_hash(&module),
        &[
            shatree_atom(module.as_ref()),
            shatree_atom(asset_id.as_ref()),
            owner,
        ],
    )
}

pub(crate) fn parse_asset(
    record: &CoinRecord,
    parent: &CoinSpend,
    owners: &HashSet<Bytes32>,
) -> Result<Option<ParsedAsset>, Error> {
    if parent.coin.name() != record.coin.parent_coin_info {
        return Err(Error::other("asset parent does not match coin"));
    }
    let reveal = parent.puzzle_reveal.to_bytes();
    let solution = parent.solution.to_bytes();
    if reveal.len() > MAX_PUZZLE_BYTES || solution.len() > MAX_PUZZLE_BYTES {
        return Err(Error::other("asset parent exceeds puzzle size limit"));
    }
    let native_puzzle = parent.puzzle_reveal.to_program()?;
    let native_solution = parent.solution.to_program()?;
    validate_tree(native_puzzle.sexp())?;
    validate_tree(native_solution.sexp())?;
    if native_puzzle.uncurry()?.0.tree_hash() == dg_xch_puzzles::cats::CAT_2_TREE_HASH {
        return dg_xch_puzzles::cats::CatCoin::parse_child(
            record.coin,
            parent,
            owners,
            ASSET_COST_LIMIT,
        )
        .map(|cat| cat.map(ParsedAsset::Cat));
    }
    if let Some(nft) =
        dg_xch_puzzles::nft::NftCoin::parse_child(record.coin, parent, owners, ASSET_COST_LIMIT)?
    {
        return Ok(Some(ParsedAsset::Nft(nft)));
    }
    if let Some(did) =
        dg_xch_puzzles::dids::DidCoin::parse_child(record.coin, parent, owners, ASSET_COST_LIMIT)?
    {
        return Ok(Some(ParsedAsset::Did(did)));
    }
    if native_puzzle.tree_hash() != parent.coin.puzzle_hash {
        return Err(Error::other("asset parent puzzle hash mismatch"));
    }
    let program = native_puzzle;
    let (module, args) = program.uncurry().map_err(Error::other)?;
    if module.tree_hash() == dg_xch_puzzles::cats::CAT_1_TREE_HASH {
        if !parent
            .compute_additions_with_cost(ASSET_COST_LIMIT)?
            .0
            .contains(&record.coin)
        {
            return Err(Error::other("asset is not an output of its parent"));
        }

        let args = dg_xch_puzzles::cats::CatPuzzleCurriedArgs::try_from(args.sexp())?;
        if args.mod_hash != dg_xch_puzzles::cats::CAT_1_TREE_HASH {
            return Err(Error::other("invalid CAT1 module hash"));
        }
        for owner in owners {
            use dg_xch_core::curry_and_treehash::{
                calculate_hash_of_quoted_mod_hash, curry_and_treehash, shatree_atom,
            };
            let expected = curry_and_treehash(
                &calculate_hash_of_quoted_mod_hash(&args.mod_hash),
                &[
                    shatree_atom(args.mod_hash.as_ref()),
                    shatree_atom(args.tail_program_hash.as_ref()),
                    *owner,
                ],
            );
            if expected == record.coin.puzzle_hash {
                return Ok(Some(ParsedAsset::Legacy(args.tail_program_hash, *owner)));
            }
        }
    }
    Ok(None)
}

pub fn discover_asset(
    record: CoinRecord,
    parent: CoinSpend,
    owners: &HashSet<Bytes32>,
) -> Result<Option<AssetCoin>, Error> {
    let Some(parsed) = parse_asset(&record, &parent, owners)? else {
        return Ok(None);
    };
    let (kind, asset_id, owner, metadata, royalty, royalty_hash, did) = match parsed {
        ParsedAsset::Cat(cat) => (
            AssetKind::Cat2,
            cat.asset_id,
            cat.inner_puzzle_hash,
            None,
            None,
            None,
            None,
        ),
        ParsedAsset::Legacy(id, owner) => (AssetKind::Cat1, id, owner, None, None, None, None),
        ParsedAsset::Did(did) => (
            AssetKind::Did(DidType::Cni),
            did.info.launcher_id,
            did.owner_puzzle_hash,
            Some(hex::encode(did.info.metadata.serialized()?.as_ref())),
            None,
            None,
            None,
        ),
        ParsedAsset::Nft(nft) => (
            AssetKind::Nft1,
            nft.info.launcher_id,
            nft.owner_puzzle_hash,
            Some(hex::encode(nft.info.metadata.serialized()?.as_ref())),
            Some(nft.info.royalty_basis_points),
            Some(nft.info.royalty_puzzle_hash),
            nft.info.current_owner,
        ),
    };
    let metadata_summary = metadata
        .as_ref()
        .map(|encoded| {
            let bytes = hex::decode(encoded).map_err(Error::other)?;
            Ok::<_, Error>(format!(
                "{}",
                SerializedProgram::from_bytes(&bytes).to_program()?
            ))
        })
        .transpose()?;
    Ok(Some(AssetCoin {
        kind,
        asset_id,
        record,
        reserved: false,
        owner_puzzle_hash: owner,
        parent_spend: parent,
        metadata,
        metadata_summary,
        royalty_basis_points: royalty,
        royalty_puzzle_hash: royalty_hash,
        did_owner: did,
    }))
}

fn nft_metadata(request: &NftLaunch) -> Result<Program<'static>, Error> {
    request.validate()?;
    Ok(Program::to(vec![
        SExp::from(b"u".to_vec()).cons(SExp::from(vec![SExp::from(
            request.data_uri.as_bytes().to_vec(),
        )])),
        SExp::from(b"h".to_vec()).cons(SExp::from(request.data_hash)),
        SExp::from(b"sn".to_vec()).cons(SExp::from(request.edition_number)),
        SExp::from(b"st".to_vec()).cons(SExp::from(request.edition_total)),
    ]))
}

pub(crate) async fn build_transaction(
    wallet: &MemoryWallet,
    coins: &[CoinRecord],
    assets: &[AssetCoin],
    reserved: &HashSet<Bytes32>,
    action: &AssetAction,
    fee: u64,
    change: Bytes32,
) -> Result<SpendBundle, Error> {
    let mut native_spends = Vec::new();
    let cost = match action {
        AssetAction::LaunchCat2 { amount } if *amount > 0 => *amount,
        AssetAction::LaunchNft(_) | AssetAction::LaunchDid(DidType::Cni) => 1,
        AssetAction::LaunchDid(_) => return Err(Error::other("this DID type is not implemented")),
        AssetAction::Transfer { .. } => 0,
        _ => return Err(Error::other("CAT supply must be positive")),
    };
    let needed = cost
        .checked_add(fee)
        .ok_or_else(|| Error::other("amount plus fee overflows"))?;
    let mut funding = Vec::new();
    let mut total = 0u64;
    for record in coins
        .iter()
        .filter(|record| !record.spent && !reserved.contains(&record.coin.name()))
    {
        if total >= needed {
            break;
        }
        total = total
            .checked_add(record.coin.amount)
            .ok_or_else(|| Error::other("funding amount overflow"))?;
        funding.push(record.coin);
    }
    if total < needed {
        return Err(Error::other(
            "not enough spendable XCH for issuance and fee",
        ));
    }
    let destination = change;
    let mut conditions = Conditions::new().reserve_fee(fee);
    let mut asset_anchor = None;
    match action {
        AssetAction::LaunchDid(kind) => {
            if *kind != DidType::Cni {
                return Err(Error::other("this DID type is not implemented"));
            }
            let parent = funding
                .first()
                .ok_or_else(|| Error::other("missing DID funding coin"))?;
            let inner = wallet.puzzle_for_puzzle_hash(&change).await?.to_owned();
            let (issue, spends) = dg_xch_puzzles::dids::launch_did(parent.name(), &inner)?;
            conditions = conditions.extend(issue);
            native_spends.extend(spends);
        }
        AssetAction::LaunchCat2 { amount } => {
            let parent = funding
                .first()
                .ok_or_else(|| Error::other("missing CAT funding coin"))?;
            let spend = dg_xch_puzzles::cats::issue_cat(
                parent.name(),
                *amount,
                vec![dg_xch_core::clvm::sexp::SExp::from(vec![
                    51.into(),
                    change.into(),
                    (*amount).into(),
                    dg_xch_core::clvm::sexp::SExp::from(vec![dg_xch_core::clvm::sexp::SExp::from(
                        change,
                    )]),
                ])],
                ASSET_COST_LIMIT,
            )?;
            conditions = conditions.create_coin(spend.coin.puzzle_hash, spend.coin.amount, None);
            native_spends.push(spend);
        }
        AssetAction::LaunchNft(request) => {
            let parent = funding
                .first()
                .ok_or_else(|| Error::other("missing NFT funding coin"))?;
            let (issue, spends) = dg_xch_puzzles::nft::mint_nft(
                parent.name(),
                change,
                nft_metadata(request)?,
                request.royalty_puzzle_hash,
                request.royalty_basis_points,
            )?;
            conditions = conditions.extend(issue);
            native_spends.extend(spends);
        }
        AssetAction::Transfer {
            coin_id,
            destination,
            amount,
        } => {
            let asset = assets
                .iter()
                .find(|asset| {
                    asset.record.coin.name() == *coin_id
                        && !asset.record.spent
                        && !reserved.contains(coin_id)
                })
                .ok_or_else(|| Error::other("asset is unavailable or already reserved"))?;
            if asset.kind == AssetKind::Cat1 {
                return Err(Error::other("CAT1 is read-only; transfers are disabled"));
            }
            if matches!(asset.kind, AssetKind::Did(kind) if kind != DidType::Cni) {
                return Err(Error::other("this DID type is not implemented"));
            }
            if *amount == 0 {
                return Err(Error::other("invalid asset amount"));
            }
            let owners = HashSet::from([asset.owner_puzzle_hash]);
            let parsed = parse_asset(&asset.record, &asset.parent_spend, &owners)?
                .ok_or_else(|| Error::other("asset parent could not be verified"))?;
            let mut extra = Conditions::new();
            if let Some(parent) = funding.first() {
                extra = extra
                    .assert_coin_announcement(announcement_id(parent.name(), b"dgx-asset-fee"));
                extra = extra.create_coin_announcement(b"dgx-asset".to_vec());
                asset_anchor = Some(*coin_id);
            }
            match parsed {
                ParsedAsset::Cat(cat) => {
                    if cat.asset_id != asset.asset_id {
                        return Err(Error::other("CAT asset ID does not match its parent"));
                    }
                    let mut selected = vec![(asset, cat)];
                    let mut selected_ids = HashSet::from([*coin_id]);
                    let mut asset_total = asset.record.coin.amount;
                    for candidate in assets {
                        if asset_total >= *amount {
                            break;
                        }
                        let candidate_id = candidate.record.coin.name();
                        if candidate.kind != AssetKind::Cat2
                            || candidate.asset_id != asset.asset_id
                            || candidate.record.spent
                            || reserved.contains(&candidate_id)
                            || !selected_ids.insert(candidate_id)
                        {
                            continue;
                        }
                        let owners = HashSet::from([candidate.owner_puzzle_hash]);
                        let Some(ParsedAsset::Cat(candidate_cat)) =
                            parse_asset(&candidate.record, &candidate.parent_spend, &owners)?
                        else {
                            return Err(Error::other("CAT parent could not be verified"));
                        };
                        if candidate_cat.asset_id != cat.asset_id {
                            return Err(Error::other("CAT asset ID does not match its parent"));
                        }
                        asset_total = asset_total
                            .checked_add(candidate.record.coin.amount)
                            .ok_or_else(|| Error::other("CAT amount overflow"))?;
                        selected.push((candidate, candidate_cat));
                    }
                    if asset_total < *amount {
                        return Err(Error::other("not enough spendable coins of this CAT"));
                    }
                    let memos = Some(*destination);
                    extra = extra.create_coin(*destination, *amount, memos);
                    if *amount < asset_total {
                        let memos = Some(asset.owner_puzzle_hash);
                        extra =
                            extra.create_coin(asset.owner_puzzle_hash, asset_total - amount, memos);
                    }
                    let mut spends = Vec::with_capacity(selected.len());
                    for (index, (selected_asset, selected_cat)) in selected.into_iter().enumerate()
                    {
                        let inner_puzzle = wallet
                            .puzzle_for_puzzle_hash(&selected_asset.owner_puzzle_hash)
                            .await?
                            .to_owned();
                        let conditions = if index == 0 {
                            extra.clone()
                        } else {
                            Conditions::new()
                        };
                        let conditions = conditions.program();
                        spends.push(dg_xch_puzzles::cats::CatSpend {
                            coin: selected_asset.record.coin,
                            asset_id: selected_asset.asset_id,
                            lineage_proof: selected_cat.lineage_proof(),
                            inner_puzzle,
                            inner_solution: dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(conditions.sexp().to_owned())?,
                        });
                    }
                    native_spends
                        .extend(dg_xch_puzzles::cats::spend_ring(&spends, ASSET_COST_LIMIT)?);
                }
                ParsedAsset::Nft(nft) => {
                    if *amount != nft.coin.amount {
                        return Err(Error::other("NFT transfers must move the entire coin"));
                    }
                    let info = nft.info;
                    let memos = Some(*destination);
                    extra = extra
                        .create_coin(*destination, *amount, memos)
                        .with(SExp::from(vec![
                            SExp::from(-10),
                            SExp::from(0),
                            SExp::from(0),
                            SExp::from(0),
                        ]));
                    let conditions = extra.program();
                    let lineage = nft.lineage_proof;
                    let inner = wallet
                        .puzzle_for_puzzle_hash(&asset.owner_puzzle_hash)
                        .await?
                        .to_owned();
                    let solution = dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(conditions.sexp().to_owned())?;
                    native_spends.push(info.spend(
                        asset.record.coin,
                        lineage,
                        &inner,
                        &solution,
                    )?);
                }
                ParsedAsset::Did(did) => {
                    if *amount != did.coin.amount {
                        return Err(Error::other("DID transfers must move the entire coin"));
                    }
                    let conditions = extra
                        .create_coin(
                            did.info.inner_puzzle_hash(*destination),
                            *amount,
                            Some(*destination),
                        )
                        .program();
                    let inner = wallet
                        .puzzle_for_puzzle_hash(&asset.owner_puzzle_hash)
                        .await?
                        .to_owned();
                    let solution = dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(conditions.sexp().to_owned())?;
                    native_spends.push(did.info.spend(
                        asset.record.coin,
                        did.lineage_proof,
                        &inner,
                        &solution,
                    )?);
                }
                ParsedAsset::Legacy(_, _) => return Err(Error::other("CAT1 is read-only")),
            }
        }
    }
    if total > needed {
        conditions = conditions.create_coin(destination, total - needed, None);
    }
    if let Some(asset) = asset_anchor {
        conditions = conditions
            .create_coin_announcement(b"dgx-asset-fee".to_vec())
            .assert_coin_announcement(announcement_id(asset, b"dgx-asset"));
    }
    for (index, coin) in funding.iter().enumerate() {
        let next = funding[(index + 1) % funding.len()];
        let bound = Conditions::new()
            .create_coin_announcement(b"dgx-funding".to_vec())
            .assert_coin_announcement(announcement_id(next.name(), b"dgx-funding"));
        let spend_conditions = if index == 0 {
            bound.extend(conditions.clone())
        } else {
            bound
        };
        let puzzle = wallet
            .puzzle_for_puzzle_hash(&coin.puzzle_hash)
            .await?
            .to_owned();
        let solution =
            dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::solution_for_conditions(
                spend_conditions.program().sexp().to_owned(),
            )?;
        native_spends.push(CoinSpend {
            coin: *coin,
            puzzle_reveal: puzzle.serialized()?,
            solution: solution.serialized()?,
        });
    }
    let spends = native_spends;
    let store = wallet.wallet_store();
    sign_coin_spends(
        spends,
        |key| {
            let key = *key;
            let store = store.clone();
            async move { store.lock().await.secret_key_for_public_key(&key).await }
        },
        HashMap::new(),
        wallet
            .wallet_info()
            .constants
            .agg_sig_me_additional_data
            .as_ref(),
        wallet
            .wallet_info()
            .constants
            .max_block_cost_clvm
            .to_u64()
            .ok_or_else(|| Error::other("invalid network cost limit"))?,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_clients::rpc::full_node::FullnodeClient;
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::consensus::constants::TESTNET_11;
    use dg_xch_simulator_lib::coinset::CoinsetSimulator as Simulator;
    use std::sync::Arc;

    #[test]
    fn hashes_and_nested_puzzles_are_bounded() {
        assert!(parse_asset_hash("01").is_err());
        assert!(parse_asset_hash(&"ab".repeat(33)).is_err());
        assert_eq!(
            parse_asset_hash(&"ab".repeat(32)).unwrap(),
            [0xab; 32].into()
        );
        let mut nested = dg_xch_core::constants::NULL_SEXP;
        for _ in 0..300 {
            nested = dg_xch_core::constants::NULL_SEXP.cons(nested);
        }
        assert!(validate_tree(&nested).is_err());
    }

    #[test]
    fn cat1_watch_hash_matches_legacy_puzzle_currying() {
        use dg_xch_core::clvm::program::Program;
        let asset_id = Bytes32::from([15; 32]);
        let inner = Program::new(dg_xch_core::clvm::sexp::SExp::from(vec![1u8]));
        let module_hash = dg_xch_puzzles::cats::CAT_1_TREE_HASH;
        let curried = dg_xch_puzzles::cats::CAT_1_PROGRAM.curry(&[
            Program::new(module_hash.into()),
            Program::new(asset_id.into()),
            inner.clone(),
        ]);
        assert_eq!(
            cat_puzzle_hash(AssetKind::Cat1, asset_id, inner.tree_hash()),
            curried.tree_hash()
        );
    }

    fn record(coin: Coin) -> CoinRecord {
        CoinRecord {
            coin,
            confirmed_block_index: 1,
            spent_block_index: 0,
            coinbase: false,
            timestamp: 1,
            spent: false,
        }
    }

    #[test]
    fn legacy_cat1_parent_outputs_are_discovered_read_only() {
        use dg_xch_core::clvm::program::Program;
        let owner = Bytes32::from([25; 32]);
        let asset_id = Bytes32::from([26; 32]);
        let inner = Program::new(
            SExp::from(1).cons(
                Conditions::new()
                    .create_coin(owner, 100, None)
                    .program()
                    .sexp()
                    .to_owned(),
            ),
        );
        let legacy = dg_xch_puzzles::cats::CAT_1_PROGRAM.curry(&[
            Program::new(dg_xch_puzzles::cats::CAT_1_TREE_HASH.into()),
            Program::new(asset_id.into()),
            inner.clone(),
        ]);
        let ancestor = Coin {
            parent_coin_info: [27; 32].into(),
            puzzle_hash: legacy.tree_hash(),
            amount: 100,
        };
        let coin = Coin {
            parent_coin_info: ancestor.name(),
            puzzle_hash: legacy.tree_hash(),
            amount: 100,
        };
        let solution = Program::to(vec![
            SExp::from(0),
            SExp::from(vec![
                SExp::from(ancestor.parent_coin_info),
                inner.tree_hash().into(),
                SExp::from(100),
            ]),
            coin.name().into(),
            SExp::from(coin),
            SExp::from(vec![
                SExp::from(coin.parent_coin_info),
                inner.tree_hash().into(),
                SExp::from(100),
            ]),
            SExp::from(0),
            SExp::from(0),
        ])
        .serialized()
        .unwrap();
        let parent = CoinSpend {
            coin,
            puzzle_reveal: legacy.serialized().unwrap(),
            solution,
        };
        let additions = parent
            .compute_additions_with_cost(ASSET_COST_LIMIT)
            .unwrap()
            .0;
        assert_eq!(additions.len(), 1);
        let discovered = discover_asset(record(additions[0]), parent, &HashSet::from([owner]))
            .unwrap()
            .unwrap();
        assert_eq!(discovered.kind, AssetKind::Cat1);
        assert_eq!(discovered.asset_id, asset_id);
        assert_eq!(discovered.record.coin.amount, 100);
    }

    async fn wallet() -> MemoryWallet {
        let secret = blst::min_pk::SecretKey::key_gen_v3(&[77; 32], &[]).unwrap();
        let client = FullnodeClient::new_simulator("127.0.0.1", 1, 1).unwrap();
        MemoryWallet::new(
            secret,
            &client,
            Arc::new(dg_xch_core::consensus::constants::ConsensusConstants {
                simulated: true,
                ..TESTNET_11
            }),
        )
        .unwrap()
    }

    fn validate(sim: &mut Simulator, bundle: &SpendBundle) {
        sim.new_transaction(bundle.clone()).unwrap();
    }

    fn discovered(bundle: &SpendBundle, owner: Bytes32) -> Vec<AssetCoin> {
        let mut assets = Vec::new();
        let removals: HashSet<_> = bundle
            .coin_spends
            .iter()
            .map(|spend| spend.coin.name())
            .collect();
        for parent in &bundle.coin_spends {
            for coin in parent
                .compute_additions_with_cost(ASSET_COST_LIMIT)
                .unwrap()
                .0
            {
                if removals.contains(&coin.name()) {
                    continue;
                }
                if let Some(asset) =
                    discover_asset(record(coin), parent.clone(), &HashSet::from([owner])).unwrap()
                {
                    assets.push(asset);
                }
            }
        }
        assets
    }

    #[tokio::test]
    async fn cat2_launch_and_partial_transfer_pass_native_consensus() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let coins: Vec<_> = [600, 700]
            .map(|amount| record(simulator.new_coin(owner, amount)))
            .into();
        let bundle = build_transaction(
            &wallet,
            &coins,
            &[],
            &HashSet::new(),
            &AssetAction::LaunchCat2 { amount: 1000 },
            10,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &bundle);
        let assets = discovered(&bundle, owner);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].kind, AssetKind::Cat2);
        assert_eq!(assets[0].record.coin.amount, 1000);
        assert_eq!(
            cat_puzzle_hash(AssetKind::Cat2, assets[0].asset_id, owner),
            assets[0].record.coin.puzzle_hash
        );
        let fee_coin = record(simulator.new_coin(owner, 100));
        let recipient = Bytes32::from([44; 32]);
        let transfer = build_transaction(
            &wallet,
            &[fee_coin],
            &assets,
            &HashSet::new(),
            &AssetAction::Transfer {
                coin_id: assets[0].record.coin.name(),
                destination: recipient,
                amount: 400,
            },
            7,
            owner,
        )
        .await
        .unwrap();
        let mut stripped_simulator = simulator.clone();
        validate(&mut simulator, &transfer);
        assert_eq!(discovered(&transfer, recipient)[0].record.coin.amount, 400);
        assert_eq!(discovered(&transfer, owner)[0].record.coin.amount, 600);
        let mut stripped = transfer.clone();
        stripped
            .coin_spends
            .retain(|spend| spend.coin.name() != fee_coin.coin.name());
        let error = stripped_simulator.new_transaction(stripped).unwrap_err();
        assert!(format!("{error:?}").contains("AssertAnnounceConsumedFailed"));
    }

    #[tokio::test]
    async fn cat2_transfer_combines_coins_and_excludes_reserved_inputs() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let funding = record(simulator.new_coin(owner, 1000));
        let issue = build_transaction(
            &wallet,
            &[funding],
            &[],
            &HashSet::new(),
            &AssetAction::LaunchCat2 { amount: 1000 },
            0,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &issue);
        let issued = discovered(&issue, owner);
        let split = build_transaction(
            &wallet,
            &[],
            &issued,
            &HashSet::new(),
            &AssetAction::Transfer {
                coin_id: issued[0].record.coin.name(),
                destination: owner,
                amount: 400,
            },
            0,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &split);
        let assets = discovered(&split, owner);
        assert_eq!(assets.len(), 2);
        let action = AssetAction::Transfer {
            coin_id: assets[0].record.coin.name(),
            destination: [72; 32].into(),
            amount: 900,
        };
        assert!(
            build_transaction(
                &wallet,
                &[],
                &assets,
                &HashSet::from([assets[1].record.coin.name()]),
                &action,
                0,
                owner
            )
            .await
            .is_err()
        );
        let transfer = build_transaction(&wallet, &[], &assets, &HashSet::new(), &action, 0, owner)
            .await
            .unwrap();
        validate(&mut simulator, &transfer);
        assert_eq!(
            discovered(&transfer, [72; 32].into())[0].record.coin.amount,
            900
        );
        assert_eq!(discovered(&transfer, owner)[0].record.coin.amount, 100);
    }

    #[tokio::test]
    async fn cni_did_launch_transfer_and_placeholder_rejection() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let funding = record(simulator.new_coin(owner, 100));
        assert!(
            build_transaction(
                &wallet,
                &[funding],
                &[],
                &HashSet::new(),
                &AssetAction::LaunchDid(DidType::Julia),
                0,
                owner
            )
            .await
            .is_err()
        );
        let issue = build_transaction(
            &wallet,
            &[funding],
            &[],
            &HashSet::new(),
            &AssetAction::LaunchDid(DidType::Cni),
            5,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &issue);
        let assets = discovered(&issue, owner);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].kind, AssetKind::Did(DidType::Cni));
        let destination = Bytes32::from([73; 32]);
        let transfer = build_transaction(
            &wallet,
            &[],
            &assets,
            &HashSet::new(),
            &AssetAction::Transfer {
                coin_id: assets[0].record.coin.name(),
                destination,
                amount: 1,
            },
            0,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &transfer);
        let received = discovered(&transfer, destination);
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].asset_id, assets[0].asset_id);
        let encoded = serde_json::to_vec(&received[0]).unwrap();
        let restored: AssetCoin = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.kind, AssetKind::Did(DidType::Cni));
    }

    #[tokio::test]
    async fn nft_launch_transfer_metadata_and_persistence_round_trip() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let coin = record(simulator.new_coin(owner, 100));
        let request = NftLaunch {
            data_uri: "https://example.org/art.png".into(),
            data_hash: [12; 32].into(),
            royalty_basis_points: 500,
            royalty_puzzle_hash: owner,
            edition_number: 1,
            edition_total: 10,
        };
        let bundle = build_transaction(
            &wallet,
            &[coin],
            &[],
            &HashSet::new(),
            &AssetAction::LaunchNft(request.clone()),
            5,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &bundle);
        let assets = discovered(&bundle, owner);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].kind, AssetKind::Nft1);
        assert_eq!(assets[0].royalty_basis_points, Some(500));
        assert!(
            assets[0]
                .metadata_summary
                .as_ref()
                .unwrap()
                .contains("https://example.org/art.png")
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.sqlite");
        let mut database =
            crate::storage::WalletDatabase::open(&path, &hex::encode([7; 32]), [8; 32].into())
                .await
                .unwrap();
        let mut stored = crate::storage::StoredWallet::default();
        stored.snapshot.assets = assets.clone();
        stored.snapshot.watched_cats.push([9; 32].into());
        database.save(&stored).await.unwrap();
        drop(database);
        let mut database =
            crate::storage::WalletDatabase::open(&path, &hex::encode([7; 32]), [8; 32].into())
                .await
                .unwrap();
        let loaded = database.load().await.unwrap().unwrap();
        assert_eq!(loaded.snapshot.assets[0].asset_id, assets[0].asset_id);
        assert_eq!(loaded.snapshot.watched_cats, stored.snapshot.watched_cats);
        let recipient = Bytes32::from([45; 32]);
        let transfer = build_transaction(
            &wallet,
            &[],
            &loaded.snapshot.assets,
            &HashSet::new(),
            &AssetAction::Transfer {
                coin_id: assets[0].record.coin.name(),
                destination: recipient,
                amount: 1,
            },
            0,
            owner,
        )
        .await
        .unwrap();
        validate(&mut simulator, &transfer);
        let received = discovered(&transfer, recipient);
        assert_eq!(received[0].asset_id, assets[0].asset_id);
        assert_eq!(received[0].royalty_basis_points, Some(500));
        assert_eq!(received[0].metadata, assets[0].metadata);
        assert!(received[0].did_owner.is_none());
        let mut invalid = request;
        invalid.royalty_basis_points = 10_001;
        assert!(nft_metadata(&invalid).is_err());
        invalid.royalty_basis_points = 0;
        invalid.data_uri = "file:///etc/passwd".into();
        assert!(nft_metadata(&invalid).is_err());
    }

    #[tokio::test]
    async fn spoofed_assets_reserved_coins_and_cat1_spends_are_rejected() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let coin = record(simulator.new_coin(owner, 2000));
        let bundle = build_transaction(
            &wallet,
            &[coin],
            &[],
            &HashSet::new(),
            &AssetAction::LaunchCat2 { amount: 1000 },
            0,
            owner,
        )
        .await
        .unwrap();
        let mut asset = discovered(&bundle, owner).remove(0);
        assert!(
            discover_asset(
                asset.record,
                asset.parent_spend.clone(),
                &HashSet::from([Bytes32::from([66; 32])])
            )
            .unwrap()
            .is_none()
        );
        let mut spoof = asset.record;
        spoof.coin.amount += 1;
        assert!(
            discover_asset(spoof, asset.parent_spend.clone(), &HashSet::from([owner])).is_err()
        );
        let action = AssetAction::Transfer {
            coin_id: asset.record.coin.name(),
            destination: owner,
            amount: 1000,
        };
        let reserved = HashSet::from([asset.record.coin.name()]);
        assert!(
            build_transaction(&wallet, &[], &[asset.clone()], &reserved, &action, 0, owner)
                .await
                .is_err()
        );
        asset.kind = AssetKind::Cat1;
        let error = build_transaction(&wallet, &[], &[asset], &HashSet::new(), &action, 0, owner)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("read-only"));
    }
}
