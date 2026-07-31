use crate::blockchain::coin::Coin;
use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::condition_with_args::{ConditionWithArgs, Message};
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::npc_result::NPCResult;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::spend::{NewCoin, Spend};
use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::blockchain::utils::{pkm_pairs_for_conditions, verify_agg_sig_unsafe_message};
use crate::clvm::bls_bindings::aggregate_verify_signature;
use crate::clvm::program::{Program, SerializedProgram};
use crate::clvm::sexp::{AtomBuf, SExp};
use crate::clvm::utils::{COST_CONDITIONS, ENABLE_KECCAK_OPS_OUTSIDE_FORK};
use crate::consensus::constants::ConsensusConstants;
use crate::consensus::generator_puzzles::ROM_BOOTSTRAP_GENERATOR_HEX;
use crate::consensus::{AGG_SIG_COST, CREATE_COIN_COST};
use crate::errors::{ChiaError, ClvmError};
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use blst::min_pk::Signature;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap, HashSet};

const ANNOUNCEMENT_LIMIT: u64 = 1024;

#[derive(Clone, Debug)]
pub struct GeneratorReference {
    pub height: u32,
    pub index: u32,
    pub generator: SerializedProgram,
}

#[derive(Clone, Debug, Default)]
pub struct BlockGeneratorFlags {
    pub clvm_flags: u32,
    pub simple_generator: bool,
}

impl BlockGeneratorFlags {
    #[must_use]
    pub fn for_height(
        constants: &ConsensusConstants,
        previous_transaction_block_height: u32,
    ) -> Self {
        if previous_transaction_block_height >= constants.hard_fork_height {
            Self {
                clvm_flags: COST_CONDITIONS | ENABLE_KECCAK_OPS_OUTSIDE_FORK,
                simple_generator: true,
            }
        } else {
            Self::default()
        }
    }
}

#[derive(Clone, Debug)]
pub struct BlockGeneratorInput {
    pub transactions_generator: SerializedProgram,
    pub generator_refs: Vec<GeneratorReference>,
    pub constants: ConsensusConstants,
    pub height: u32,
    pub flags: BlockGeneratorFlags,
}

