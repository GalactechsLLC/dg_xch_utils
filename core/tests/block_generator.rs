use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_opcode::ConditionOpcode;
use dg_xch_core::blockchain::condition_with_args::{ConditionWithArgs, Message, MessageArgs};
use dg_xch_core::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::blockchain::transactions_info::TransactionsInfo;
use dg_xch_core::clvm::parser::{sexp_from_bytes, sexp_from_bytes_backrefs};
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::{AtomBuf, SExp};
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, CoinSpendContext, ConditionValidationContext,
    GeneratorReference, TransactionBlockValidationInput, additions_root, block_fee_amount,
    conditions_from_spend_bundle, execute_block_generator, execute_block_generator_result,
    fee_summary, removals_root, transactions_generator_refs_root, transactions_generator_root,
    transactions_info_hash, validate_block_aggregate_signature, validate_block_conditions,
    validate_transaction_block,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::errors::ChiaError;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use std::collections::HashMap;
use std::io::Cursor;

const HISTORICAL_BLOCK_834752: &str =
    include_str!("fixtures/chia_generator_tests/block-834752.txt");
const HISTORICAL_BLOCK_834752_COMPRESSED: &str =
    include_str!("fixtures/chia_generator_tests/block-834752-compressed.txt");
const HISTORICAL_BLOCK_4671894: &str =
    include_str!("fixtures/chia_generator_tests/block-4671894.txt");
const HISTORICAL_BLOCK_4671894_REF: &str =
    include_str!("fixtures/chia_generator_tests/block-4671894.env");

#[test]
fn historical_block_6755796_completes_with_exact_cost() {
    let input = BlockGeneratorInput {
        transactions_generator: SerializedProgram::from_hex(
            include_str!("fixtures/chia_generator_tests/block-6755796.txt").trim(),
        )
        .unwrap(),
        generator_refs: Vec::new(),
        constants: MAINNET,
        height: 6_755_796,
        flags: BlockGeneratorFlags::for_height(&MAINNET, 6_755_796),
    };
    let (completion, completed) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = completion.send(execute_block_generator_result(&input));
    });
    let conditions = completed
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("historical generator regressed to a superlinear checkpoint walk")
        .unwrap();
    assert_eq!(conditions.cost, 1_755_381_849);
    assert_eq!(conditions.spends.len(), 5);
}

#[derive(Debug)]
struct ExpectedGeneratorOutput {
    generator: SerializedProgram,
    reserve_fee: u64,
    cost: u64,
    removal_amount: u128,
    addition_amount: u128,
    spends: Vec<ExpectedSpend>,
}

#[derive(Clone, Debug)]
struct ExpectedSpend {
    coin_id: Bytes32,
    puzzle_hash: Bytes32,
    height_relative: Option<u32>,
    create_coins: Vec<(Bytes32, u64)>,
    agg_sig_me: Vec<(Bytes48, Vec<u8>)>,
}

fn parse_bytes32(hex: &str) -> Bytes32 {
    Bytes32::parse(&hex::decode(hex).unwrap()).unwrap()
}

fn parse_chia_generator_fixture(fixture: &str) -> ExpectedGeneratorOutput {
    let mut lines = fixture.lines();
    let generator = SerializedProgram::from_hex(lines.next().unwrap()).unwrap();
    let mut expected = ExpectedGeneratorOutput {
        generator,
        reserve_fee: 0,
        cost: 0,
        removal_amount: 0,
        addition_amount: 0,
        spends: Vec::new(),
    };
    let mut current_spend = None::<ExpectedSpend>;
    for line in lines {
        if !line.starts_with(' ')
            && !line.starts_with("- coin id:")
            && let Some(spend) = current_spend.take()
        {
            expected.spends.push(spend);
        }
        if line.starts_with("RESERVE_FEE:") {
            expected.reserve_fee = line
                .strip_prefix("RESERVE_FEE:")
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        } else if line.starts_with("- coin id:") {
            if let Some(spend) = current_spend.take() {
                expected.spends.push(spend);
            }
            let fields = line.split_whitespace().collect::<Vec<_>>();
            current_spend = Some(ExpectedSpend {
                coin_id: parse_bytes32(fields[3]),
                puzzle_hash: parse_bytes32(fields[5]),
                height_relative: None,
                create_coins: Vec::new(),
                agg_sig_me: Vec::new(),
            });
        } else if line.starts_with("  ASSERT_HEIGHT_RELATIVE") {
            current_spend.as_mut().unwrap().height_relative =
                Some(line.split_whitespace().last().unwrap().parse().unwrap());
        } else if line.starts_with("  CREATE_COIN:") {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            current_spend
                .as_mut()
                .unwrap()
                .create_coins
                .push((parse_bytes32(fields[2]), fields[4].parse().unwrap()));
        } else if line.starts_with("  AGG_SIG_ME") {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            current_spend.as_mut().unwrap().agg_sig_me.push((
                Bytes48::parse(&hex::decode(fields[2]).unwrap()).unwrap(),
                hex::decode(fields[4]).unwrap(),
            ));
        } else if line.starts_with("cost:") {
            expected.cost = line.strip_prefix("cost:").unwrap().trim().parse().unwrap();
        } else if line.starts_with("removal_amount:") {
            expected.removal_amount = line
                .strip_prefix("removal_amount:")
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        } else if line.starts_with("addition_amount:") {
            expected.addition_amount = line
                .strip_prefix("addition_amount:")
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        }
    }
    if let Some(spend) = current_spend.take() {
        expected.spends.push(spend);
    }
    expected
}

fn historical_input(
    fixture: &ExpectedGeneratorOutput,
    height: u32,
    generator_refs: Vec<GeneratorReference>,
) -> BlockGeneratorInput {
    BlockGeneratorInput {
        transactions_generator: fixture.generator.clone(),
        generator_refs,
        constants: MAINNET,
        height,
        flags: BlockGeneratorFlags::default(),
    }
}

