use crate::common::sign_coin_spends;
use crate::memory_wallet::MemoryWallet;
use crate::{Wallet, WalletStore};
use chia_protocol::{Bytes32 as SdkHash, Coin as SdkCoin};
use chia_sdk_driver::{
    Cat, CatSpend, Did, DidInfo, HashedPtr, Launcher, Layer, Nft, NftMint, Puzzle, SingletonInfo,
    SpendContext, SpendWithConditions, StandardLayer,
};
use chia_sdk_types::{
    Conditions,
    conditions::{Memos, TransferNft},
};
use clvmr::serde::{node_from_bytes, node_to_bytes};
use dg_xch_core::blockchain::{
    coin::Coin, coin_record::CoinRecord, coin_spend::CoinSpend, sized_bytes::Bytes32,
    spend_bundle::SpendBundle,
};
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::traits::SizedBytes;
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

pub(crate) fn sdk_hash(hash: Bytes32) -> SdkHash {
    hash.bytes().into()
}
pub(crate) fn native_hash(hash: SdkHash) -> Bytes32 {
    hash.to_bytes().into()
}
pub(crate) fn sdk_coin(coin: Coin) -> SdkCoin {
    SdkCoin::new(
        sdk_hash(coin.parent_coin_info),
        sdk_hash(coin.puzzle_hash),
        coin.amount,
    )
}
pub(crate) fn native_coin(coin: SdkCoin) -> Coin {
    Coin {
        parent_coin_info: native_hash(coin.parent_coin_info),
        puzzle_hash: native_hash(coin.puzzle_hash),
        amount: coin.amount,
    }
}

pub(crate) enum ParsedAsset {
    Cat(Cat),
    Nft(Nft),
    Did(Did),
    Legacy(Bytes32, Bytes32),
}