#[derive(Copy, Clone, Debug)]
pub struct CoinSpendContext {
    pub birth_height: Option<u32>,
    pub birth_seconds: Option<u64>,
    pub spent_height: Option<u32>,
    pub spent_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct ConditionValidationContext {
    pub block_height: u32,
    pub previous_transaction_block_timestamp: Option<u64>,
    pub coin_context: HashMap<Bytes32, CoinSpendContext>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct BlockFeeSummary {
    pub removals_total: u128,
    pub additions_total: u128,
    pub reserve_fee: u64,
}

#[derive(Clone, Debug)]
pub struct TransactionBlockValidationInput<'a> {
    pub generator_input: BlockGeneratorInput,
    pub transactions_info: &'a TransactionsInfo,
    pub foliage_transaction_block: Option<&'a FoliageTransactionBlock>,
    pub condition_context: Option<&'a ConditionValidationContext>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TransactionBlockValidationResult {
    pub conditions: SpendBundleConditions,
    pub additions_root: Bytes32,
    pub removals_root: Bytes32,
    pub generator_root: Bytes32,
    pub generator_refs_root: Bytes32,
    pub fee_summary: BlockFeeSummary,
}

pub fn execute_block_generator(input: &BlockGeneratorInput) -> NPCResult {
    match execute_block_generator_result(input) {
        Ok(conds) => NPCResult {
            error: None,
            conds: Some(conds),
        },
        Err(error) => NPCResult {
            error: Some(chia_error_code(error)),
            conds: None,
        },
    }
}

pub fn validate_transaction_block(
    input: &TransactionBlockValidationInput,
) -> Result<TransactionBlockValidationResult, ChiaError> {
    let conds = execute_block_generator_result(&input.generator_input)?;
    let generator_root = transactions_generator_root(
        &input.generator_input.transactions_generator,
        &input.generator_input.constants,
        input.generator_input.height,
    )?;
    if generator_root != input.transactions_info.generator_root {
        return Err(ChiaError::InvalidTransactionsGeneratorHash);
    }

    let generator_ref_heights = input
        .generator_input
        .generator_refs
        .iter()
        .map(|reference| reference.height)
        .collect::<Vec<_>>();
    let generator_refs_root = transactions_generator_refs_root(&generator_ref_heights)?;
    if generator_refs_root != input.transactions_info.generator_refs_root {
        return Err(ChiaError::InvalidTransactionsGeneratorHash);
    }

    validate_block_aggregate_signature(
        &conds,
        &input.transactions_info.aggregated_signature,
        &input.generator_input.constants,
    )?;

    if conds.cost != input.transactions_info.cost {
        return Err(ChiaError::InvalidBlockCost);
    }

    let fee_summary = fee_summary(&conds)?;
    let computed_fee = block_fee_amount(&fee_summary)?;
    if computed_fee != input.transactions_info.fees {
        return Err(ChiaError::InvalidBlockFeeAmount);
    }
    if computed_fee < fee_summary.reserve_fee {
        return Err(ChiaError::ReserveFeeConditionFailed);
    }

    let additions_root =
        additions_root(&conds, &input.transactions_info.reward_claims_incorporated)
            .map_err(|_| ChiaError::BadAdditionRoot)?;
    let removals_root = removals_root(&conds);
    if let Some(foliage) = input.foliage_transaction_block {
        if foliage.transactions_info_hash != transactions_info_hash(input.transactions_info)? {
            return Err(ChiaError::InvalidTransactionsInfoHash);
        }
        if foliage.additions_root != additions_root {
            return Err(ChiaError::BadAdditionRoot);
        }
        if foliage.removals_root != removals_root {
            return Err(ChiaError::BadRemovalRoot);
        }
    }

    if let Some(ctx) = input.condition_context {
        validate_block_conditions(&conds, ctx)?;
    }

    Ok(TransactionBlockValidationResult {
        conditions: conds,
        additions_root,
        removals_root,
        generator_root,
        generator_refs_root,
        fee_summary,
    })
}

pub fn transactions_generator_root(
    generator: &SerializedProgram,
    constants: &ConsensusConstants,
    height: u32,
) -> Result<Bytes32, ChiaError> {
    if height >= constants.hard_fork_height {
        Ok(generator
            .to_program_backrefs()
            .map_err(|_| ChiaError::InvalidBlockSolution)?
            .tree_hash())
    } else {
        Ok(Bytes32::new(hash_256(generator.as_ref())))
    }
}

pub fn transactions_generator_refs_root(ref_heights: &[u32]) -> Result<Bytes32, ChiaError> {
    if ref_heights.is_empty() {
        return Ok(Bytes32::new([1; 32]));
    }
    let mut serialized = Vec::with_capacity(ref_heights.len() * 4);
    for height in ref_heights {
        serialized.extend(
            height
                .to_bytes(ChiaProtocolVersion::default())
                .map_err(|_| ChiaError::InvalidTransactionsGeneratorHash)?,
        );
    }
    Ok(Bytes32::new(hash_256(serialized)))
}

pub fn transactions_info_hash(transactions_info: &TransactionsInfo) -> Result<Bytes32, ChiaError> {
    let serialized = transactions_info
        .to_bytes(ChiaProtocolVersion::default())
        .map_err(|_| ChiaError::InvalidTransactionsInfoHash)?;
    Ok(Bytes32::new(hash_256(serialized)))
}

pub fn execute_block_generator_result(
    input: &BlockGeneratorInput,
) -> Result<SpendBundleConditions, ChiaError> {
    if input.transactions_generator.as_ref().len() > input.constants.max_generator_size as usize {
        return Err(ChiaError::InvalidTransactionsGeneratorHash);
    }
    if input.generator_refs.len() > input.constants.max_generator_ref_list_size as usize {
        return Err(ChiaError::TooManyGeneratorRefs);
    }
    for generator_ref in &input.generator_refs {
        if generator_ref.height >= input.height {
            return Err(ChiaError::FutureGeneratorRefs);
        }
        generator_ref
            .generator
            .to_program_backrefs()
            .map_err(|_| ChiaError::GeneratorRefHasNoGenerator)?;
    }
    if input.flags.simple_generator && !input.generator_refs.is_empty() {
        return Err(ChiaError::TooManyGeneratorRefs);
    }

    let generator = input
        .transactions_generator
        .to_program_backrefs()
        .map_err(|_| ChiaError::InvalidBlockSolution)?;
    if input.flags.simple_generator {
        check_simple_generator(&input.transactions_generator, &generator)?;
    }
    let max_cost = input.constants.max_block_cost_clvm;
    let byte_cost = (input.transactions_generator.as_ref().len() as u64)
        .checked_mul(input.constants.cost_per_byte)
        .ok_or(ChiaError::InvalidCostResult)?;
    let cost_left = max_cost
        .checked_sub(byte_cost)
        .ok_or(ChiaError::BlockCostExceedsMax)?;
    let (cost, output) = if input.flags.simple_generator {
        generator
            .run(cost_left, input.flags.clvm_flags, &Program::default())
            .map_err(generator_run_error)?
    } else {
        let rom_serial = SerializedProgram::from_hex(ROM_BOOTSTRAP_GENERATOR_HEX)
            .map_err(|_| ChiaError::GeneratorRuntimeError)?;
        let rom = rom_serial
            .to_program()
            .map_err(|_| ChiaError::GeneratorRuntimeError)?;
        let args = Program::to(SExp::from(vec![
            generator.sexp().to_owned(),
            SExp::from(vec![generator_ref_atoms(input)]),
        ]));
        rom.run(cost_left, input.flags.clvm_flags, &args)
            .map_err(generator_run_error)?
    };
    let cost = byte_cost
        .checked_add(cost)
        .ok_or(ChiaError::InvalidCostResult)?;
    if input.flags.simple_generator {
        conditions_from_generator_output(
            output.sexp(),
            cost,
            max_cost,
            input.flags.clvm_flags,
            &input.constants,
        )
    } else {
        conditions_from_processed_generator_output(output.sexp(), cost, max_cost, &input.constants)
    }
    .map_err(|_| ChiaError::InvalidBlockSolution)
}

fn generator_run_error(error: ClvmError) -> ChiaError {
    match error {
        ClvmError::CostExceeded(_, _) => ChiaError::BlockCostExceedsMax,
        _ => ChiaError::GeneratorRuntimeError,
    }
}

fn check_simple_generator(
    serialized: &SerializedProgram,
    generator: &Program,
) -> Result<(), ChiaError> {
    if !serialized.as_ref().starts_with(&[0xff, 0x01]) {
        return Err(ChiaError::ComplexGeneratorReceived);
    }
    let Ok((operator, _)) = generator.sexp().split() else {
        return Err(ChiaError::ComplexGeneratorReceived);
    };
    if operator.as_vec().as_deref() == Some(&[1]) {
        Ok(())
    } else {
        Err(ChiaError::ComplexGeneratorReceived)
    }
}

fn generator_ref_atoms(input: &BlockGeneratorInput) -> SExp<'static> {
    SExp::from(
        input
            .generator_refs
            .iter()
            .map(|reference| SExp::Atom(AtomBuf::new(reference.generator.to_bytes())))
            .collect::<Vec<_>>(),
    )
}