fn assert_matches_chia_fixture(conds: &SpendBundleConditions, expected: &ExpectedGeneratorOutput) {
    assert_eq!(conds.reserve_fee, expected.reserve_fee);
    assert!(
        conds.cost >= expected.cost,
        "legacy ROM cost {} must be at least Chia generator2 cost {}",
        conds.cost,
        expected.cost
    );
    assert_eq!(conds.removal_amount, expected.removal_amount);
    assert_eq!(conds.addition_amount, expected.addition_amount);
    assert_eq!(conds.spends.len(), expected.spends.len());
    let mut expected_spends = expected.spends.clone();
    expected_spends.sort_by_key(|spend| spend.coin_id.bytes());
    let mut actual_spends = conds
        .spends
        .iter()
        .map(|spend| {
            let mut create_coins = spend
                .create_coin
                .iter()
                .map(|coin| (coin.puzzle_hash, coin.amount))
                .collect::<Vec<_>>();
            create_coins.sort_by(|a, b| a.0.bytes().cmp(&b.0.bytes()).then(a.1.cmp(&b.1)));
            let mut agg_sig_me = spend
                .agg_sig_me
                .iter()
                .map(|(pk, msg)| {
                    (
                        Bytes48::parse(pk.as_slice()).unwrap(),
                        msg.as_slice().to_vec(),
                    )
                })
                .collect::<Vec<_>>();
            agg_sig_me.sort_by(|a, b| a.0.bytes().cmp(&b.0.bytes()).then(a.1.cmp(&b.1)));
            (
                spend.coin_id,
                spend.puzzle_hash,
                spend.height_relative,
                create_coins,
                agg_sig_me,
            )
        })
        .collect::<Vec<_>>();
    actual_spends.sort_by_key(|(coin_id, _, _, _, _)| coin_id.bytes());
    for (expected_spend, actual_spend) in expected_spends.iter().zip(actual_spends.iter()) {
        let (actual_coin_id, actual_puzzle_hash, height_relative, create_coins, agg_sig_me) =
            actual_spend;
        assert_eq!(actual_coin_id, &expected_spend.coin_id);
        assert_eq!(actual_puzzle_hash, &expected_spend.puzzle_hash);
        assert_eq!(height_relative, &expected_spend.height_relative);
        let mut expected_create_coins = expected_spend.create_coins.clone();
        expected_create_coins.sort_by(|a, b| a.0.bytes().cmp(&b.0.bytes()).then(a.1.cmp(&b.1)));
        assert_eq!(create_coins, &expected_create_coins);
        let mut expected_agg_sig_me = expected_spend.agg_sig_me.clone();
        expected_agg_sig_me.sort_by(|a, b| a.0.bytes().cmp(&b.0.bytes()).then(a.1.cmp(&b.1)));
        assert_eq!(agg_sig_me, &expected_agg_sig_me);
    }
}

fn quoted_generator(output: SExp<'static>) -> SerializedProgram {
    Program::to((1_u8, output)).serialized().unwrap()
}

fn puzzle_for_conditions(conditions: Vec<ConditionWithArgs>) -> Program<'static> {
    let condition_sexps = conditions
        .iter()
        .map(|condition| SExp::from(condition).to_owned())
        .collect::<Vec<_>>();
    Program::to((1_u8, SExp::from(condition_sexps)))
}

fn spend_output(parent: Bytes32, amount: u64, puzzle: &Program<'static>) -> SExp<'static> {
    SExp::from(vec![
        SExp::from(parent),
        puzzle.sexp().to_owned(),
        SExp::from(amount),
        SExp::default(),
    ])
}

fn input(generator: SerializedProgram) -> BlockGeneratorInput {
    BlockGeneratorInput {
        transactions_generator: generator,
        generator_refs: Vec::new(),
        constants: MAINNET,
        height: 10,
        flags: BlockGeneratorFlags {
            simple_generator: true,
            ..Default::default()
        },
    }
}

fn validation_case(
    generator: SerializedProgram,
    reward_claims: Vec<Coin>,
) -> (
    BlockGeneratorInput,
    TransactionsInfo,
    FoliageTransactionBlock,
    SpendBundleConditions,
) {
    let req = input(generator);
    let conds = execute_block_generator_result(&req).unwrap();
    let summary = fee_summary(&conds).unwrap();
    let tx_info = TransactionsInfo {
        generator_root: transactions_generator_root(&req.transactions_generator),
        generator_refs_root: transactions_generator_refs_root(&[]).unwrap(),
        aggregated_signature: SpendBundle::empty().aggregated_signature,
        fees: block_fee_amount(&summary).unwrap(),
        cost: conds.cost,
        reward_claims_incorporated: reward_claims,
    };
    let foliage = FoliageTransactionBlock {
        prev_transaction_block_hash: Bytes32::new([10; 32]),
        timestamp: 1,
        filter_hash: Bytes32::new([11; 32]),
        additions_root: additions_root(&conds, &tx_info.reward_claims_incorporated).unwrap(),
        removals_root: removals_root(&conds),
        transactions_info_hash: transactions_info_hash(&tx_info).unwrap(),
    };
    (req, tx_info, foliage, conds)
}

#[test]
fn executes_block_generator_and_summarizes_conditions() {
    let parent = Bytes32::new([1; 32]);
    let child_puzzle = Bytes32::new([3; 32]);
    let puzzle = puzzle_for_conditions(vec![
        ConditionWithArgs::CreateCoin(child_puzzle, 90, vec![b"hint".to_vec()]),
        ConditionWithArgs::ReserveFee(7),
        ConditionWithArgs::AssertHeightAbsolute(9),
    ]);
    let output = SExp::from(vec![SExp::from(vec![spend_output(parent, 100, &puzzle)])]);

    let conds = execute_block_generator_result(&input(quoted_generator(output))).unwrap();

    assert_eq!(conds.spends.len(), 1);
    assert_eq!(conds.reserve_fee, 7);
    assert_eq!(conds.height_absolute, 9);
    assert_eq!(conds.removal_amount, 100);
    assert_eq!(conds.addition_amount, 90);
    assert_eq!(fee_summary(&conds).unwrap().reserve_fee, 7);
}

#[test]
fn malformed_reference_is_reported_without_storage_fetch() {
    let output = SExp::from(vec![SExp::from(Vec::<SExp>::new())]);
    let mut req = input(quoted_generator(output));
    req.generator_refs.push(GeneratorReference {
        height: 5,
        index: 0,
        generator: SerializedProgram::from_bytes(&[0xff]),
    });

    let result = execute_block_generator(&req);

    assert_eq!(
        result.error,
        Some(ChiaError::GeneratorRefHasNoGenerator as u16)
    );
    assert!(result.conds.is_none());
}

#[test]
fn bad_aggregate_signature_fails() {
    let sk = SecretKey::key_gen_v3(&[42; 32], &[]).unwrap();
    let pk = Bytes48::from(&sk.sk_to_pk());
    let parent = Bytes32::new([1; 32]);
    let puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AggSigMe(
        pk,
        Message::new(b"message".to_vec()).unwrap(),
    )]);
    let output = SExp::from(vec![SExp::from(vec![spend_output(parent, 100, &puzzle)])]);
    let conds = execute_block_generator_result(&input(quoted_generator(output))).unwrap();
    let bad_signature = SpendBundle::empty().aggregated_signature;

    assert_eq!(
        validate_block_aggregate_signature(&conds, &bad_signature, &MAINNET),
        Err(ChiaError::BadAggregateSignature)
    );
}

#[test]
fn additions_and_removals_roots_change_with_generated_coins() {
    let parent = Bytes32::new([1; 32]);
    let child_puzzle = Bytes32::new([3; 32]);
    let puzzle = puzzle_for_conditions(vec![ConditionWithArgs::CreateCoin(
        child_puzzle,
        90,
        vec![],
    )]);
    let output = SExp::from(vec![SExp::from(vec![spend_output(parent, 100, &puzzle)])]);
    let conds = execute_block_generator_result(&input(quoted_generator(output))).unwrap();
    let reward = Coin {
        parent_coin_info: Bytes32::new([8; 32]),
        puzzle_hash: Bytes32::new([9; 32]),
        amount: 2,
    };

    assert_ne!(additions_root(&conds, &[]).unwrap(), Bytes32::default());
    assert_ne!(
        additions_root(&conds, &[]).unwrap(),
        additions_root(&conds, &[reward]).unwrap()
    );
    assert_eq!(removals_root(&conds), removals_root(&conds));
}

