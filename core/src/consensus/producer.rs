use crate::blockchain::block_record::BlockRecord;
use crate::blockchain::coin::Coin;
use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_block_data::FoliageBlockData;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::full_block::FullBlock;
use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::subslot_bundle::SubSlotBundle;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::unfinished_block::UnfinishedBlock;
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use crate::clvm::program::SerializedProgram;
use crate::consensus::block_filter::chia_block_filter;
use crate::consensus::block_generator::{
    canonical_additions_root, canonical_removals_root, transactions_generator_refs_root,
    transactions_generator_root, transactions_info_hash,
};
use crate::consensus::block_rewards::{calculate_base_farmer_reward, calculate_pool_reward};
use crate::consensus::coinbase::{create_farmer_coin, create_pool_coin};
use crate::consensus::constants::ConsensusConstants;
use crate::errors::ChiaError;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};

#[must_use]
pub fn g2_infinity() -> Bytes96 {
    let mut buf = [0u8; 96];
    buf[0] = 0xc0;
    Bytes96::new(buf)
}

/// `extension_data` is farmer-chosen entropy whose only consensus commitment is via the
/// plot signature over `FoliageBlockData::hash`; any 32-byte value is valid. It is derived
/// deterministically as `sha256(seed)`.
#[must_use]
fn extension_data_from_seed(seed: &[u8]) -> Bytes32 {
    Bytes32::new(hash_256(seed))
}

/// The four BLS plot signatures the FARMER supplies for a block it declared. In the live
/// node flow the producer never holds the plot secret key: the two signage-point
/// signatures arrive on the `DeclareProofOfSpace` message
/// (`challenge_chain_sp_signature`/`reward_chain_sp_signature`), and the two foliage
/// signatures arrive later on the `SignedValues` reply, after the node sends the farmer
/// the two foliage hashes to sign (`RequestSignedValues`).
///
/// `foliage_transaction_block_signature` is meaningful only for a transaction block
/// (`is_transaction_block == true`); it is ignored otherwise.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct FarmerSignatures {
    pub challenge_chain_sp_signature: Bytes96,
    pub reward_chain_sp_signature: Bytes96,
    pub foliage_block_data_signature: Bytes96,
    pub foliage_transaction_block_signature: Bytes96,
}

/// The candidate's total iters at its infusion point. When the signage point is in the
/// overflow region of its sub-slot (`sp_iters > ip_iters`, i.e. the SP is past where the
/// infusion lands so infusion spills into the NEXT sub-slot) one `sub_slot_iters` is
/// added; otherwise the infusion is in the same sub-slot.
///
/// This is the value [`create_unfinished_block`] takes as `infusion_point_total_iters` and
/// writes verbatim into `RewardChainBlockUnfinished.total_iters`. The caller (the declare
/// handler) must compute it here so the overflow case is handled at exactly one place.
/// All sums are `u128`.
#[must_use]
pub fn calculate_infusion_point_total_iters(
    sub_slot_start_total_iters: u128,
    sp_iters: u64,
    ip_iters: u64,
    sub_slot_iters: u64,
) -> u128 {
    let overflow = sp_iters > ip_iters;
    sub_slot_start_total_iters
        + u128::from(ip_iters)
        + if overflow {
            u128::from(sub_slot_iters)
        } else {
            0
        }
}

/// A single reward claim incorporated into a transaction block: the pool + farmer coins
/// minted for one prior block.
///
/// The claims come from walking the block records backwards from the previous block to the
/// most recent transaction block (and the non-transaction blocks between it and the one
/// before); the caller supplies the already-walked list. `fees` is the claimed block's own
/// fee total, added to the farmer reward for the *previous transaction block only* — pass
/// `fees == 0` for the skipped non-transaction blocks.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct RewardBlockClaim {
    pub height: u32,
    pub pool_puzzle_hash: Bytes32,
    pub farmer_puzzle_hash: Bytes32,
    pub fees: u64,
}

/// The transaction payload of a block being produced.
/// `additions`/`removals` are the SPEND coins only — reward coins are added separately from
/// [`RewardBlockClaim`]s. When this is `None` (no spend bundle), `create_foliage` still emits a
/// `TransactionsInfo` for a transaction block, with the generator-root / refs-root / signature / cost
/// defaults documented in [`create_foliage`].
#[derive(Clone, Debug)]
pub struct BlockTransactions {
    pub program: SerializedProgram,
    pub block_refs: Vec<u32>,
    pub additions: Vec<Coin>,
    pub removals: Vec<Coin>,
    pub aggregated_signature: Bytes96,
    pub cost: u64,
}

/// The foliage-assembly output:
/// `(Foliage, FoliageTransactionBlock | None, TransactionsInfo | None)`. The two trailing members are
/// `Some` iff `is_transaction_block`.
#[derive(Clone, Debug)]
pub struct FoliageResult {
    pub foliage: Foliage,
    pub foliage_transaction_block: Option<FoliageTransactionBlock>,
    pub transactions_info: Option<TransactionsInfo>,
}