pub fn validate_block_aggregate_signature(
    conds: &SpendBundleConditions,
    aggregated_signature: &Bytes96,
    constants: &ConsensusConstants,
) -> Result<(), ChiaError> {
    let mut keys = Vec::<Bytes48>::new();
    let mut messages = Vec::<Message>::new();
    for spend in &conds.spends {
        let coin = Coin {
            parent_coin_info: spend.parent_id,
            puzzle_hash: spend.puzzle_hash,
            amount: spend.coin_amount,
        };
        for (_, pk, msg) in pkm_pairs_for_spend(spend, coin, constants)? {
            keys.push(pk);
            messages.push(msg);
        }
    }
    if keys.is_empty() {
        let mut infinity = [0_u8; 96];
        infinity[0] = 0xc0;
        return if aggregated_signature.as_ref() == infinity {
            Ok(())
        } else {
            Err(ChiaError::BadAggregateSignature)
        };
    }
    let message_refs = messages.iter().map(Message::data).collect::<Vec<&[u8]>>();
    let signature: Signature = aggregated_signature
        .try_into()
        .map_err(|_| ChiaError::BadAggregateSignature)?;
    if aggregate_verify_signature(&keys, &message_refs, &signature) {
        Ok(())
    } else {
        Err(ChiaError::BadAggregateSignature)
    }
}

pub fn additions_for_conditions(
    conds: &SpendBundleConditions,
    reward_claims: &[Coin],
) -> Vec<Coin> {
    let mut additions = reward_claims.to_vec();
    for spend in &conds.spends {
        for coin in &spend.create_coin {
            additions.push(Coin {
                parent_coin_info: spend.coin_id,
                puzzle_hash: coin.puzzle_hash,
                amount: coin.amount,
            });
        }
    }
    additions
}

pub fn removals_for_conditions(conds: &SpendBundleConditions) -> Vec<Bytes32> {
    conds.spends.iter().map(|spend| spend.coin_id).collect()
}

pub fn additions_root(
    conds: &SpendBundleConditions,
    reward_claims: &[Coin],
) -> Result<Bytes32, ClvmError> {
    canonical_additions_root(&additions_for_conditions(conds, reward_claims))
}

pub fn removals_root(conds: &SpendBundleConditions) -> Bytes32 {
    canonical_removals_root(&removals_for_conditions(conds))
}

pub fn canonical_removals_root(removals: &[Bytes32]) -> Bytes32 {
    merkle_set_root(removals.iter().map(|v| v.bytes()).collect())
}

pub fn canonical_additions_root(additions: &[Coin]) -> Result<Bytes32, ClvmError> {
    let mut by_puzzle_hash = BTreeMap::<[u8; 32], Vec<[u8; 32]>>::new();
    for coin in additions {
        by_puzzle_hash
            .entry(coin.puzzle_hash.bytes())
            .or_default()
            .push(coin.name().bytes());
    }
    let mut merkle_items = Vec::<[u8; 32]>::new();
    for (puzzle_hash, mut coin_ids) in by_puzzle_hash {
        coin_ids.sort();
        if coin_ids.len() > 1 {
            let mut buf = Vec::with_capacity(32 * coin_ids.len());
            for coin_id in &coin_ids {
                buf.extend_from_slice(coin_id.as_ref());
            }
            merkle_items.push(hash_256(buf));
        }
        merkle_items.push(puzzle_hash);
    }
    Ok(merkle_set_root(merkle_items))
}