#[test]
fn generator_refs_root_hashes_concatenated_ref_heights() {
    assert_eq!(
        transactions_generator_refs_root(&[]).unwrap(),
        Bytes32::new([1; 32])
    );

    let mut raw_refs = Vec::new();
    raw_refs.extend_from_slice(&5_u32.to_be_bytes());
    raw_refs.extend_from_slice(&12_u32.to_be_bytes());
    assert_eq!(
        transactions_generator_refs_root(&[5, 12]).unwrap(),
        Bytes32::new(hash_256(raw_refs))
    );
}

#[test]
fn transaction_block_validation_checks_metadata_and_roots() {
    let parent = Bytes32::new([1; 32]);
    let child_puzzle = Bytes32::new([3; 32]);
    let puzzle = puzzle_for_conditions(vec![ConditionWithArgs::CreateCoin(
        child_puzzle,
        90,
        vec![],
    )]);
    let output = SExp::from(vec![SExp::from(vec![spend_output(parent, 100, &puzzle)])]);
    let reward = Coin {
        parent_coin_info: Bytes32::new([8; 32]),
        puzzle_hash: Bytes32::new([9; 32]),
        amount: 2,
    };
    let (req, tx_info, foliage, conds) = validation_case(quoted_generator(output), vec![reward]);

    let result = validate_transaction_block(&TransactionBlockValidationInput {
        prev_transaction_block_height: 0,
        generator_input: req,
        transactions_info: &tx_info,
        foliage_transaction_block: Some(&foliage),
        condition_context: None,
    })
    .unwrap();

    assert_eq!(result.conditions, conds);
    assert_eq!(result.additions_root, foliage.additions_root);
    assert_eq!(result.removals_root, foliage.removals_root);
    assert_eq!(result.fee_summary.removals_total, 100);
    assert_eq!(result.fee_summary.additions_total, 90);
}

#[test]
fn transaction_block_validation_rejects_bad_generator_root() {
    let output = SExp::from(vec![SExp::from(Vec::<SExp>::new())]);
    let (req, mut tx_info, foliage, _) = validation_case(quoted_generator(output), vec![]);
    tx_info.generator_root = Bytes32::new([99; 32]);

    assert_eq!(
        validate_transaction_block(&TransactionBlockValidationInput {
            prev_transaction_block_height: 0,
            generator_input: req,
            transactions_info: &tx_info,
            foliage_transaction_block: Some(&foliage),
            condition_context: None,
        }),
        Err(ChiaError::InvalidTransactionsGeneratorHash)
    );
}

#[test]
fn transaction_block_validation_rejects_bad_fee_cost_and_roots() {
    let parent = Bytes32::new([1; 32]);
    let puzzle = puzzle_for_conditions(vec![ConditionWithArgs::CreateCoin(
        Bytes32::new([3; 32]),
        90,
        vec![],
    )]);
    let output = SExp::from(vec![SExp::from(vec![spend_output(parent, 100, &puzzle)])]);
    let (req, mut tx_info, mut foliage, _) = validation_case(quoted_generator(output), vec![]);

    tx_info.fees += 1;
    assert_eq!(
        validate_transaction_block(&TransactionBlockValidationInput {
            prev_transaction_block_height: 0,
            generator_input: req.clone(),
            transactions_info: &tx_info,
            foliage_transaction_block: Some(&foliage),
            condition_context: None,
        }),
        Err(ChiaError::InvalidBlockFeeAmount)
    );

    tx_info.fees -= 1;
    tx_info.cost += 1;
    assert_eq!(
        validate_transaction_block(&TransactionBlockValidationInput {
            prev_transaction_block_height: 0,
            generator_input: req.clone(),
            transactions_info: &tx_info,
            foliage_transaction_block: Some(&foliage),
            condition_context: None,
        }),
        Err(ChiaError::InvalidBlockCost)
    );

    tx_info.cost -= 1;
    foliage.additions_root = Bytes32::new([77; 32]);
    assert_eq!(
        validate_transaction_block(&TransactionBlockValidationInput {
            prev_transaction_block_height: 0,
            generator_input: req,
            transactions_info: &tx_info,
            foliage_transaction_block: Some(&foliage),
            condition_context: None,
        }),
        Err(ChiaError::BadAdditionRoot)
    );
}

#[test]
fn future_generator_reference_is_invalid() {
    let output = SExp::from(vec![SExp::from(Vec::<SExp>::new())]);
    let mut req = input(quoted_generator(output.clone()));
    req.generator_refs.push(GeneratorReference {
        height: 10,
        index: 0,
        generator: quoted_generator(output),
    });

    assert_eq!(
        execute_block_generator_result(&req),
        Err(ChiaError::FutureGeneratorRefs)
    );
}

#[test]
fn cost_limit_failure_is_reported() {
    let mut req = input(quoted_generator(SExp::from(vec![SExp::from(
        Vec::<SExp>::new(),
    )])));
    req.constants.max_block_cost_clvm = 1;

    assert_eq!(
        execute_block_generator_result(&req),
        Err(ChiaError::BlockCostExceedsMax)
    );
}

#[test]
fn legacy_generator_mode_executes_bootstrap_rom() {
    let output = SExp::from(vec![SExp::from(Vec::<SExp>::new())]);
    let mut req = input(quoted_generator(output));
    req.flags.simple_generator = false;

    let conds = execute_block_generator_result(&req).unwrap();

    assert!(conds.spends.is_empty());
    assert!(conds.cost > 0);
}

#[test]
fn legacy_generator_mode_accepts_caller_supplied_reference_generators() {
    let output = SExp::from(vec![SExp::from(Vec::<SExp>::new())]);
    let mut req = input(quoted_generator(output.clone()));
    req.flags.simple_generator = false;
    req.generator_refs.push(GeneratorReference {
        height: 5,
        index: 0,
        generator: quoted_generator(output),
    });

    let conds = execute_block_generator_result(&req).unwrap();

    assert!(conds.spends.is_empty());
}

#[test]
fn simple_generator_mode_rejects_complex_generator_shape() {
    let complex = Program::to((2_u8, SExp::default())).serialized().unwrap();
    let req = input(complex);

    assert_eq!(
        execute_block_generator_result(&req),
        Err(ChiaError::ComplexGeneratorReceived)
    );
}

#[test]
fn height_flags_select_legacy_before_hardfork() {
    let before = BlockGeneratorFlags::for_height(&MAINNET, MAINNET.hard_fork_height - 1);
    let after = BlockGeneratorFlags::for_height(&MAINNET, MAINNET.hard_fork_height);

    assert!(!before.simple_generator);
    assert!(after.simple_generator);
    assert_eq!(after.clvm_flags, 0, "no CLVM flag activates at hard fork 1");
}

#[test]
fn height_flags_activate_canonical_ints_at_soft_fork9() {
    use dg_xch_core::clvm::utils::CANONICAL_INTS;

    let before = BlockGeneratorFlags::for_height(&MAINNET, MAINNET.soft_fork9_height - 1);
    let at = BlockGeneratorFlags::for_height(&MAINNET, MAINNET.soft_fork9_height);

    assert_eq!(before.clvm_flags & CANONICAL_INTS, 0);
    assert_eq!(at.clvm_flags & CANONICAL_INTS, CANONICAL_INTS);
}