/// Block fee = Σ(removal amounts) − Σ(addition amounts).
/// Sums are taken in `u128` (a block holds ≤ `MAX_SPENDS_PER_BLOCK` coins, each a `u64`, so the sum
/// cannot overflow `u128`). Errors when the
/// additions exceed the removals (a would-be minting block) or the fee exceeds `u64`.
///
/// # Errors
/// [`ChiaError::InvalidBlockFeeAmount`] when additions exceed removals or the fee overflows `u64`.
pub fn compute_block_fee(additions: &[Coin], removals: &[Coin]) -> Result<u64, ChiaError> {
    let removal_amount: u128 = removals.iter().map(|c| u128::from(c.amount)).sum();
    let addition_amount: u128 = additions.iter().map(|c| u128::from(c.amount)).sum();
    let fee = removal_amount
        .checked_sub(addition_amount)
        .ok_or(ChiaError::InvalidBlockFeeAmount)?;
    u64::try_from(fee).map_err(|_| ChiaError::InvalidBlockFeeAmount)
}

/// Assemble the foliage + reward coins for a block being produced. Given the
/// unfinished reward block's hash, the reward claims, and (optionally) a transaction payload, this
/// builds:
///   * the pool + farmer reward coins for every [`RewardBlockClaim`] (via
///     `coinbase::{create_pool_coin, create_farmer_coin}` + `block_rewards::calculate_*`),
///   * [`FoliageBlockData`],
///   * [`Foliage`] with real BLS plot signatures: `foliage_block_data_signature` over
///     `foliage_data.get_hash()` and — for a transaction block — `foliage_transaction_block_signature`
///     over `foliage_transaction_block.get_hash()`, both produced by `plot_signer`,
///   * for a transaction block, [`TransactionsInfo`] and [`FoliageTransactionBlock`].
///
/// This STOPS before assembling the `RewardChainBlockUnfinished` / `UnfinishedBlock`: it
/// therefore takes `reward_block_unfinished_hash`
/// directly rather than the whole object. Two more inputs are pre-resolved for the same reason,
/// being derived from the block records:
///   * `is_transaction_block` — the previous-transaction-block result (always `true` for
///     genesis);
///   * `prev_block_hash` / `prev_transaction_block_hash` — the genesis challenge at height 0,
///     else the respective ancestor's `header_hash`.
///
/// Field order and hashing are fixed so the resulting `foliage_transaction_block_hash` /
/// `transactions_info_hash` match: reward coins → `tx_additions` → `additions_root`/`removals_root`
/// (reusing the `block_generator::canonical_*` merkle helpers) → `TransactionsInfo` →
/// `FoliageTransactionBlock` → its hash feeding `Foliage`.
///
/// The BIP158 transaction filter is built internally via [`chia_block_filter`], so
/// `filter_hash = std_hash(chia_block_filter(...))` for both the empty (genesis /
/// no-tx-content) case and non-empty transaction filters.
///
/// `plot_public_key` is the proof-of-space's aggregate plot public key
/// (`reward_block_unfinished.proof_of_space.plot_public_key`), forwarded to `plot_signer` at each
/// signing point.
///
/// `plot_signer`: given a 32-byte message and the plot public key, it returns the BLS G2
/// element (AugScheme) plot signature. The producer never holds the plot secret key — the
/// farmer/harvester owns the key material (see the module note on taproot/pool aggregation).
///
/// # Errors
/// [`ChiaError::BadFarmerCoinAmount`] on farmer reward + fees overflow; [`ChiaError::BadAdditionRoot`]
/// if the additions merkle set fails to build; [`ChiaError::InvalidFoliageBlockHash`] if the
/// `FoliageBlockData` / `FoliageTransactionBlock` fail to serialize for hashing (the plot-signature
/// message); and the propagated errors of
/// [`compute_block_fee`]/`transactions_*_root`/[`transactions_info_hash`].
/// How the two foliage plot signatures are produced. `Signer` is the local-signing
/// callback model used by tests and the local-signing path; `Precomputed`
/// is the live-node farmer-supplied model — the node inserts signatures it received from the farmer
/// (placeholders at declare time, real ones spliced in at `signed_values`), never holding the plot
/// key. See [`FarmerSignatures`].
enum FoliageSigning<'a> {
    Signer {
        plot_public_key: Bytes48,
        sign: &'a dyn Fn(Bytes32, &Bytes48) -> Bytes96,
    },
    Precomputed {
        foliage_block_data_signature: Bytes96,
        foliage_transaction_block_signature: Bytes96,
    },
}