pub(crate) fn validate_tree(ctx: &SpendContext, root: clvmr::NodePtr) -> Result<(), Error> {
    let mut pending = vec![(root, 0u16)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 65_536 || depth > 256 {
            return Err(Error::other("asset puzzle exceeds structural limits"));
        }
        if let clvmr::SExp::Pair(left, right) = ctx.sexp(node) {
            pending.push((left, depth + 1));
            pending.push((right, depth + 1));
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
    ctx: &mut SpendContext,
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
    let puzzle_ptr = node_from_bytes(ctx, &reveal).map_err(Error::other)?;
    let solution_ptr = node_from_bytes(ctx, &solution).map_err(Error::other)?;
    validate_tree(ctx, puzzle_ptr)?;
    validate_tree(ctx, solution_ptr)?;
    let puzzle = Puzzle::parse(ctx, puzzle_ptr);
    if native_hash(puzzle.curried_puzzle_hash().into()) != parent.coin.puzzle_hash {
        return Err(Error::other("asset parent puzzle hash mismatch"));
    }
    let module = native_hash(puzzle.mod_hash().into());
    if module != dg_xch_puzzles::cats::CAT_1_TREE_HASH
        && module != dg_xch_puzzles::cats::CAT_2_TREE_HASH
        && chia_sdk_driver::NftInfo::parse(ctx, puzzle)
            .map_err(Error::other)?
            .is_none()
        && DidInfo::parse(ctx, puzzle).map_err(Error::other)?.is_none()
    {
        return Ok(None);
    }
    let (additions, _) = parent
        .compute_additions_with_cost(ASSET_COST_LIMIT)
        .map_err(Error::other)?;
    if !additions.contains(&record.coin) {
        return Err(Error::other("asset is not an output of its parent"));
    }
    if let Some(cats) = Cat::parse_children(ctx, sdk_coin(parent.coin), puzzle, solution_ptr)
        .map_err(Error::other)?
    {
        return Ok(cats
            .into_iter()
            .find(|cat| {
                native_coin(cat.coin) == record.coin
                    && cat.info.hidden_puzzle_hash.is_none()
                    && owners.contains(&native_hash(cat.info.p2_puzzle_hash))
            })
            .map(ParsedAsset::Cat));
    }
    if let Some(nft) =
        Nft::parse_child(ctx, sdk_coin(parent.coin), puzzle, solution_ptr).map_err(Error::other)?
    {
        return Ok((native_coin(nft.coin) == record.coin
            && owners.contains(&native_hash(nft.info.p2_puzzle_hash)))
        .then_some(ParsedAsset::Nft(nft)));
    }
    let program = parent.puzzle_reveal.to_program().map_err(Error::other)?;
    if let Some(did) = Did::parse_child(
        ctx,
        sdk_coin(parent.coin),
        puzzle,
        solution_ptr,
        sdk_coin(record.coin),
    )
    .map_err(Error::other)?
    {
        return Ok(
            (native_hash(did.info.puzzle_hash().into()) == record.coin.puzzle_hash
                && owners.contains(&native_hash(did.info.p2_puzzle_hash)))
            .then_some(ParsedAsset::Did(did)),
        );
    }
    let (module, args) = program.uncurry().map_err(Error::other)?;
    if module.tree_hash() == dg_xch_puzzles::cats::CAT_1_TREE_HASH {
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
    let mut ctx = SpendContext::new();
    let Some(parsed) = parse_asset(&mut ctx, &record, &parent, owners)? else {
        return Ok(None);
    };
    let (kind, asset_id, owner, metadata, royalty, royalty_hash, did) = match parsed {
        ParsedAsset::Cat(cat) => (
            AssetKind::Cat2,
            native_hash(cat.info.asset_id),
            native_hash(cat.info.p2_puzzle_hash),
            None,
            None,
            None,
            None,
        ),
        ParsedAsset::Legacy(id, owner) => (AssetKind::Cat1, id, owner, None, None, None, None),
        ParsedAsset::Did(did) => (
            AssetKind::Did(DidType::Cni),
            native_hash(did.info.launcher_id),
            native_hash(did.info.p2_puzzle_hash),
            Some(hex::encode(
                node_to_bytes(&ctx, did.info.metadata.ptr()).map_err(Error::other)?,
            )),
            None,
            None,
            None,
        ),
        ParsedAsset::Nft(nft) => (
            AssetKind::Nft1,
            native_hash(nft.info.launcher_id),
            native_hash(nft.info.p2_puzzle_hash),
            Some(hex::encode(
                node_to_bytes(&ctx, nft.info.metadata.ptr()).map_err(Error::other)?,
            )),
            Some(nft.info.royalty_basis_points),
            Some(native_hash(nft.info.royalty_puzzle_hash)),
            nft.info.current_owner.map(native_hash),
        ),
    };
    let metadata_summary = if let Some(encoded) = &metadata {
        use clvm_traits::FromClvm;
        let bytes = hex::decode(encoded).map_err(Error::other)?;
        let ptr = node_from_bytes(&mut ctx, &bytes).map_err(Error::other)?;
        chia_puzzle_types::nft::NftMetadata::from_clvm(&*ctx, ptr)
            .ok()
            .map(|metadata| format!("{metadata:#?}"))
    } else {
        None
    };
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

pub(crate) async fn standard_layer(
    wallet: &MemoryWallet,
    ctx: &mut SpendContext,
    hash: Bytes32,
) -> Result<StandardLayer, Error> {
    let serialized = wallet
        .puzzle_for_puzzle_hash(&hash)
        .await?
        .serialized()
        .map_err(Error::other)?
        .to_bytes();
    let ptr = node_from_bytes(ctx, &serialized).map_err(Error::other)?;
    StandardLayer::parse_puzzle(ctx, Puzzle::parse(ctx, ptr))
        .map_err(Error::other)?
        .ok_or_else(|| Error::other("wallet puzzle is not a standard key puzzle"))
}

fn nft_metadata(ctx: &mut SpendContext, request: &NftLaunch) -> Result<HashedPtr, Error> {
    request.validate()?;
    let uri = ctx.alloc(&request.data_uri).map_err(Error::other)?;
    let uris = ctx
        .new_pair(uri, clvmr::NodePtr::NIL)
        .map_err(Error::other)?;
    let hash = ctx
        .new_atom(request.data_hash.as_ref())
        .map_err(Error::other)?;
    let number = ctx.alloc(&request.edition_number).map_err(Error::other)?;
    let total = ctx.alloc(&request.edition_total).map_err(Error::other)?;
    let mut metadata = clvmr::NodePtr::NIL;
    for (name, value) in [("st", total), ("sn", number), ("h", hash), ("u", uris)] {
        let key = ctx.new_atom(name.as_bytes()).map_err(Error::other)?;
        let entry = ctx.new_pair(key, value).map_err(Error::other)?;
        metadata = ctx.new_pair(entry, metadata).map_err(Error::other)?;
    }
    Ok(HashedPtr::from_ptr(ctx, metadata))
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
    let mut ctx = SpendContext::new();
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
    let destination = sdk_hash(change);
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
            let layer = standard_layer(wallet, &mut ctx, change).await?;
            let recovery_hash = SerializedProgram::from_bytes(&[0x80])
                .to_program()?
                .tree_hash();
            let (issue, _) = Launcher::new(sdk_hash(parent.name()), 1)
                .create_did(
                    &mut ctx,
                    Some(sdk_hash(recovery_hash)),
                    0,
                    HashedPtr::NIL,
                    &layer,
                )
                .map_err(Error::other)?;
            conditions = conditions.extend(issue);
        }
        AssetAction::LaunchCat2 { amount } => {
            let parent = funding
                .first()
                .ok_or_else(|| Error::other("missing CAT funding coin"))?;
            let memos = ctx.hint(destination).map_err(Error::other)?;
            let (issue, _) = Cat::single_issuance(
                &mut ctx,
                sdk_hash(parent.name()),
                None,
                *amount,
                Conditions::new().create_coin(destination, *amount, memos),
            )
            .map_err(Error::other)?;
            conditions = conditions.extend(issue);
        }
        AssetAction::LaunchNft(request) => {
            let parent = funding
                .first()
                .ok_or_else(|| Error::other("missing NFT funding coin"))?;
            let metadata = nft_metadata(&mut ctx, request)?;
            let mut mint = NftMint::new(metadata, destination, request.royalty_basis_points, None);
            mint.royalty_puzzle_hash = sdk_hash(request.royalty_puzzle_hash);
            let (issue, _) = Launcher::new(sdk_hash(parent.name()), 1)
                .mint_nft(&mut ctx, &mint)
                .map_err(Error::other)?;
            conditions = conditions.extend(issue);
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
            let parsed = parse_asset(&mut ctx, &asset.record, &asset.parent_spend, &owners)?
                .ok_or_else(|| Error::other("asset parent could not be verified"))?;
            let layer = standard_layer(wallet, &mut ctx, asset.owner_puzzle_hash).await?;
            let mut extra = Conditions::new();
            if let Some(parent) = funding.first() {
                extra = extra.assert_coin_announcement(chia_sdk_types::announcement_id(
                    sdk_hash(parent.name()),
                    "dgx-asset-fee",
                ));
                extra = extra.create_coin_announcement(b"dgx-asset".to_vec().into());
                asset_anchor = Some(*coin_id);
            }
            match parsed {
                ParsedAsset::Cat(cat) => {
                    if native_hash(cat.info.asset_id) != asset.asset_id {
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
                        let Some(ParsedAsset::Cat(candidate_cat)) = parse_asset(
                            &mut ctx,
                            &candidate.record,
                            &candidate.parent_spend,
                            &owners,
                        )?
                        else {
                            return Err(Error::other("CAT parent could not be verified"));
                        };
                        if candidate_cat.info.asset_id != cat.info.asset_id {
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
                    let memos = ctx.hint(sdk_hash(*destination)).map_err(Error::other)?;
                    extra = extra.create_coin(sdk_hash(*destination), *amount, memos);
                    if *amount < asset_total {
                        let memos = ctx
                            .hint(sdk_hash(asset.owner_puzzle_hash))
                            .map_err(Error::other)?;
                        extra = extra.create_coin(
                            sdk_hash(asset.owner_puzzle_hash),
                            asset_total - amount,
                            memos,
                        );
                    }
                    let mut spends = Vec::with_capacity(selected.len());
                    for (index, (selected_asset, selected_cat)) in selected.into_iter().enumerate()
                    {
                        let selected_layer =
                            standard_layer(wallet, &mut ctx, selected_asset.owner_puzzle_hash)
                                .await?;
                        let spend = selected_layer
                            .spend_with_conditions(
                                &mut ctx,
                                if index == 0 {
                                    extra.clone()
                                } else {
                                    Conditions::new()
                                },
                            )
                            .map_err(Error::other)?;
                        spends.push(CatSpend::new(selected_cat, spend));
                    }
                    Cat::spend_all(&mut ctx, &spends).map_err(Error::other)?;
                }
                ParsedAsset::Nft(nft) => {
                    if *amount != nft.coin.amount {
                        return Err(Error::other("NFT transfers must move the entire coin"));
                    }
                    let (did_conditions, _) = nft
                        .assign_owner(
                            &mut ctx,
                            &layer,
                            sdk_hash(*destination),
                            TransferNft::new(None, Vec::new(), None),
                            extra,
                        )
                        .map_err(Error::other)?;
                    if !did_conditions.is_empty() {
                        return Err(Error::other("unexpected DID approval requirements"));
                    }
                }
                ParsedAsset::Did(did) => {
                    if *amount != did.coin.amount {
                        return Err(Error::other("DID transfers must move the entire coin"));
                    }
                    let _ = did
                        .transfer(&mut ctx, &layer, sdk_hash(*destination), extra)
                        .map_err(Error::other)?;
                }
                ParsedAsset::Legacy(_, _) => return Err(Error::other("CAT1 is read-only")),
            }
        }
    }
    if total > needed {
        conditions = conditions.create_coin(destination, total - needed, Memos::None);
    }
    if let Some(asset) = asset_anchor {
        conditions = conditions
            .create_coin_announcement(b"dgx-asset-fee".to_vec().into())
            .assert_coin_announcement(chia_sdk_types::announcement_id(
                sdk_hash(asset),
                "dgx-asset",
            ));
    }
    for (index, coin) in funding.iter().enumerate() {
        let next = funding[(index + 1) % funding.len()];
        let bound = Conditions::new()
            .create_coin_announcement(b"dgx-funding".to_vec().into())
            .assert_coin_announcement(chia_sdk_types::announcement_id(
                sdk_hash(next.name()),
                "dgx-funding",
            ));
        let spend_conditions = if index == 0 {
            bound.extend(conditions.clone())
        } else {
            bound
        };
        standard_layer(wallet, &mut ctx, coin.puzzle_hash)
            .await?
            .spend(&mut ctx, sdk_coin(*coin), spend_conditions)
            .map_err(Error::other)?;
    }
    let spends = ctx
        .take()
        .into_iter()
        .map(|spend| CoinSpend {
            coin: native_coin(spend.coin),
            puzzle_reveal: SerializedProgram::from_bytes(spend.puzzle_reveal.as_ref()),
            solution: SerializedProgram::from_bytes(spend.solution.as_ref()),
        })
        .collect();
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
    use chia_sdk_test::Simulator;
    use dg_xch_clients::rpc::full_node::FullnodeClient;
    use dg_xch_core::consensus::constants::TESTNET_11;
    use std::sync::Arc;

    #[test]
    fn hashes_and_nested_puzzles_are_bounded() {
        assert!(parse_asset_hash("01").is_err());
        assert!(parse_asset_hash(&"ab".repeat(33)).is_err());
        assert_eq!(
            parse_asset_hash(&"ab".repeat(32)).unwrap(),
            [0xab; 32].into()
        );
        let mut ctx = SpendContext::new();
        let mut nested = clvmr::NodePtr::NIL;
        for _ in 0..300 {
            nested = ctx.new_pair(clvmr::NodePtr::NIL, nested).unwrap();
        }
        assert!(validate_tree(&ctx, nested).is_err());
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
        let mut ctx = SpendContext::new();
        let owner = Bytes32::from([25; 32]);
        let asset_id = Bytes32::from([26; 32]);
        let delegated = ctx
            .delegated_spend(Conditions::new().create_coin(sdk_hash(owner), 100, Memos::None))
            .unwrap();
        let inner = SerializedProgram::from_bytes(&node_to_bytes(&ctx, delegated.puzzle).unwrap())
            .to_program()
            .unwrap()
            .to_owned();
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
        let solution = ctx
            .serialize(&chia_puzzle_types::cat::CatSolution {
                inner_puzzle_solution: clvmr::NodePtr::NIL,
                lineage_proof: Some(chia_puzzle_types::LineageProof {
                    parent_parent_coin_info: sdk_hash(ancestor.parent_coin_info),
                    parent_inner_puzzle_hash: sdk_hash(inner.tree_hash()),
                    parent_amount: 100,
                }),
                prev_coin_id: sdk_hash(coin.name()),
                this_coin_info: sdk_coin(coin),
                next_coin_proof: chia_puzzle_types::CoinProof {
                    parent_coin_info: sdk_hash(coin.parent_coin_info),
                    inner_puzzle_hash: sdk_hash(inner.tree_hash()),
                    amount: 100,
                },
                prev_subtotal: 0,
                extra_delta: 0,
            })
            .unwrap();
        let parent = CoinSpend {
            coin,
            puzzle_reveal: legacy.serialized().unwrap(),
            solution: SerializedProgram::from_bytes(solution.as_ref()),
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
        let spends = bundle
            .coin_spends
            .iter()
            .map(|spend| {
                chia_protocol::CoinSpend::new(
                    sdk_coin(spend.coin),
                    spend.puzzle_reveal.to_bytes().into(),
                    spend.solution.to_bytes().into(),
                )
            })
            .collect();
        let signature =
            chia_bls::Signature::from_bytes(&bundle.aggregated_signature.bytes()).unwrap();
        sim.new_transaction(chia_protocol::SpendBundle::new(spends, signature))
            .unwrap();
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
    async fn cat2_launch_and_partial_transfer_pass_reference_consensus() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let coins: Vec<_> = [600, 700]
            .map(|amount| record(native_coin(simulator.new_coin(sdk_hash(owner), amount))))
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
        let fee_coin = record(native_coin(simulator.new_coin(sdk_hash(owner), 100)));
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
        let spends = stripped
            .coin_spends
            .iter()
            .map(|spend| {
                chia_protocol::CoinSpend::new(
                    sdk_coin(spend.coin),
                    spend.puzzle_reveal.to_bytes().into(),
                    spend.solution.to_bytes().into(),
                )
            })
            .collect();
        let error = stripped_simulator
            .new_transaction(chia_protocol::SpendBundle::new(
                spends,
                chia_bls::Signature::from_bytes(&stripped.aggregated_signature.bytes()).unwrap(),
            ))
            .unwrap_err();
        assert!(format!("{error:?}").contains("AssertCoinAnnouncementFailed"));
    }

    #[tokio::test]
    async fn cat2_transfer_combines_coins_and_excludes_reserved_inputs() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let funding = record(native_coin(simulator.new_coin(sdk_hash(owner), 1000)));
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
        let funding = record(native_coin(simulator.new_coin(sdk_hash(owner), 100)));
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
        let coin = record(native_coin(simulator.new_coin(sdk_hash(owner), 100)));
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
        assert!(nft_metadata(&mut SpendContext::new(), &invalid).is_err());
        invalid.royalty_basis_points = 0;
        invalid.data_uri = "file:///etc/passwd".into();
        assert!(nft_metadata(&mut SpendContext::new(), &invalid).is_err());
    }

    #[tokio::test]
    async fn spoofed_assets_reserved_coins_and_cat1_spends_are_rejected() {
        let wallet = wallet().await;
        let owner = wallet.get_puzzle_hash(false).await.unwrap();
        let mut simulator = Simulator::new();
        let coin = record(native_coin(simulator.new_coin(sdk_hash(owner), 2000)));
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