pub fn fee_summary(conds: &SpendBundleConditions) -> Result<BlockFeeSummary, ChiaError> {
    Ok(BlockFeeSummary {
        removals_total: conds.removal_amount,
        additions_total: conds.addition_amount,
        reserve_fee: conds.reserve_fee,
    })
}

pub fn block_fee_amount(summary: &BlockFeeSummary) -> Result<u64, ChiaError> {
    let fee = summary
        .removals_total
        .checked_sub(summary.additions_total)
        .ok_or(ChiaError::InvalidBlockFeeAmount)?;
    u64::try_from(fee).map_err(|_| ChiaError::InvalidBlockFeeAmount)
}

pub fn validate_block_conditions(
    conds: &SpendBundleConditions,
    ctx: &ConditionValidationContext,
) -> Result<(), ChiaError> {
    if conds.height_absolute > ctx.block_height {
        return Err(ChiaError::AssertHeightAbsoluteFailed);
    }
    if let Some(before_height) = conds.before_height_absolute
        && ctx.block_height >= before_height
    {
        return Err(ChiaError::AssertHeightAbsoluteFailed);
    }
    if let Some(timestamp) = ctx.previous_transaction_block_timestamp {
        if conds.seconds_absolute > timestamp {
            return Err(ChiaError::AssertSecondsAbsoluteFailed);
        }
        if let Some(before_seconds) = conds.before_seconds_absolute
            && timestamp >= before_seconds
        {
            return Err(ChiaError::AssertSecondsAbsoluteFailed);
        }
    }

    let mut spent = HashSet::new();
    let mut created = HashSet::new();
    let mut coin_announcements = HashSet::<Bytes32>::new();
    let mut puzzle_announcements = HashSet::<Bytes32>::new();
    let mut asserted_coin_announcements = Vec::<Bytes32>::new();
    let mut asserted_puzzle_announcements = Vec::<Bytes32>::new();
    let mut total_announcements = 0_u64;

    for spend in &conds.spends {
        if !spent.insert(spend.coin_id) {
            return Err(ChiaError::DoubleSpend);
        }
        if let Some(context) = ctx.coin_context.get(&spend.coin_id) {
            validate_spend_context(spend, *context, ctx)?;
        }
        for coin in &spend.create_coin {
            let created_coin = Coin {
                parent_coin_info: spend.coin_id,
                puzzle_hash: coin.puzzle_hash,
                amount: coin.amount,
            };
            if !created.insert(created_coin.name()) {
                return Err(ChiaError::DuplicateOutput);
            }
        }
        total_announcements = total_announcements
            .checked_add(
                (spend.create_coin_announcements.len()
                    + spend.assert_coin_announcements.len()
                    + spend.create_puzzle_announcements.len()
                    + spend.assert_puzzle_announcements.len()) as u64,
            )
            .ok_or(ChiaError::InvalidCondition)?;
        if total_announcements > ANNOUNCEMENT_LIMIT {
            return Err(ChiaError::InvalidCondition);
        }
        collect_announcements(
            spend,
            &mut coin_announcements,
            &mut puzzle_announcements,
            &mut asserted_coin_announcements,
            &mut asserted_puzzle_announcements,
        );
    }
    for asserted in asserted_coin_announcements {
        if !coin_announcements.contains(&asserted) {
            return Err(ChiaError::AssertAnnounceConsumedFailed);
        }
    }
    for asserted in asserted_puzzle_announcements {
        if !puzzle_announcements.contains(&asserted) {
            return Err(ChiaError::AssertAnnounceConsumedFailed);
        }
    }
    Ok(())
}