#[test]
fn matching_coin_announcement_assertion_validates() {
    let announcing_parent = Bytes32::new([1; 32]);
    let asserting_parent = Bytes32::new([2; 32]);
    let message = b"ann";
    let announcing_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::CreateCoinAnnouncement(
        Message::new(message.to_vec()).unwrap(),
    )]);
    let coin = Coin {
        parent_coin_info: announcing_parent,
        puzzle_hash: announcing_puzzle.tree_hash(),
        amount: 100,
    };
    let mut announcement_buf = Vec::new();
    announcement_buf.extend_from_slice(coin.name().as_ref());
    announcement_buf.extend_from_slice(message);
    let announcement_id = Bytes32::new(hash_256(announcement_buf));
    let asserting_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertCoinAnnouncement(
        announcement_id,
    )]);
    let output = SExp::from(vec![SExp::from(vec![
        spend_output(announcing_parent, 100, &announcing_puzzle),
        spend_output(asserting_parent, 100, &asserting_puzzle),
    ])]);
    let conds = execute_block_generator_result(&input(quoted_generator(output))).unwrap();

    assert!(validate_block_conditions(&conds, &Default::default()).is_ok());
}

#[test]
fn clvm_backreference_deserialization_matches_expanded_form() {
    let expanded = hex::decode("ff86666f6f626172ff86666f6f62617280").unwrap();
    let compressed = hex::decode("ff86666f6f626172fe01").unwrap();
    let expanded_sexp = sexp_from_bytes(&mut Cursor::new(expanded.as_slice())).unwrap();
    let compressed_sexp =
        sexp_from_bytes_backrefs(&mut Cursor::new(compressed.as_slice())).unwrap();

    assert_eq!(expanded_sexp.tree_hash(), compressed_sexp.tree_hash());
    assert_eq!(expanded_sexp, compressed_sexp);
}

#[test]
fn historical_block_834752_matches_chia_generator_fixture() {
    let expected = parse_chia_generator_fixture(HISTORICAL_BLOCK_834752);
    let conds =
        execute_block_generator_result(&historical_input(&expected, 834_752, vec![])).unwrap();

    assert_matches_chia_fixture(&conds, &expected);
}

#[test]
fn historical_compressed_block_834752_matches_chia_generator_fixture() {
    let expected = parse_chia_generator_fixture(HISTORICAL_BLOCK_834752_COMPRESSED);
    let conds =
        execute_block_generator_result(&historical_input(&expected, 834_752, vec![])).unwrap();

    assert_matches_chia_fixture(&conds, &expected);
}

#[test]
fn historical_generator_ref_block_4671894_matches_chia_generator_fixture() {
    let expected = parse_chia_generator_fixture(HISTORICAL_BLOCK_4671894);
    let refs = vec![GeneratorReference {
        height: 4_671_893,
        index: 0,
        generator: SerializedProgram::from_hex(HISTORICAL_BLOCK_4671894_REF).unwrap(),
    }];
    let conds =
        execute_block_generator_result(&historical_input(&expected, 4_671_894, refs)).unwrap();

    assert_matches_chia_fixture(&conds, &expected);
}

fn coin_of(parent: Bytes32, amount: u64, puzzle: &Program<'static>) -> Coin {
    Coin {
        parent_coin_info: parent,
        puzzle_hash: puzzle.tree_hash(),
        amount,
    }
}

fn conds_for(spends: Vec<SExp<'static>>) -> SpendBundleConditions {
    let output = SExp::from(vec![SExp::from(spends)]);
    execute_block_generator_result(&input(quoted_generator(output))).unwrap()
}

#[test]
fn assert_concurrent_spend_satisfied_validates() {
    let other_parent = Bytes32::new([2; 32]);
    let other_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::ReserveFee(0)]);
    let other_coin = coin_of(other_parent, 100, &other_puzzle);
    let asserting_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertConcurrentSpend(
        other_coin.name(),
    )]);
    let conds = conds_for(vec![
        spend_output(Bytes32::new([1; 32]), 100, &asserting_puzzle),
        spend_output(other_parent, 100, &other_puzzle),
    ]);
    assert!(validate_block_conditions(&conds, &Default::default()).is_ok());
}

#[test]
fn assert_concurrent_spend_missing_coin_fails_132() {
    let asserting_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertConcurrentSpend(
        Bytes32::new([0xAB; 32]),
    )]);
    let conds = conds_for(vec![spend_output(
        Bytes32::new([1; 32]),
        100,
        &asserting_puzzle,
    )]);
    let err = validate_block_conditions(&conds, &Default::default()).unwrap_err();
    assert_eq!(err, ChiaError::AssertConcurrentSpendFailed);
    assert_eq!(err as i64, 132);
}

#[test]
fn assert_concurrent_puzzle_satisfied_validates() {
    let other_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::ReserveFee(0)]);
    let asserting_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertConcurrentPuzzle(
        other_puzzle.tree_hash(),
    )]);
    let conds = conds_for(vec![
        spend_output(Bytes32::new([1; 32]), 100, &asserting_puzzle),
        spend_output(Bytes32::new([2; 32]), 100, &other_puzzle),
    ]);
    assert!(validate_block_conditions(&conds, &Default::default()).is_ok());
}

#[test]
fn assert_concurrent_puzzle_missing_puzzle_fails_133() {
    let asserting_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertConcurrentPuzzle(
        Bytes32::new([0xCD; 32]),
    )]);
    let conds = conds_for(vec![spend_output(
        Bytes32::new([1; 32]),
        100,
        &asserting_puzzle,
    )]);
    let err = validate_block_conditions(&conds, &Default::default()).unwrap_err();
    assert_eq!(err, ChiaError::AssertConcurrentPuzzleFailed);
    assert_eq!(err as i64, 133);
}

#[test]
fn assert_ephemeral_created_in_block_validates() {
    let child_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertEphemeral]);
    let child_amount = 42;
    let parent_parent = Bytes32::new([3; 32]);
    let parent_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::CreateCoin(
        child_puzzle.tree_hash(),
        child_amount,
        vec![],
    )]);
    let parent_coin = coin_of(parent_parent, 42, &parent_puzzle);
    let conds = conds_for(vec![
        spend_output(parent_parent, 42, &parent_puzzle),
        spend_output(parent_coin.name(), child_amount, &child_puzzle),
    ]);
    assert!(validate_block_conditions(&conds, &Default::default()).is_ok());
}

#[test]
fn assert_ephemeral_without_parent_in_block_fails_140() {
    let child_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::AssertEphemeral]);
    // The parent coin is not spent in this block, so the coin is not ephemeral.
    let conds = conds_for(vec![spend_output(Bytes32::new([9; 32]), 42, &child_puzzle)]);
    let err = validate_block_conditions(&conds, &Default::default()).unwrap_err();
    assert_eq!(err, ChiaError::AssertEphemeralFailed);
    assert_eq!(err as i64, 140);
}

// mode 0b111_000 = 56: sender commits its own coin id, receiver commits nothing.
const SENDER_COMMITS_COIN_ID: u8 = 0b111_000;