/// [`create_foliage`] with a local `plot_signer`. See the
/// module-level docs on the signer seam; forwards to [`create_foliage_inner`] with
/// [`FoliageSigning::Signer`].
///
/// # Errors
/// See [`create_foliage_inner`].
#[allow(clippy::too_many_arguments)]
pub fn create_foliage(
    constants: &ConsensusConstants,
    reward_block_unfinished_hash: Bytes32,
    height: u32,
    is_transaction_block: bool,
    reward_claims: &[RewardBlockClaim],
    transactions: Option<&BlockTransactions>,
    prev_block_hash: Bytes32,
    prev_transaction_block_hash: Bytes32,
    pool_target: PoolTarget,
    pool_signature: Option<Bytes96>,
    plot_public_key: Bytes48,
    farmer_reward_puzzle_hash: Bytes32,
    timestamp: u64,
    seed: &[u8],
    plot_signer: impl Fn(Bytes32, &Bytes48) -> Bytes96,
) -> Result<FoliageResult, ChiaError> {
    create_foliage_inner(
        constants,
        reward_block_unfinished_hash,
        height,
        is_transaction_block,
        reward_claims,
        transactions,
        prev_block_hash,
        prev_transaction_block_hash,
        pool_target,
        pool_signature,
        farmer_reward_puzzle_hash,
        timestamp,
        seed,
        &FoliageSigning::Signer {
            plot_public_key,
            sign: &plot_signer,
        },
    )
}