fn conditions_from_generator_output(
    output: &SExp,
    cost: u64,
    max_cost: u64,
    clvm_flags: u32,
    constants: &ConsensusConstants,
) -> Result<SpendBundleConditions, ClvmError> {
    let mut conds = SpendBundleConditions {
        spends: Vec::new(),
        reserve_fee: 0,
        height_absolute: 0,
        seconds_absolute: 0,
        before_height_absolute: None,
        before_seconds_absolute: None,
        agg_sig_unsafe: Vec::new(),
        cost,
        removal_amount: 0,
        addition_amount: 0,
    };
    let mut spent = HashSet::<Bytes32>::new();
    let mut created = HashSet::<Bytes32>::new();
    let mut conditions_cost = 0_u64;
    let all_spends = output.first()?;
    for spend_sexp in all_spends.ref_list() {
        let spend_parts = spend_sexp.ref_list();
        if spend_parts.len() < 4 {
            return Err(ClvmError::InvalidSpendbundle(
                "generator spend must be (parent puzzle_reveal amount solution . extra)"
                    .to_string(),
            ));
        }
        let parent_id = Bytes32::parse_atom(spend_parts[0])?;
        let puzzle_reveal = Program::new_ref(spend_parts[1]).to_owned();
        let puzzle_hash = puzzle_reveal.tree_hash();
        let amount = spend_parts[2]
            .as_int()?
            .to_u64()
            .ok_or_else(|| ClvmError::AtomNotValidU64(spend_parts[2].to_string()))?;
        let solution = Program::new_ref(spend_parts[3]).to_owned();
        let coin = Coin {
            parent_coin_info: parent_id,
            puzzle_hash,
            amount,
        };
        if !spent.insert(coin.name()) {
            return Err(ClvmError::DoubleSpend(format!("{}", coin.name())));
        }
        let cost_left = max_cost
            .checked_sub(conds.cost)
            .ok_or(ClvmError::CostExceeded(max_cost, conds.cost))?;
        let (puzzle_cost, puzzle_output) = puzzle_reveal.run(cost_left, clvm_flags, &solution)?;
        conds.cost = conds
            .cost
            .checked_add(puzzle_cost)
            .ok_or_else(|| ClvmError::Overflow("puzzle execution cost overflow".to_string()))?;
        let conditions = puzzle_output.sexp().try_into()?;
        let spend = spend_from_conditions(
            coin,
            conditions,
            &mut conds,
            constants,
            &mut conditions_cost,
        )?;
        for created_coin in &spend.create_coin {
            let coin = Coin {
                parent_coin_info: spend.coin_id,
                puzzle_hash: created_coin.puzzle_hash,
                amount: created_coin.amount,
            };
            if !created.insert(coin.name()) {
                return Err(ClvmError::DuplicateCreate(format!("{}", coin.name())));
            }
        }
        conds.spends.push(spend);
    }
    conds.cost = conds
        .cost
        .checked_add(conditions_cost)
        .ok_or_else(|| ClvmError::Overflow("block generator cost overflow".to_string()))?;
    if conds.cost > max_cost {
        return Err(ClvmError::CostExceeded(max_cost, conds.cost));
    }
    Ok(conds)
}

fn conditions_from_processed_generator_output(
    output: &SExp,
    cost: u64,
    max_cost: u64,
    constants: &ConsensusConstants,
) -> Result<SpendBundleConditions, ClvmError> {
    let mut conds = SpendBundleConditions {
        spends: Vec::new(),
        reserve_fee: 0,
        height_absolute: 0,
        seconds_absolute: 0,
        before_height_absolute: None,
        before_seconds_absolute: None,
        agg_sig_unsafe: Vec::new(),
        cost,
        removal_amount: 0,
        addition_amount: 0,
    };
    let mut spent = HashSet::<Bytes32>::new();
    let mut created = HashSet::<Bytes32>::new();
    let mut conditions_cost = 0_u64;
    let all_spends = output.first()?;
    for spend_sexp in all_spends.ref_list() {
        let spend_parts = spend_sexp.ref_list();
        if spend_parts.len() < 4 {
            return Err(ClvmError::InvalidSpendbundle(
                "generator spend must be (parent puzzle_hash amount conditions . extra)"
                    .to_string(),
            ));
        }
        let parent_id = Bytes32::parse_atom(spend_parts[0])?;
        let puzzle_hash = Bytes32::parse_atom(spend_parts[1])?;
        let amount = spend_parts[2]
            .as_int()?
            .to_u64()
            .ok_or_else(|| ClvmError::AtomNotValidU64(spend_parts[2].to_string()))?;
        let coin = Coin {
            parent_coin_info: parent_id,
            puzzle_hash,
            amount,
        };
        if !spent.insert(coin.name()) {
            return Err(ClvmError::DoubleSpend(format!("{}", coin.name())));
        }
        let conditions = spend_parts[3].try_into()?;
        let spend = spend_from_conditions(
            coin,
            conditions,
            &mut conds,
            constants,
            &mut conditions_cost,
        )?;
        for created_coin in &spend.create_coin {
            let coin = Coin {
                parent_coin_info: spend.coin_id,
                puzzle_hash: created_coin.puzzle_hash,
                amount: created_coin.amount,
            };
            if !created.insert(coin.name()) {
                return Err(ClvmError::DuplicateCreate(format!("{}", coin.name())));
            }
        }
        conds.spends.push(spend);
    }
    conds.cost = conds
        .cost
        .checked_add(conditions_cost)
        .ok_or_else(|| ClvmError::Overflow("block generator cost overflow".to_string()))?;
    if conds.cost > max_cost {
        return Err(ClvmError::CostExceeded(max_cost, conds.cost));
    }
    Ok(conds)
}