#[test]
fn paired_send_and_receive_message_validates() {
    let sender_parent = Bytes32::new([4; 32]);
    let message = Message::new(b"hello".to_vec()).unwrap();
    let sender_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::SendMessage(
        SENDER_COMMITS_COIN_ID,
        message,
        MessageArgs::None,
    )]);
    let sender_coin = coin_of(sender_parent, 100, &sender_puzzle);
    // Receiver names the sender's coin id as the message source; both commit
    // nothing on the receiver side, so the pair nets to zero.
    let receiver_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::ReceiveMessage(
        SENDER_COMMITS_COIN_ID,
        message,
        MessageArgs::CoinId(sender_coin.name()),
    )]);
    let conds = conds_for(vec![
        spend_output(sender_parent, 100, &sender_puzzle),
        spend_output(Bytes32::new([5; 32]), 100, &receiver_puzzle),
    ]);
    assert!(validate_block_conditions(&conds, &Default::default()).is_ok());
}

#[test]
fn unpaired_send_message_fails_147() {
    let sender_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::SendMessage(
        SENDER_COMMITS_COIN_ID,
        Message::new(b"orphan".to_vec()).unwrap(),
        MessageArgs::None,
    )]);
    let conds = conds_for(vec![spend_output(
        Bytes32::new([4; 32]),
        100,
        &sender_puzzle,
    )]);
    let err = validate_block_conditions(&conds, &Default::default()).unwrap_err();
    assert_eq!(err, ChiaError::MessageNotSentOrReceived);
    assert_eq!(err as i64, 147);
}

#[test]
fn send_and_receive_with_mismatched_message_fails_147() {
    let sender_parent = Bytes32::new([4; 32]);
    let sender_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::SendMessage(
        SENDER_COMMITS_COIN_ID,
        Message::new(b"ping".to_vec()).unwrap(),
        MessageArgs::None,
    )]);
    let sender_coin = coin_of(sender_parent, 100, &sender_puzzle);
    let receiver_puzzle = puzzle_for_conditions(vec![ConditionWithArgs::ReceiveMessage(
        SENDER_COMMITS_COIN_ID,
        Message::new(b"pong".to_vec()).unwrap(),
        MessageArgs::CoinId(sender_coin.name()),
    )]);
    let conds = conds_for(vec![
        spend_output(sender_parent, 100, &sender_puzzle),
        spend_output(Bytes32::new([5; 32]), 100, &receiver_puzzle),
    ]);
    let err = validate_block_conditions(&conds, &Default::default()).unwrap_err();
    assert_eq!(err, ChiaError::MessageNotSentOrReceived);
    assert_eq!(err as i64, 147);
}

// EASY_PUZZLE: CLVM path `1` returns the solution verbatim, so conditions live in
// the solution and the coin id does NOT depend on them (needed for MY_COIN_ID).
fn easy_puzzle() -> Program<'static> {
    Program::to(1_u8)
}

fn easy_spend(parent: Bytes32, amount: u64, solution: SExp<'static>) -> SExp<'static> {
    SExp::from(vec![
        SExp::from(parent),
        easy_puzzle().sexp().to_owned(),
        SExp::from(amount),
        solution,
    ])
}

fn conditions_solution(conditions: Vec<ConditionWithArgs>) -> SExp<'static> {
    SExp::from(
        conditions
            .iter()
            .map(|condition| SExp::from(condition).to_owned())
            .collect::<Vec<_>>(),
    )
}

// Execute a single EASY_PUZZLE spend whose solution is the given condition list.
fn conds_for_solution(
    parent: Bytes32,
    amount: u64,
    conditions: Vec<ConditionWithArgs>,
) -> SpendBundleConditions {
    let output = SExp::from(vec![SExp::from(vec![easy_spend(
        parent,
        amount,
        conditions_solution(conditions),
    )])]);
    execute_block_generator_result(&input(quoted_generator(output))).unwrap()
}

// Execute an EASY_PUZZLE spend whose solution carries a raw (untyped) condition,
// used for negative/edge arguments that the typed `ConditionWithArgs` can't hold.
fn conds_for_raw(
    parent: Bytes32,
    amount: u64,
    raw_conditions: Vec<SExp<'static>>,
) -> Result<SpendBundleConditions, ChiaError> {
    let solution = SExp::from(raw_conditions);
    let output = SExp::from(vec![SExp::from(vec![easy_spend(parent, amount, solution)])]);
    execute_block_generator_result(&input(quoted_generator(output)))
}

fn raw_condition(opcode: u8, arg: Vec<u8>) -> SExp<'static> {
    SExp::from(vec![SExp::from(opcode), SExp::Atom(AtomBuf::new(arg))])
}

fn height_ctx(block_height: u32) -> ConditionValidationContext {
    ConditionValidationContext {
        block_height,
        previous_transaction_block_timestamp: None,
        coin_context: HashMap::new(),
    }
}

fn seconds_ctx(timestamp: u64) -> ConditionValidationContext {
    ConditionValidationContext {
        block_height: 0,
        previous_transaction_block_timestamp: Some(timestamp),
        coin_context: HashMap::new(),
    }
}

#[test]
fn assert_height_absolute_boundary_matches_chia() {
    let conds = conds_for_solution(
        Bytes32::new([1; 32]),
        100,
        vec![ConditionWithArgs::AssertHeightAbsolute(100)],
    );
    assert!(validate_block_conditions(&conds, &height_ctx(100)).is_ok());
    assert_eq!(
        validate_block_conditions(&conds, &height_ctx(99)),
        Err(ChiaError::AssertHeightAbsoluteFailed)
    );
}

#[test]
fn assert_before_height_absolute_boundary_matches_chia() {
    let conds = conds_for_solution(
        Bytes32::new([1; 32]),
        100,
        vec![ConditionWithArgs::AssertBeforeHeightAbsolute(100)],
    );
    assert!(validate_block_conditions(&conds, &height_ctx(99)).is_ok());
    assert_eq!(
        validate_block_conditions(&conds, &height_ctx(100)),
        Err(ChiaError::AssertHeightAbsoluteFailed)
    );
}

#[test]
fn assert_seconds_absolute_boundary_matches_chia() {
    let conds = conds_for_solution(
        Bytes32::new([1; 32]),
        100,
        vec![ConditionWithArgs::AssertSecondsAbsolute(10030)],
    );
    assert!(validate_block_conditions(&conds, &seconds_ctx(10030)).is_ok());
    assert_eq!(
        validate_block_conditions(&conds, &seconds_ctx(10029)),
        Err(ChiaError::AssertSecondsAbsoluteFailed)
    );
}

#[test]
fn assert_before_seconds_absolute_boundary_matches_chia() {
    let conds = conds_for_solution(
        Bytes32::new([1; 32]),
        100,
        vec![ConditionWithArgs::AssertBeforeSecondsAbsolute(10031)],
    );
    assert!(validate_block_conditions(&conds, &seconds_ctx(10030)).is_ok());
    assert_eq!(
        validate_block_conditions(&conds, &seconds_ctx(10031)),
        Err(ChiaError::AssertSecondsAbsoluteFailed)
    );
}