/// [`create_foliage`] with FARMER-supplied foliage signatures instead of a local signer — the live
/// node path (the candidate foliage is built with infinity placeholders, then
/// `signed_values` splices the real signatures in). Pass
/// [`g2_infinity`] placeholders at declare time; the real values are spliced later via
/// [`splice_farmer_foliage_signatures`] (or passed here directly when both are already known).
/// `foliage_transaction_block_signature` is used only when `is_transaction_block`.
///
/// The two foliage HASHES the farmer must sign are recoverable from the returned [`FoliageResult`]:
/// `result.foliage.foliage_block_data.hash()` and `result.foliage.foliage_transaction_block_hash`.
///
/// # Errors
/// See [`create_foliage_inner`].
#[allow(clippy::too_many_arguments)]
pub fn create_foliage_with_sigs(
    constants: &ConsensusConstants,
    reward_block_unfinished_hash: Bytes32,
    height: u32,
    is_transaction_block: bool,
    reward_claims: &[RewardBlockClaim],
    transactions: Option<&BlockTransactions>,
    prev_block_hash: Bytes32,
    prev_transaction_block_hash: Bytes32,
    pool_target: PoolTarget,
    pool_signature: Option<Bytes96>,
    farmer_reward_puzzle_hash: Bytes32,
    timestamp: u64,
    seed: &[u8],
    foliage_block_data_signature: Bytes96,
    foliage_transaction_block_signature: Bytes96,
) -> Result<FoliageResult, ChiaError> {
    create_foliage_inner(
        constants,
        reward_block_unfinished_hash,
        height,
        is_transaction_block,
        reward_claims,
        transactions,
        prev_block_hash,
        prev_transaction_block_hash,
        pool_target,
        pool_signature,
        farmer_reward_puzzle_hash,
        timestamp,
        seed,
        &FoliageSigning::Precomputed {
            foliage_block_data_signature,
            foliage_transaction_block_signature,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn create_foliage_inner(
    constants: &ConsensusConstants,
    reward_block_unfinished_hash: Bytes32,
    height: u32,
    is_transaction_block: bool,
    reward_claims: &[RewardBlockClaim],
    transactions: Option<&BlockTransactions>,
    prev_block_hash: Bytes32,
    prev_transaction_block_hash: Bytes32,
    pool_target: PoolTarget,
    pool_signature: Option<Bytes96>,
    farmer_reward_puzzle_hash: Bytes32,
    timestamp: u64,
    seed: &[u8],
    signing: &FoliageSigning,
) -> Result<FoliageResult, ChiaError> {
    // extension_data makes blocks differ by header hash; see the note on the fn
    let extension_data = extension_data_from_seed(seed);

    // FoliageBlockData(reward_block_unfinished.get_hash(), pool_target, pool_target_signature,
    // farmer_reward_puzzlehash, extension_data). The plot signature over foliage_data.get_hash()
    // is not a field of FoliageBlockData itself.
    let foliage_data = FoliageBlockData {
        unfinished_reward_block_hash: reward_block_unfinished_hash,
        pool_target,
        pool_signature,
        farmer_reward_puzzle_hash,
        extension_data,
    };

    // foliage_block_data_signature over foliage_data.get_hash() is always signed (tx block
    // or not). In the Precomputed (farmer-supplied) path this is the signature the farmer
    // returned — or its g2_infinity() placeholder while awaiting SignedValues.
    let foliage_block_data_hash = foliage_data
        .hash()
        .map_err(|_| ChiaError::InvalidFoliageBlockHash)?;
    let foliage_block_data_signature = match signing {
        FoliageSigning::Signer {
            plot_public_key,
            sign,
        } => sign(foliage_block_data_hash, plot_public_key),
        FoliageSigning::Precomputed {
            foliage_block_data_signature,
            ..
        } => *foliage_block_data_signature,
    };

    let mut foliage_transaction_block: Option<FoliageTransactionBlock> = None;
    let mut transactions_info: Option<TransactionsInfo> = None;
    let mut foliage_transaction_block_hash: Option<Bytes32> = None;
    let mut foliage_transaction_block_signature: Option<Bytes96> = None;

    if is_transaction_block {
        // reward_claims_incorporated += [pool_coin, farmer_coin] per walked block, only when
        // height > 0 (the genesis transaction block claims nothing). Preserve the [pool, farmer] order.
        let mut reward_claims_incorporated: Vec<Coin> = Vec::new();
        if height > 0 {
            for claim in reward_claims {
                let pool_coin = create_pool_coin(
                    claim.height,
                    claim.pool_puzzle_hash,
                    calculate_pool_reward(claim.height),
                    constants.genesis_challenge,
                );
                // calculate_base_farmer_reward(curr.height) + curr.fees, overflow-checked.
                let farmer_amount = calculate_base_farmer_reward(claim.height)
                    .checked_add(claim.fees)
                    .ok_or(ChiaError::BadFarmerCoinAmount)?;
                let farmer_coin = create_farmer_coin(
                    claim.height,
                    claim.farmer_puzzle_hash,
                    farmer_amount,
                    constants.genesis_challenge,
                );
                reward_claims_incorporated.push(pool_coin);
                reward_claims_incorporated.push(farmer_coin);
            }
        }

        // tx_additions (reward coins first, then spend additions) and byte_array_tx (each
        // addition's puzzle_hash, then each removal's coin name) are built in this exact order.
        let mut tx_additions: Vec<Coin> = Vec::with_capacity(reward_claims_incorporated.len());
        let mut tx_removal_names: Vec<Bytes32> = Vec::new();
        let mut byte_array_tx: Vec<Vec<u8>> = Vec::new();
        for coin in &reward_claims_incorporated {
            tx_additions.push(*coin);
            byte_array_tx.push(coin.puzzle_hash.bytes().to_vec());
        }

        // TransactionsInfo defaults when there is no generator:
        //   generator_hash = zeros; generator_refs_hash = [1; 32];
        //   signature = infinity; cost = 0; spend_bundle_fees = 0.
        let (generator_root, generator_refs_root, aggregated_signature, cost, spend_bundle_fees) =
            if let Some(tx) = transactions {
                for coin in &tx.additions {
                    tx_additions.push(*coin);
                    byte_array_tx.push(coin.puzzle_hash.bytes().to_vec());
                }
                for coin in &tx.removals {
                    let cname = coin.name();
                    tx_removal_names.push(cname);
                    byte_array_tx.push(cname.bytes().to_vec());
                }
                let generator_root = transactions_generator_root(&tx.program);
                // transactions_generator_refs_root returns [1; 32] for an empty list.
                let generator_refs_root = transactions_generator_refs_root(&tx.block_refs)?;
                // spend_bundle_fees = compute_block_fee(additions, removals)
                let fees = compute_block_fee(&tx.additions, &tx.removals)?;
                (
                    generator_root,
                    generator_refs_root,
                    tx.aggregated_signature,
                    tx.cost,
                    fees,
                )
            } else {
                (
                    Bytes32::default(),
                    transactions_generator_refs_root(&[])?,
                    g2_infinity(),
                    0u64,
                    0u64,
                )
            };

        // additions_root over merkle items (puzzle_hash, hash_coin_ids(coin_ids)); removals_root
        // over the removal coin names. Both reuse block_generator's canonical_* helpers.
        let additions_root =
            canonical_additions_root(&tx_additions).map_err(|_| ChiaError::BadAdditionRoot)?;
        let removals_root = canonical_removals_root(&tx_removal_names);

        // filter_hash = std_hash(chia_block_filter(byte_array_tx))
        let encoded = chia_block_filter(&byte_array_tx);
        let filter_hash = Bytes32::new(hash_256(encoded));

        let info = TransactionsInfo {
            generator_root,
            generator_refs_root,
            aggregated_signature,
            fees: spend_bundle_fees,
            cost,
            reward_claims_incorporated,
        };

        let ftb = FoliageTransactionBlock {
            prev_transaction_block_hash,
            timestamp,
            filter_hash,
            additions_root,
            removals_root,
            transactions_info_hash: transactions_info_hash(&info)?,
        };
        // foliage_transaction_block_hash is computed once and reused as the signing message,
        // so the (hash Some) == (signature Some) invariant is established atomically here.
        let ftb_hash = ftb.hash().map_err(|_| ChiaError::InvalidFoliageBlockHash)?;
        foliage_transaction_block_hash = Some(ftb_hash);
        foliage_transaction_block_signature = Some(match signing {
            FoliageSigning::Signer {
                plot_public_key,
                sign,
            } => sign(ftb_hash, plot_public_key),
            FoliageSigning::Precomputed {
                foliage_transaction_block_signature,
                ..
            } => *foliage_transaction_block_signature,
        });
        foliage_transaction_block = Some(ftb);
        transactions_info = Some(info);
    }

    // Foliage(prev_block_hash, reward_block_unfinished.get_hash(), foliage_data,
    // foliage_block_data_signature, foliage_transaction_block_hash, foliage_transaction_block_signature).
    // The invariant `(ftb_hash is None) == (ftb_signature is None)` holds because both are set
    // together inside the is_transaction_block branch above and both remain None otherwise.
    debug_assert_eq!(
        foliage_transaction_block_hash.is_some(),
        foliage_transaction_block_signature.is_some(),
        "(ftb_hash Some) == (ftb_signature Some) must hold"
    );
    let foliage = Foliage {
        prev_block_hash,
        reward_block_hash: reward_block_unfinished_hash,
        foliage_block_data: foliage_data,
        foliage_block_data_signature,
        foliage_transaction_block_hash,
        foliage_transaction_block_signature,
    };

    Ok(FoliageResult {
        foliage,
        foliage_transaction_block,
        transactions_info,
    })
}

/// Assemble a full [`UnfinishedBlock`] from the signage-point state plus the foliage payload.
/// This builds the
/// [`RewardChainBlockUnfinished`] from the proof-of-space and the challenge-/reward-chain signage-point
/// VDFs, calls [`create_foliage`] with that reward block's hash, and packs the result into an
/// [`UnfinishedBlock`] (the infusion-point VDFs are filled later by `unfinished_block_to_full_block`).
///
///   * `infusion_point_total_iters` becomes the reward block's `total_iters`;
///   * `pos_ss_cc_challenge_hash` is the slot's cc challenge;
///   * `challenge_chain_sp_vdf` / `reward_chain_sp_vdf` are the signage point's cc/rc VDFs
///     (`Option`: `None` at a sub-slot's first signage point);
///   * `challenge_chain_sp_proof` / `reward_chain_sp_proof` are carried through unchanged into
///     the `UnfinishedBlock`;
///   * `finished_sub_slots` is already copied by the caller.
///
/// The remaining parameters (`height` .. `plot_signer`) are forwarded verbatim to [`create_foliage`];
/// see its docs for the block-store-derived inputs pre-resolved at the call site.
///
/// `plot_signer` produces `challenge_chain_sp_signature` over `cc_sp_hash` and
/// `reward_chain_sp_signature` over `rc_sp_hash`, and the same signer is forwarded into
/// [`create_foliage`] for the two foliage signatures. `proof_of_space.plot_public_key` is the G1 key
/// handed to the signer at every point.
///
/// `cc_sp_hash` / `rc_sp_hash` are the signage-point message hashes, pre-resolved by the caller.
/// In testing mode the caller
/// sets `cc_sp_hash = signage_point.cc_vdf.output.get_hash()` and
/// `rc_sp_hash = signage_point.rc_vdf.output.get_hash()`; on the real path it derives `rc_sp_hash` from
/// the last finished sub-slot's reward chain (or the genesis challenge / the ancestor's reward-slot
/// hash) and `cc_sp_hash = slot_cc_challenge`. The VDFs enter the reward block exactly as supplied.
///
/// # Errors
/// [`ChiaError::InvalidRewardBlockHash`] if the assembled [`RewardChainBlockUnfinished`] fails to
/// serialize for hashing, plus the propagated errors of [`create_foliage`].
#[allow(clippy::too_many_arguments)]
pub fn create_unfinished_block(
    constants: &ConsensusConstants,
    infusion_point_total_iters: u128,
    signage_point_index: u8,
    proof_of_space: ProofOfSpace,
    pos_ss_cc_challenge_hash: Bytes32,
    challenge_chain_sp_vdf: Option<VdfInfo>,
    challenge_chain_sp_proof: Option<VdfProof>,
    reward_chain_sp_vdf: Option<VdfInfo>,
    reward_chain_sp_proof: Option<VdfProof>,
    cc_sp_hash: Bytes32,
    rc_sp_hash: Bytes32,
    finished_sub_slots: Vec<SubSlotBundle>,
    height: u32,
    is_transaction_block: bool,
    reward_claims: &[RewardBlockClaim],
    transactions: Option<&BlockTransactions>,
    prev_block_hash: Bytes32,
    prev_transaction_block_hash: Bytes32,
    pool_target: PoolTarget,
    pool_signature: Option<Bytes96>,
    farmer_reward_puzzle_hash: Bytes32,
    timestamp: u64,
    seed: &[u8],
    plot_signer: impl Fn(Bytes32, &Bytes48) -> Bytes96,
) -> Result<UnfinishedBlock, ChiaError> {
    // plot_public_key is captured (Copy) before proof_of_space is moved into the reward block below,
    // and is also forwarded to create_foliage for the two foliage plot signatures.
    let plot_public_key = proof_of_space.plot_public_key;
    let challenge_chain_sp_signature = plot_signer(cc_sp_hash, &plot_public_key);
    let reward_chain_sp_signature = plot_signer(rc_sp_hash, &plot_public_key);

    let reward_chain_block = RewardChainBlockUnfinished {
        total_iters: infusion_point_total_iters,
        signage_point_index,
        pos_ss_cc_challenge_hash,
        proof_of_space,
        challenge_chain_sp_vdf,
        challenge_chain_sp_signature,
        reward_chain_sp_vdf,
        reward_chain_sp_signature,
    };

    // create_foliage takes the reward block's hash directly, so compute it once here and forward it
    // (borrow before the reward block is moved into the UnfinishedBlock below).
    let reward_block_unfinished_hash = reward_chain_block
        .hash()
        .map_err(|_| ChiaError::InvalidRewardBlockHash)?;

    let foliage_result = create_foliage(
        constants,
        reward_block_unfinished_hash,
        height,
        is_transaction_block,
        reward_claims,
        transactions,
        prev_block_hash,
        prev_transaction_block_hash,
        pool_target,
        pool_signature,
        plot_public_key,
        farmer_reward_puzzle_hash,
        timestamp,
        seed,
        plot_signer,
    )?;

    // The UnfinishedBlock model types the ref list as a bare Vec<u32> (never None; empty when
    // absent) — see blockchain/full_block.rs for the same shape.
    let (transactions_generator, transactions_generator_ref_list) = match transactions {
        Some(tx) => (Some(tx.program.clone()), tx.block_refs.clone()),
        None => (None, Vec::new()),
    };

    Ok(UnfinishedBlock {
        finished_sub_slots,
        reward_chain_block,
        challenge_chain_sp_proof,
        reward_chain_sp_proof,
        foliage: foliage_result.foliage,
        foliage_transaction_block: foliage_result.foliage_transaction_block,
        transactions_info: foliage_result.transactions_info,
        transactions_generator,
        transactions_generator_ref_list,
    })
}

/// [`create_unfinished_block`] with FARMER-supplied signatures instead of a local `plot_signer` — the
/// live-node emit path: the SP signatures come FROM THE DECLARE MESSAGE and the two foliage
/// signatures are infinity placeholders at declare time.
///
/// The node never holds the plot key, so:
///   * `farmer_sigs.challenge_chain_sp_signature` / `.reward_chain_sp_signature` are taken verbatim
///     from the `DeclareProofOfSpace` message (they are already there — do NOT placeholder them);
///   * `farmer_sigs.foliage_block_data_signature` / `.foliage_transaction_block_signature` are the
///     [`g2_infinity`] PLACEHOLDER at declare time and the REAL farmer signatures (from `SignedValues`)
///     once known. When building the placeholder candidate, splice the real ones in later with
///     [`splice_farmer_foliage_signatures`] rather than rebuilding.
///
/// After building the placeholder candidate, the caller reads the two hashes the farmer must sign from
/// the returned block: `block.foliage.foliage_block_data.hash()` and
/// `block.foliage.foliage_transaction_block_hash` (the `RequestSignedValues` payload).
///
/// # Errors
/// [`ChiaError::InvalidRewardBlockHash`] if the reward block fails to hash, plus the propagated errors
/// of [`create_foliage_with_sigs`].
#[allow(clippy::too_many_arguments)]
pub fn create_unfinished_block_with_sigs(
    constants: &ConsensusConstants,
    infusion_point_total_iters: u128,
    signage_point_index: u8,
    proof_of_space: ProofOfSpace,
    pos_ss_cc_challenge_hash: Bytes32,
    challenge_chain_sp_vdf: Option<VdfInfo>,
    challenge_chain_sp_proof: Option<VdfProof>,
    reward_chain_sp_vdf: Option<VdfInfo>,
    reward_chain_sp_proof: Option<VdfProof>,
    finished_sub_slots: Vec<SubSlotBundle>,
    height: u32,
    is_transaction_block: bool,
    reward_claims: &[RewardBlockClaim],
    transactions: Option<&BlockTransactions>,
    prev_block_hash: Bytes32,
    prev_transaction_block_hash: Bytes32,
    pool_target: PoolTarget,
    pool_signature: Option<Bytes96>,
    farmer_reward_puzzle_hash: Bytes32,
    timestamp: u64,
    seed: &[u8],
    farmer_sigs: FarmerSignatures,
) -> Result<UnfinishedBlock, ChiaError> {
    // The SP signatures come from the DeclareProofOfSpace message, NOT a signer.
    // Unlike the signer path, cc_sp_hash / rc_sp_hash are not needed here — the SP signatures are
    // already resolved — so this sibling drops those two params.
    let reward_chain_block = RewardChainBlockUnfinished {
        total_iters: infusion_point_total_iters,
        signage_point_index,
        pos_ss_cc_challenge_hash,
        proof_of_space,
        challenge_chain_sp_vdf,
        challenge_chain_sp_signature: farmer_sigs.challenge_chain_sp_signature,
        reward_chain_sp_vdf,
        reward_chain_sp_signature: farmer_sigs.reward_chain_sp_signature,
    };

    let reward_block_unfinished_hash = reward_chain_block
        .hash()
        .map_err(|_| ChiaError::InvalidRewardBlockHash)?;

    let foliage_result = create_foliage_with_sigs(
        constants,
        reward_block_unfinished_hash,
        height,
        is_transaction_block,
        reward_claims,
        transactions,
        prev_block_hash,
        prev_transaction_block_hash,
        pool_target,
        pool_signature,
        farmer_reward_puzzle_hash,
        timestamp,
        seed,
        farmer_sigs.foliage_block_data_signature,
        farmer_sigs.foliage_transaction_block_signature,
    )?;

    let (transactions_generator, transactions_generator_ref_list) = match transactions {
        Some(tx) => (Some(tx.program.clone()), tx.block_refs.clone()),
        None => (None, Vec::new()),
    };

    Ok(UnfinishedBlock {
        finished_sub_slots,
        reward_chain_block,
        challenge_chain_sp_proof,
        reward_chain_sp_proof,
        foliage: foliage_result.foliage,
        foliage_transaction_block: foliage_result.foliage_transaction_block,
        transactions_info: foliage_result.transactions_info,
        transactions_generator,
        transactions_generator_ref_list,
    })
}

/// Splice the farmer's real foliage signatures into a candidate built with placeholders —
/// the node half of the `signed_values` flow. The `foliage_block_data_signature` is always
/// overwritten; the `foliage_transaction_block_signature` is overwritten ONLY for a
/// transaction block (`candidate.foliage.foliage_transaction_block_hash.is_some()`) — a
/// non-transaction block has no `foliage_transaction_block_signature` slot (it stays `None`).
///
/// The caller MUST first verify `foliage_block_data_signature` against the candidate's
/// `reward_chain_block.proof_of_space.plot_public_key` over
/// `candidate.foliage.foliage_block_data.hash()` before calling
/// this — this helper only performs the splice, it does not validate.
pub fn splice_farmer_foliage_signatures(
    candidate: &mut UnfinishedBlock,
    foliage_block_data_signature: Bytes96,
    foliage_transaction_block_signature: Bytes96,
) {
    candidate.foliage.foliage_block_data_signature = foliage_block_data_signature;
    // Only a transaction block carries a foliage_transaction_block_signature. The
    // (ftb_hash Some) == (ftb_signature Some) invariant from create_foliage means we key the splice
    // on the hash slot being present.
    if candidate.foliage.foliage_transaction_block_hash.is_some() {
        candidate.foliage.foliage_transaction_block_signature =
            Some(foliage_transaction_block_signature);
    }
}

/// Verify a farmer plot signature (`Bytes96`) over `msg` against a plot public key (`Bytes48`) under
/// AugScheme. Kept here so callers (the full node's `signed_values` handler) never touch `blst`
/// directly. Returns `false` on any decode failure (fail-closed — a malformed signature is a
/// verify miss).
#[must_use]
pub fn verify_plot_signature(plot_public_key: &Bytes48, msg: Bytes32, signature: &Bytes96) -> bool {
    use blst::min_pk::{PublicKey, Signature};
    let pk: PublicKey = plot_public_key.into();
    let Ok(sig) = Signature::try_from(signature) else {
        return false;
    };
    crate::clvm::bls_bindings::verify_signature(&pk, msg.as_ref(), &sig)
}

/// Finish an [`UnfinishedBlock`] into a [`FullBlock`] by infusing the timelord's infusion-point VDFs
/// and tweaking the height / weight / foliage links the foliage could not know at signage time.
///
/// The reward-chain block's signage-point-and-earlier fields are copied verbatim from the unfinished
/// reward block; the three infusion-point VDFs (`cc_ip`, `rc_ip`, optional `icc_ip`) and their proofs
/// come from the timelord's `NewInfusionPointVDF`. `is_transaction_block` is computed by the server
/// against the block store and passed in (core holds no store); it is forced `true` at genesis.
///
/// The foliage `reward_block_hash` is re-derived from the finished reward-chain block. On a non-genesis
/// block the foliage's `prev_block_hash` is set to the previous block's header hash, and a
/// non-transaction block additionally nulls the `foliage_transaction_block_hash` + its signature
/// and drops the transaction foliage/info/generator. The genesis block keeps
/// its foliage untouched save for `reward_block_hash`.
///
/// # Errors
/// [`ChiaError::InvalidRewardBlockHash`] if the finished reward-chain block fails to hash.
#[allow(clippy::too_many_arguments)]
pub fn unfinished_block_to_full_block(
    unfinished_block: &UnfinishedBlock,
    cc_ip_vdf: VdfInfo,
    cc_ip_proof: VdfProof,
    rc_ip_vdf: VdfInfo,
    rc_ip_proof: VdfProof,
    icc_ip_vdf: Option<VdfInfo>,
    icc_ip_proof: Option<VdfProof>,
    finished_sub_slots: Vec<SubSlotBundle>,
    prev_block: Option<&BlockRecord>,
    is_transaction_block: bool,
    difficulty: u64,
) -> Result<FullBlock, ChiaError> {
    let rcb_u = &unfinished_block.reward_chain_block;
    // prev None ⇒ genesis transaction block at height 0, weight = difficulty; else extend the
    // prev block with the caller-supplied is_transaction_block.
    let (is_transaction_block, new_weight, new_height) = match prev_block {
        None => (true, u128::from(difficulty), 0u32),
        Some(prev) => (
            is_transaction_block,
            prev.weight + u128::from(difficulty),
            prev.height + 1,
        ),
    };
    // only a genesis or a transaction block keeps the transaction foliage/info/generator
    let keep_tx = prev_block.is_none() || is_transaction_block;
    let (new_foliage_transaction_block, new_tx_info, new_generator, new_generator_ref_list) =
        if keep_tx {
            (
                unfinished_block.foliage_transaction_block,
                unfinished_block.transactions_info.clone(),
                unfinished_block.transactions_generator.clone(),
                unfinished_block.transactions_generator_ref_list.clone(),
            )
        } else {
            (None, None, None, Vec::new())
        };
    // RewardChainBlock: SP-and-earlier fields verbatim from the
    // unfinished reward block, the three infusion-point VDFs spliced in, new height/weight/is_tx.
    let reward_chain_block = RewardChainBlock {
        weight: new_weight,
        height: new_height,
        total_iters: rcb_u.total_iters,
        signage_point_index: rcb_u.signage_point_index,
        pos_ss_cc_challenge_hash: rcb_u.pos_ss_cc_challenge_hash,
        proof_of_space: rcb_u.proof_of_space.clone(),
        challenge_chain_sp_vdf: rcb_u.challenge_chain_sp_vdf,
        challenge_chain_sp_signature: rcb_u.challenge_chain_sp_signature,
        challenge_chain_ip_vdf: cc_ip_vdf,
        reward_chain_sp_vdf: rcb_u.reward_chain_sp_vdf,
        reward_chain_sp_signature: rcb_u.reward_chain_sp_signature,
        reward_chain_ip_vdf: rc_ip_vdf,
        infused_challenge_chain_ip_vdf: icc_ip_vdf,
        is_transaction_block,
    };
    // foliage.replace(reward_block_hash=..., [prev_block_hash,
    // foliage_transaction_block_hash/signature nulled for a non-tx block]).
    let reward_block_hash = reward_chain_block
        .hash()
        .map_err(|_| ChiaError::InvalidRewardBlockHash)?;
    let mut new_foliage = unfinished_block.foliage;
    new_foliage.reward_block_hash = reward_block_hash;
    if let Some(prev) = prev_block {
        new_foliage.prev_block_hash = prev.header_hash;
        if !is_transaction_block {
            new_foliage.foliage_transaction_block_hash = None;
            new_foliage.foliage_transaction_block_signature = None;
        }
    }
    Ok(FullBlock {
        finished_sub_slots,
        reward_chain_block,
        challenge_chain_sp_proof: unfinished_block.challenge_chain_sp_proof.clone(),
        challenge_chain_ip_proof: cc_ip_proof,
        reward_chain_sp_proof: unfinished_block.reward_chain_sp_proof.clone(),
        reward_chain_ip_proof: rc_ip_proof,
        infused_challenge_chain_ip_proof: icc_ip_proof,
        foliage: new_foliage,
        foliage_transaction_block: new_foliage_transaction_block,
        transactions_info: new_tx_info,
        transactions_generator: new_generator,
        transactions_generator_ref_list: new_generator_ref_list,
    })
}

#[must_use]
pub fn has_valid_pool_sig(constants: &ConsensusConstants, block: &FullBlock) -> bool {
    use blst::min_pk::{PublicKey, Signature};
    let fbd = &block.foliage.foliage_block_data;
    let Some(pool_pk) = block.reward_chain_block.proof_of_space.pool_public_key else {
        return true;
    };
    let is_pre_farm_target = fbd.pool_target.puzzle_hash
        == constants.genesis_pre_farm_pool_puzzle_hash
        && fbd.pool_target.max_height == 0;
    if !is_pre_farm_target || block.foliage.prev_block_hash == constants.genesis_challenge {
        return true;
    }
    let (Ok(pool_target_bytes), Some(pool_sig)) = (
        fbd.pool_target.to_bytes(ChiaProtocolVersion::default()),
        fbd.pool_signature,
    ) else {
        return false;
    };
    let pk: PublicKey = (&pool_pk).into();
    let Ok(sig) = Signature::try_from(&pool_sig) else {
        return false;
    };
    crate::clvm::bls_bindings::verify_signature(&pk, &pool_target_bytes, &sig)
}

#[cfg(test)]
#[path = "../../tests/unit/consensus/producer/producer_foliage_tests.rs"]
mod producer_foliage_tests;

#[cfg(test)]
#[path = "../../tests/unit/consensus/producer/producer_unfinished_block_tests.rs"]
mod producer_unfinished_block_tests;

// The farmer-supplied-signature emit path. These prove the producer half of the
// declare→RequestSignedValues→signed_values flow: the SP signatures come from the declare
// message (not a signer), the foliage signatures are placeholders at declare time and are
// spliced in from SignedValues, and the resulting block is byte-identical to the signer path
// fed the same four signatures.
#[cfg(test)]
#[path = "../../tests/unit/consensus/producer/producer_emit_path_tests.rs"]
mod producer_emit_path_tests;