fn spend_from_conditions(
    coin: Coin,
    conditions: Vec<ConditionWithArgs>,
    conds: &mut SpendBundleConditions,
    constants: &ConsensusConstants,
    extra_cost: &mut u64,
) -> Result<Spend, ClvmError> {
    let mut spend = Spend {
        parent_id: coin.parent_coin_info,
        coin_amount: coin.amount,
        puzzle_hash: coin.puzzle_hash,
        coin_id: coin.name(),
        height_relative: None,
        seconds_relative: None,
        before_height_relative: None,
        before_seconds_relative: None,
        birth_height: None,
        birth_seconds: None,
        create_coin: HashSet::new(),
        agg_sig_me: Vec::new(),
        agg_sig_parent: Vec::new(),
        agg_sig_puzzle: Vec::new(),
        agg_sig_amount: Vec::new(),
        agg_sig_puzzle_amount: Vec::new(),
        agg_sig_parent_amount: Vec::new(),
        agg_sig_parent_puzzle: Vec::new(),
        create_coin_announcements: Vec::new(),
        assert_coin_announcements: Vec::new(),
        create_puzzle_announcements: Vec::new(),
        assert_puzzle_announcements: Vec::new(),
        flags: 0,
    };
    conds.removal_amount = conds
        .removal_amount
        .checked_add(u128::from(coin.amount))
        .ok_or_else(|| ClvmError::Overflow("removal amount overflow".to_string()))?;
    for condition in conditions {
        match condition {
            ConditionWithArgs::CreateCoin(puzzle_hash, amount, memos) => {
                *extra_cost = extra_cost
                    .checked_add(CREATE_COIN_COST)
                    .ok_or_else(|| ClvmError::Overflow("create coin cost overflow".to_string()))?;
                conds.addition_amount = conds
                    .addition_amount
                    .checked_add(u128::from(amount))
                    .ok_or_else(|| ClvmError::Overflow("addition amount overflow".to_string()))?;
                spend.create_coin.insert(NewCoin {
                    puzzle_hash,
                    amount,
                    hint: memos.first().map(|memo| UnsizedBytes::new(memo.clone())),
                });
            }
            ConditionWithArgs::ReserveFee(fee) => {
                conds.reserve_fee = conds
                    .reserve_fee
                    .checked_add(fee)
                    .ok_or_else(|| ClvmError::Overflow("reserve fee overflow".to_string()))?;
            }
            ConditionWithArgs::AssertHeightAbsolute(height) => {
                conds.height_absolute = max(conds.height_absolute, height);
            }
            ConditionWithArgs::AssertSecondsAbsolute(seconds) => {
                conds.seconds_absolute = max(conds.seconds_absolute, seconds);
            }
            ConditionWithArgs::AssertBeforeHeightAbsolute(height) => {
                conds.before_height_absolute = Some(match conds.before_height_absolute {
                    Some(existing) => min(existing, height),
                    None => height,
                });
            }
            ConditionWithArgs::AssertBeforeSecondsAbsolute(seconds) => {
                conds.before_seconds_absolute = Some(match conds.before_seconds_absolute {
                    Some(existing) => min(existing, seconds),
                    None => seconds,
                });
            }
            ConditionWithArgs::AssertHeightRelative(height) => {
                spend.height_relative = Some(max(spend.height_relative.unwrap_or(0), height));
            }
            ConditionWithArgs::AssertSecondsRelative(seconds) => {
                spend.seconds_relative = Some(max(spend.seconds_relative.unwrap_or(0), seconds));
            }
            ConditionWithArgs::AssertBeforeHeightRelative(height) => {
                spend.before_height_relative = Some(match spend.before_height_relative {
                    Some(existing) => min(existing, height),
                    None => height,
                });
            }
            ConditionWithArgs::AssertBeforeSecondsRelative(seconds) => {
                spend.before_seconds_relative = Some(match spend.before_seconds_relative {
                    Some(existing) => min(existing, seconds),
                    None => seconds,
                });
            }
            ConditionWithArgs::AssertMyBirthHeight(height) => spend.birth_height = Some(height),
            ConditionWithArgs::AssertMyBirthSeconds(seconds) => spend.birth_seconds = Some(seconds),
            ConditionWithArgs::AggSigMe(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_me.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigParent(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_parent.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigPuzzle(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_puzzle.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigAmount(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_amount.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigPuzzleAmount(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_puzzle_amount.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigParentAmount(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_parent_amount.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigParentPuzzle(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                spend.agg_sig_parent_puzzle.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::AggSigUnsafe(pk, msg) => {
                *extra_cost = extra_cost.checked_add(AGG_SIG_COST).ok_or_else(|| {
                    ClvmError::Overflow("aggregate signature cost overflow".to_string())
                })?;
                verify_agg_sig_unsafe_message(&msg, constants)?;
                conds.agg_sig_unsafe.push((
                    UnsizedBytes::new(pk.bytes().to_vec()),
                    UnsizedBytes::from(msg.data()),
                ));
            }
            ConditionWithArgs::CreateCoinAnnouncement(msg) => {
                spend
                    .create_coin_announcements
                    .push(UnsizedBytes::from(msg.data()));
            }
            ConditionWithArgs::AssertCoinAnnouncement(announcement) => {
                spend.assert_coin_announcements.push(announcement);
            }
            ConditionWithArgs::CreatePuzzleAnnouncement(msg) => {
                spend
                    .create_puzzle_announcements
                    .push(UnsizedBytes::from(msg.data()));
            }
            ConditionWithArgs::AssertPuzzleAnnouncement(announcement) => {
                spend.assert_puzzle_announcements.push(announcement);
            }
            ConditionWithArgs::AssertMyCoinId(coin_id) if coin_id != coin.name() => {
                return Err(ClvmError::InvalidSpendbundle(
                    "ASSERT_MY_COIN_ID".to_string(),
                ));
            }
            ConditionWithArgs::AssertMyParentId(parent_id)
                if parent_id != coin.parent_coin_info =>
            {
                return Err(ClvmError::InvalidSpendbundle(
                    "ASSERT_MY_PARENT_ID".to_string(),
                ));
            }
            ConditionWithArgs::AssertMyPuzzlehash(puzzle_hash)
                if puzzle_hash != coin.puzzle_hash =>
            {
                return Err(ClvmError::InvalidSpendbundle(
                    "ASSERT_MY_PUZZLEHASH".to_string(),
                ));
            }
            ConditionWithArgs::AssertMyAmount(amount) if amount != coin.amount => {
                return Err(ClvmError::InvalidSpendbundle(
                    "ASSERT_MY_AMOUNT".to_string(),
                ));
            }
            ConditionWithArgs::SoftFork(cost) => {
                *extra_cost = extra_cost
                    .checked_add(cost)
                    .ok_or_else(|| ClvmError::Overflow("soft fork cost overflow".to_string()))?;
            }
            _ => {}
        }
    }
    Ok(spend)
}

fn pkm_pairs_for_spend(
    spend: &Spend,
    coin: Coin,
    constants: &ConsensusConstants,
) -> Result<Vec<(ConditionOpcode, Bytes48, Message)>, ChiaError> {
    let mut conditions = Vec::<ConditionWithArgs>::new();
    for (pk, msg) in &spend.agg_sig_me {
        conditions.push(ConditionWithArgs::AggSigMe(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    for (pk, msg) in &spend.agg_sig_parent {
        conditions.push(ConditionWithArgs::AggSigParent(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    for (pk, msg) in &spend.agg_sig_puzzle {
        conditions.push(ConditionWithArgs::AggSigPuzzle(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    for (pk, msg) in &spend.agg_sig_amount {
        conditions.push(ConditionWithArgs::AggSigAmount(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    for (pk, msg) in &spend.agg_sig_puzzle_amount {
        conditions.push(ConditionWithArgs::AggSigPuzzleAmount(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    for (pk, msg) in &spend.agg_sig_parent_amount {
        conditions.push(ConditionWithArgs::AggSigParentAmount(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    for (pk, msg) in &spend.agg_sig_parent_puzzle {
        conditions.push(ConditionWithArgs::AggSigParentPuzzle(
            Bytes48::parse(pk.as_slice()).map_err(|_| ChiaError::InvalidCondition)?,
            Message::new(msg.as_slice().to_vec()).map_err(|_| ChiaError::InvalidCondition)?,
        ));
    }
    pkm_pairs_for_conditions(
        &conditions,
        coin,
        constants.agg_sig_me_additional_data.as_ref(),
    )
    .map_err(|_| ChiaError::InvalidCondition)
}

fn validate_spend_context(
    spend: &Spend,
    context: CoinSpendContext,
    block_context: &ConditionValidationContext,
) -> Result<(), ChiaError> {
    if let Some(expected) = spend.birth_height
        && context.birth_height != Some(expected)
    {
        return Err(ChiaError::InvalidCondition);
    }
    if let Some(expected) = spend.birth_seconds
        && context.birth_seconds != Some(expected)
    {
        return Err(ChiaError::InvalidCondition);
    }
    if let Some(relative) = spend.height_relative
        && context
            .spent_height
            .and_then(|height| height.checked_add(relative))
            .is_some_and(|required| block_context.block_height < required)
    {
        return Err(ChiaError::AssertHeightRelativeFailed);
    }
    if let Some(before_relative) = spend.before_height_relative
        && context
            .spent_height
            .and_then(|height| height.checked_add(before_relative))
            .is_some_and(|before| block_context.block_height >= before)
    {
        return Err(ChiaError::AssertHeightRelativeFailed);
    }
    if let Some(timestamp) = block_context.previous_transaction_block_timestamp {
        if let Some(relative) = spend.seconds_relative
            && context
                .spent_seconds
                .and_then(|seconds| seconds.checked_add(relative))
                .is_some_and(|required| timestamp < required)
        {
            return Err(ChiaError::AssertSecondsRelativeFailed);
        }
        if let Some(before_relative) = spend.before_seconds_relative
            && context
                .spent_seconds
                .and_then(|seconds| seconds.checked_add(before_relative))
                .is_some_and(|before| timestamp >= before)
        {
            return Err(ChiaError::AssertSecondsRelativeFailed);
        }
    }
    Ok(())
}

fn collect_announcements(
    spend: &Spend,
    coin_announcements: &mut HashSet<Bytes32>,
    puzzle_announcements: &mut HashSet<Bytes32>,
    asserted_coin_announcements: &mut Vec<Bytes32>,
    asserted_puzzle_announcements: &mut Vec<Bytes32>,
) {
    for msg in &spend.create_coin_announcements {
        let mut buf = Vec::with_capacity(32 + msg.as_slice().len());
        buf.extend_from_slice(spend.coin_id.as_ref());
        buf.extend_from_slice(msg.as_slice());
        coin_announcements.insert(hash_256(buf).into());
    }
    for msg in &spend.create_puzzle_announcements {
        let mut buf = Vec::with_capacity(32 + msg.as_slice().len());
        buf.extend_from_slice(spend.puzzle_hash.as_ref());
        buf.extend_from_slice(msg.as_slice());
        puzzle_announcements.insert(hash_256(buf).into());
    }
    asserted_coin_announcements.extend(spend.assert_coin_announcements.iter().copied());
    asserted_puzzle_announcements.extend(spend.assert_puzzle_announcements.iter().copied());
}

fn merkle_set_root(mut items: Vec<[u8; 32]>) -> Bytes32 {
    if items.is_empty() {
        return hash_256([]).into();
    }
    items.sort();
    items.dedup();
    merkle_set_root_inner(&items, 0).into()
}

fn merkle_set_root_inner(items: &[[u8; 32]], bit: usize) -> [u8; 32] {
    if items.len() == 1 {
        let mut buf = Vec::with_capacity(33);
        buf.push(1);
        buf.extend_from_slice(&items[0]);
        return hash_256(buf);
    }
    let split = items
        .iter()
        .position(|item| bit_is_set(item, bit))
        .unwrap_or(items.len());
    if split == 0 || split == items.len() {
        return merkle_set_root_inner(items, bit + 1);
    }
    let left = merkle_set_root_inner(&items[..split], bit + 1);
    let right = merkle_set_root_inner(&items[split..], bit + 1);
    let mut buf = Vec::with_capacity(65);
    buf.push(2);
    buf.extend_from_slice(&left);
    buf.extend_from_slice(&right);
    hash_256(buf)
}

fn bit_is_set(item: &[u8; 32], bit: usize) -> bool {
    let byte = bit / 8;
    let offset = 7 - (bit % 8);
    byte < item.len() && (item[byte] & (1 << offset)) != 0
}

fn chia_error_code(error: ChiaError) -> u16 {
    match error {
        ChiaError::InvalidBlockSolution => 2,
        ChiaError::DuplicateOutput => 4,
        ChiaError::DoubleSpend => 5,
        ChiaError::BadAggregateSignature => 7,
        ChiaError::InvalidCondition => 10,
        ChiaError::AssertAnnounceConsumedFailed => 12,
        ChiaError::AssertHeightRelativeFailed => 13,
        ChiaError::AssertHeightAbsoluteFailed => 14,
        ChiaError::AssertSecondsAbsoluteFailed => 15,
        ChiaError::CoinAmountExceedsMaximum => 16,
        ChiaError::BlockCostExceedsMax => 23,
        ChiaError::BadAdditionRoot => 24,
        ChiaError::BadRemovalRoot => 25,
        ChiaError::ReserveFeeConditionFailed => 48,
        ChiaError::TooManyGeneratorRefs => 113,
        ChiaError::GeneratorRuntimeError => 117,
        ChiaError::InvalidCostResult => 118,
        ChiaError::FutureGeneratorRefs => 120,
        ChiaError::GeneratorRefHasNoGenerator => 121,
        ChiaError::CoinAmountNegative => 124,
        ChiaError::ComplexGeneratorReceived => 148,
        _ => 1,
    }
}

trait ParseAtomBytes32 {
    fn parse_atom(sexp: &SExp) -> Result<Self, ClvmError>
    where
        Self: Sized;
}

impl ParseAtomBytes32 for Bytes32 {
    fn parse_atom(sexp: &SExp) -> Result<Self, ClvmError> {
        Bytes32::parse(
            &sexp
                .as_vec()
                .ok_or_else(|| ClvmError::ExpectedAtomGotPair(sexp.to_string()))?,
        )
    }
}