#[test]
fn assert_height_relative_boundary_matches_chia() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());
    let conds = conds_for_solution(
        parent,
        amount,
        vec![ConditionWithArgs::AssertHeightRelative(2)],
    );
    let context = CoinSpendContext {
        birth_height: None,
        birth_seconds: None,
        spent_height: Some(2),
        spent_seconds: None,
    };

    let mut pass = height_ctx(4);
    pass.coin_context.insert(coin.name(), context);
    assert!(validate_block_conditions(&conds, &pass).is_ok());

    let mut fail = height_ctx(3);
    fail.coin_context.insert(coin.name(), context);
    assert_eq!(
        validate_block_conditions(&conds, &fail),
        Err(ChiaError::AssertHeightRelativeFailed)
    );
}

#[test]
fn assert_before_height_relative_boundary_matches_chia() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());
    let conds = conds_for_solution(
        parent,
        amount,
        vec![ConditionWithArgs::AssertBeforeHeightRelative(2)],
    );
    let context = CoinSpendContext {
        birth_height: None,
        birth_seconds: None,
        spent_height: Some(2),
        spent_seconds: None,
    };

    let mut pass = height_ctx(3);
    pass.coin_context.insert(coin.name(), context);
    assert!(validate_block_conditions(&conds, &pass).is_ok());

    let mut fail = height_ctx(4);
    fail.coin_context.insert(coin.name(), context);
    assert_eq!(
        validate_block_conditions(&conds, &fail),
        Err(ChiaError::AssertHeightRelativeFailed)
    );
}

#[test]
fn assert_seconds_relative_boundary_matches_chia() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());
    let conds = conds_for_solution(
        parent,
        amount,
        vec![ConditionWithArgs::AssertSecondsRelative(10)],
    );
    let context = CoinSpendContext {
        birth_height: None,
        birth_seconds: None,
        spent_height: None,
        spent_seconds: Some(10020),
    };

    let mut pass = seconds_ctx(10030);
    pass.coin_context.insert(coin.name(), context);
    assert!(validate_block_conditions(&conds, &pass).is_ok());

    let mut fail = seconds_ctx(10029);
    fail.coin_context.insert(coin.name(), context);
    assert_eq!(
        validate_block_conditions(&conds, &fail),
        Err(ChiaError::AssertSecondsRelativeFailed)
    );
}

#[test]
fn assert_my_birth_height_matches_and_mismatches() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());
    let context = CoinSpendContext {
        birth_height: Some(2),
        birth_seconds: None,
        spent_height: Some(2),
        spent_seconds: None,
    };

    let good = conds_for_solution(
        parent,
        amount,
        vec![ConditionWithArgs::AssertMyBirthHeight(2)],
    );
    let mut ok = height_ctx(4);
    ok.coin_context.insert(coin.name(), context);
    assert!(validate_block_conditions(&good, &ok).is_ok());

    let bad = conds_for_solution(
        parent,
        amount,
        vec![ConditionWithArgs::AssertMyBirthHeight(3)],
    );
    let mut mismatch = height_ctx(4);
    mismatch.coin_context.insert(coin.name(), context);
    assert_eq!(
        validate_block_conditions(&bad, &mismatch),
        Err(ChiaError::InvalidCondition)
    );
}

#[test]
fn assert_my_coin_id_valid_and_invalid() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());

    // Valid: the real coin id is accepted.
    let _ = conds_for_solution(
        parent,
        amount,
        vec![ConditionWithArgs::AssertMyCoinId(coin.name())],
    );

    // Invalid: flip one bit of the coin id.
    let mut wrong = coin.name().bytes();
    wrong[31] ^= 1;
    let output = SExp::from(vec![SExp::from(vec![easy_spend(
        parent,
        amount,
        conditions_solution(vec![ConditionWithArgs::AssertMyCoinId(Bytes32::new(wrong))]),
    )])]);
    assert_eq!(
        execute_block_generator_result(&input(quoted_generator(output))),
        Err(ChiaError::InvalidBlockSolution)
    );
}

#[test]
fn announcement_limit_is_mempool_only_consensus_accepts_1025() {
    for count in [1024usize, 1025] {
        let conds = conds_for_solution(
            Bytes32::new([1; 32]),
            100,
            (0..count)
                .map(|_| {
                    ConditionWithArgs::CreateCoinAnnouncement(Message::new(b"x".to_vec()).unwrap())
                })
                .collect(),
        );
        assert!(
            validate_block_conditions(&conds, &Default::default()).is_ok(),
            "block validation must accept {count} announcements"
        );
    }
}

// Two big-endian bytes that decode (signed) to +2^32, one past u32::MAX.
fn u32_overflow_arg() -> Vec<u8> {
    vec![0x01, 0x00, 0x00, 0x00, 0x00]
}

// Nine big-endian bytes that decode (signed) to +2^64, one past u64::MAX.
fn u64_overflow_arg() -> Vec<u8> {
    vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
}

#[test]
fn negative_height_absolute_is_noop_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertHeightAbsolute as u8,
            vec![0xff],
        )],
    )
    .expect("parsing 0xff (-1) as a height must not error");
    // -1 clamps to 0; height_absolute 0 <= block_height 0 => satisfied.
    assert!(validate_block_conditions(&conds, &height_ctx(0)).is_ok());
}

#[test]
fn negative_seconds_absolute_is_noop_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertSecondsAbsolute as u8,
            vec![0xff],
        )],
    )
    .expect("chia accepts a negative seconds assertion as a no-op");
    assert!(validate_block_conditions(&conds, &seconds_ctx(0)).is_ok());
}

#[test]
fn negative_before_height_absolute_rejects_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertBeforeHeightAbsolute as u8,
            vec![0xff],
        )],
    )
    .expect("parsing 0xff (-1) as a before-height must not error");
    assert_eq!(
        validate_block_conditions(&conds, &height_ctx(0)),
        Err(ChiaError::AssertHeightAbsoluteFailed)
    );
}

#[test]
fn negative_before_seconds_absolute_rejects_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertBeforeSecondsAbsolute as u8,
            vec![0xff],
        )],
    )
    .expect("parsing 0xff (-1) as a before-seconds must not error");
    assert_eq!(
        validate_block_conditions(&conds, &seconds_ctx(0)),
        Err(ChiaError::AssertSecondsAbsoluteFailed)
    );
}

#[test]
fn negative_before_height_relative_rejects_like_chia() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());
    let conds = conds_for_raw(
        parent,
        amount,
        vec![raw_condition(
            ConditionOpcode::AssertBeforeHeightRelative as u8,
            vec![0xff],
        )],
    )
    .expect("parsing 0xff (-1) as a before-height-relative must not error");
    let context = CoinSpendContext {
        birth_height: None,
        birth_seconds: None,
        spent_height: Some(2),
        spent_seconds: None,
    };
    let mut ctx = height_ctx(4);
    ctx.coin_context.insert(coin.name(), context);
    assert_eq!(
        validate_block_conditions(&conds, &ctx),
        Err(ChiaError::AssertHeightRelativeFailed)
    );
}

#[test]
fn negative_before_seconds_relative_rejects_like_chia() {
    let parent = Bytes32::new([1; 32]);
    let amount = 100;
    let coin = coin_of(parent, amount, &easy_puzzle());
    let conds = conds_for_raw(
        parent,
        amount,
        vec![raw_condition(
            ConditionOpcode::AssertBeforeSecondsRelative as u8,
            vec![0xff],
        )],
    )
    .expect("parsing 0xff (-1) as a before-seconds-relative must not error");
    let context = CoinSpendContext {
        birth_height: None,
        birth_seconds: None,
        spent_height: None,
        spent_seconds: Some(10020),
    };
    let mut ctx = seconds_ctx(10030);
    ctx.coin_context.insert(coin.name(), context);
    assert_eq!(
        validate_block_conditions(&conds, &ctx),
        Err(ChiaError::AssertSecondsRelativeFailed)
    );
}

// ---- both-direction coverage: one out-of-range (> type max) case per family -
// ASSERT_* (past) family: an unreachable future bound => FAIL.
// ASSERT_BEFORE_* family: a bound past the type max => no-op, PASS.

#[test]
fn overflow_height_absolute_rejects_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertHeightAbsolute as u8,
            u32_overflow_arg(),
        )],
    )
    .expect("parsing an out-of-range height must not error");
    assert_eq!(
        validate_block_conditions(&conds, &height_ctx(3)),
        Err(ChiaError::AssertHeightAbsoluteFailed)
    );
}

#[test]
fn overflow_seconds_absolute_rejects_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertSecondsAbsolute as u8,
            u64_overflow_arg(),
        )],
    )
    .expect("parsing an out-of-range seconds must not error");
    assert_eq!(
        validate_block_conditions(&conds, &seconds_ctx(10030)),
        Err(ChiaError::AssertSecondsAbsoluteFailed)
    );
}

#[test]
fn overflow_before_height_absolute_is_noop_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertBeforeHeightAbsolute as u8,
            u32_overflow_arg(),
        )],
    )
    .expect("parsing an out-of-range before-height must not error");
    assert!(validate_block_conditions(&conds, &height_ctx(3)).is_ok());
}

#[test]
fn overflow_before_seconds_absolute_is_noop_like_chia() {
    let conds = conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![raw_condition(
            ConditionOpcode::AssertBeforeSecondsAbsolute as u8,
            u64_overflow_arg(),
        )],
    )
    .expect("parsing an out-of-range before-seconds must not error");
    assert!(validate_block_conditions(&conds, &seconds_ctx(10030)).is_ok());
}

// The disallowed G1 identity element in compressed form: 0xc0 then 47 zero bytes.
fn g1_infinity_pubkey() -> Bytes48 {
    let mut infinity = [0_u8; 48];
    infinity[0] = 0xc0;
    Bytes48::new(infinity)
}

// Drive a single raw AGG_SIG_* condition through condition aggregation and return
// the block-generator result, so the negative path can inspect the error.
fn conds_for_condition(condition: &ConditionWithArgs) -> Result<SpendBundleConditions, ChiaError> {
    conds_for_raw(
        Bytes32::new([1; 32]),
        100,
        vec![SExp::from(condition).to_owned()],
    )
}

#[test]
fn agg_sig_infinity_pubkey_is_rejected_like_chia() {
    let condition = ConditionWithArgs::AggSigUnsafe(
        g1_infinity_pubkey(),
        Message::new(b"foobar".to_vec()).unwrap(),
    );
    let err = conds_for_condition(&condition).unwrap_err();
    assert_eq!(err, ChiaError::InvalidCondition);
    assert_eq!(err as i64, 10);
}

#[test]
fn agg_sig_infinity_pubkey_rejected_for_every_agg_sig_opcode() {
    let pk = g1_infinity_pubkey();
    let msg = Message::new(b"foobar".to_vec()).unwrap();
    // The full AGG_SIG_* family: AGG_SIG_UNSAFE / AGG_SIG_ME plus the six
    // soft-fork-5 additions. Every opcode must reject the infinity pubkey.
    let conditions = [
        ConditionWithArgs::AggSigParent(pk, msg),
        ConditionWithArgs::AggSigPuzzle(pk, msg),
        ConditionWithArgs::AggSigAmount(pk, msg),
        ConditionWithArgs::AggSigPuzzleAmount(pk, msg),
        ConditionWithArgs::AggSigParentAmount(pk, msg),
        ConditionWithArgs::AggSigParentPuzzle(pk, msg),
        ConditionWithArgs::AggSigUnsafe(pk, msg),
        ConditionWithArgs::AggSigMe(pk, msg),
    ];
    for condition in &conditions {
        let opcode = condition.op_code();
        let err = conds_for_condition(condition).unwrap_err();
        assert_eq!(
            err,
            ChiaError::InvalidCondition,
            "opcode {opcode:?} must reject the G1 infinity pubkey"
        );
    }
}

#[test]
fn agg_sig_non_infinity_pubkey_is_still_accepted() {
    // The fix must not over-reject. A wholly different key, and a near-miss that
    // shares the 0xc0 infinity prefix but carries a non-zero trailing byte, must
    // both pass condition aggregation and land in agg_sig_unsafe.
    let ordinary = Bytes48::new([1; 48]);
    let mut near_miss_bytes = [0_u8; 48];
    near_miss_bytes[0] = 0xc0;
    near_miss_bytes[47] = 0x01;
    let near_miss = Bytes48::new(near_miss_bytes);
    for pk in [ordinary, near_miss] {
        let conds = conds_for_solution(
            Bytes32::new([1; 32]),
            100,
            vec![ConditionWithArgs::AggSigUnsafe(
                pk,
                Message::new(b"foobar".to_vec()).unwrap(),
            )],
        );
        assert_eq!(
            conds.agg_sig_unsafe.len(),
            1,
            "non-infinity pubkey {pk} must be accepted"
        );
    }
}

fn easy_coin_spend(parent: Bytes32, amount: u64, conditions: Vec<ConditionWithArgs>) -> CoinSpend {
    let puzzle = easy_puzzle();
    let solution = Program::to(
        conditions
            .iter()
            .map(|condition| SExp::from(condition).to_owned())
            .collect::<Vec<_>>(),
    );
    CoinSpend {
        coin: Coin {
            parent_coin_info: parent,
            puzzle_hash: puzzle.tree_hash(),
            amount,
        },
        puzzle_reveal: puzzle.serialized().expect("puzzle serializes"),
        solution: solution.serialized().expect("solution serializes"),
    }
}

#[test]
fn spend_bundle_conditions_parse_and_charge_byte_cost() {
    let target = Bytes32::new([9; 32]);
    let bundle = SpendBundle {
        coin_spends: vec![easy_coin_spend(
            Bytes32::new([1; 32]),
            1000,
            vec![ConditionWithArgs::CreateCoin(target, 900, vec![])],
        )],
        aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::default(),
    };
    let conds = conditions_from_spend_bundle(&bundle, 1_000_000, &MAINNET).expect("bundle runs");
    assert_eq!(conds.spends.len(), 1);
    assert_eq!(conds.spends[0].create_coin.len(), 1);
    assert_eq!(conds.addition_amount, 900);
    // Cost = generator-equivalent byte cost + execution + condition cost; the byte term alone
    // exceeds the CREATE_COIN condition cost, so just pin the floor and monotonicity.
    assert!(conds.cost > MAINNET.cost_per_byte * 40, "byte cost charged");
}

#[test]
fn spend_bundle_rejects_wrong_puzzle_reveal() {
    let mut spend = easy_coin_spend(Bytes32::new([1; 32]), 1000, vec![]);
    spend.coin.puzzle_hash = Bytes32::new([0xEE; 32]);
    let bundle = SpendBundle {
        coin_spends: vec![spend],
        aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::default(),
    };
    assert_eq!(
        conditions_from_spend_bundle(&bundle, 1_000_000, &MAINNET),
        Err(ChiaError::WrongPuzzleHash)
    );
}

#[test]
fn spend_bundle_rejects_double_spend() {
    let spend = easy_coin_spend(Bytes32::new([1; 32]), 1000, vec![]);
    let bundle = SpendBundle {
        coin_spends: vec![spend.clone(), spend],
        aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::default(),
    };
    assert!(conditions_from_spend_bundle(&bundle, 1_000_000, &MAINNET).is_err());
}

// ---------------------------------------------------------------------------
// Aggregate-signature verifier equivalence guards (public-API only, so this
// file runs identically against the per-occurrence verifier and the
// deduped-key verifier — the two must be indistinguishable from outside).
// ---------------------------------------------------------------------------
mod agg_sig_verifier_equivalence {
    use blst::min_pk::AggregateSignature;
    use dg_xch_core::blockchain::sized_bytes::Bytes96;
    use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::clvm::bls_bindings::sign;
    use dg_xch_core::consensus::block_generator::validate_block_aggregate_signature;
    use dg_xch_core::consensus::constants::MAINNET;
    use dg_xch_core::errors::ChiaError;
    use dg_xch_core::traits::SizedBytes;

    use blst::min_pk::SecretKey;

    // The deduped verifier validates each distinct key once (on a post-hard-fork mainnet
    // corpus, 76.5% of pair
    // occurrences repeat a key already seen in the block). Repeated-key pair sets are the common
    // case and must verify exactly as the per-occurrence path did — and a tampered message must
    // still reject, proving the dedup map wiring didn't detach any pair from the pairing product.
    #[test]
    fn repeated_pk_pairs_verify_and_tamper_rejects() {
        let sk_a = SecretKey::key_gen_v3(&[11u8; 32], &[]).expect("sk a");
        let sk_b = SecretKey::key_gen_v3(&[13u8; 32], &[]).expect("sk b");
        let mut conds = SpendBundleConditions::default();
        let mut sigs = Vec::new();
        for (sk, msg) in [
            (&sk_a, b"repeated key message 1".to_vec()),
            (&sk_a, b"repeated key message 2".to_vec()),
            (&sk_a, b"repeated key message 3".to_vec()),
            (&sk_b, b"second key message".to_vec()),
        ] {
            sigs.push(sign(sk, &msg));
            conds.agg_sig_unsafe.push((
                UnsizedBytes::new(sk.sk_to_pk().to_bytes().to_vec()),
                UnsizedBytes::new(msg),
            ));
        }
        let aggregate = AggregateSignature::aggregate(&sigs.iter().collect::<Vec<_>>(), true)
            .expect("aggregate")
            .to_signature();
        let aggregate = Bytes96::parse(&aggregate.to_bytes()).expect("sig bytes");
        validate_block_aggregate_signature(&conds, &aggregate, &MAINNET)
            .expect("repeated-key pair set verifies");

        // Tamper with one repeated-key message: the aggregate must now reject.
        conds.agg_sig_unsafe[1].1 = UnsizedBytes::new(b"tampered message 2".to_vec());
        assert_eq!(
            validate_block_aggregate_signature(&conds, &aggregate, &MAINNET),
            Err(ChiaError::BadAggregateSignature),
            "a tampered pair under a repeated key still rejects"
        );
    }

    // Reject a bad key in BOTH verifier branches. The verifier picks its branch from the pair
    // shape: a single occurrence of the key stays on the legacy per-occurrence path (blst
    // validates inside the pairing), while duplicated occurrences (distinct*2 <= pairs) take the
    // deduped path (validate once per distinct key, pairing skips per-occurrence checks). The
    // two must be indistinguishable: same reject, no panic, either shape.
    fn assert_bad_pk_rejects_in_both_branches(bad_pk: Vec<u8>, label: &str) {
        let sk = SecretKey::key_gen_v3(&[17u8; 32], &[]).expect("sk");
        let msg = b"bad pk message".to_vec();
        let sig = sign(&sk, &msg);
        let aggregate = Bytes96::parse(&sig.to_bytes()).expect("sig bytes");
        for occurrences in [1usize, 2] {
            let mut conds = SpendBundleConditions::default();
            for i in 0..occurrences {
                conds.agg_sig_unsafe.push((
                    UnsizedBytes::new(bad_pk.clone()),
                    UnsizedBytes::new(format!("bad pk message {i}").into_bytes()),
                ));
            }
            assert_eq!(
                validate_block_aggregate_signature(&conds, &aggregate, &MAINNET),
                Err(ChiaError::BadAggregateSignature),
                "{label} with {occurrences} occurrence(s) must reject"
            );
        }
    }

    #[test]
    fn malformed_pk_bytes_reject() {
        assert_bad_pk_rejects_in_both_branches(vec![0x01u8; 48], "malformed pk");
    }

    // A block of all-distinct signer keys (the shape that keeps the legacy branch) must verify —
    // pinning that the branch heuristic never rejects a valid distinct-key set.
    #[test]
    fn distinct_pk_pairs_verify_on_legacy_branch() {
        let mut conds = SpendBundleConditions::default();
        let mut sigs = Vec::new();
        for seed in [31u8, 37, 41] {
            let sk = SecretKey::key_gen_v3(&[seed; 32], &[]).expect("sk");
            let msg = format!("distinct key message {seed}").into_bytes();
            sigs.push(sign(&sk, &msg));
            conds.agg_sig_unsafe.push((
                UnsizedBytes::new(sk.sk_to_pk().to_bytes().to_vec()),
                UnsizedBytes::new(msg),
            ));
        }
        let aggregate = AggregateSignature::aggregate(&sigs.iter().collect::<Vec<_>>(), true)
            .expect("aggregate")
            .to_signature();
        let aggregate = Bytes96::parse(&aggregate.to_bytes()).expect("sig bytes");
        validate_block_aggregate_signature(&conds, &aggregate, &MAINNET)
            .expect("distinct-key pair set verifies");
    }

    #[test]
    fn non_subgroup_pk_rejects() {
        let mut non_subgroup_pk = vec![0u8; 48];
        non_subgroup_pk[0] = 0x80; // compressed, not infinity
        non_subgroup_pk[47] = 0x04; // x = 4: on-curve, outside the r-torsion subgroup
        assert_bad_pk_rejects_in_both_branches(non_subgroup_pk, "non-subgroup pk");
    }

    #[test]
    fn infinity_pk_rejects() {
        let mut infinity_pk = vec![0u8; 48];
        infinity_pk[0] = 0xc0;
        assert_bad_pk_rejects_in_both_branches(infinity_pk, "infinity pk");
    }
}
