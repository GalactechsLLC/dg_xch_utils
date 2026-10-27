//! Splitting a block's executed conditions into the coins it removes (the spent coin ids)
//! and the coins it adds (every CREATE_COIN output, parented to its spend):
//! `removals_for_conditions` + `additions_for_conditions` in
//! `core/src/consensus/block_generator.rs`. Both are driven through a real generator
//! execution so the parse -> aggregate -> extract path is exercised end to end.
//!
//! The `TEST_GENERATOR` fixture is vendored as bytes.

use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, additions_for_conditions,
    execute_block_generator_result, removals_for_conditions,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::errors::ChiaError;
use dg_xch_core::traits::SizedBytes;

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

fn coin_of(parent: Bytes32, amount: u64, puzzle: &Program<'static>) -> Coin {
    Coin {
        parent_coin_info: parent,
        puzzle_hash: puzzle.tree_hash(),
        amount,
    }
}

fn simple_input(generator: SerializedProgram) -> BlockGeneratorInput {
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

fn empty_conditions() -> SpendBundleConditions {
    SpendBundleConditions {
        falcon_signatures: Vec::new(),
        spends: Vec::new(),
        reserve_fee: 0,
        height_absolute: 0,
        seconds_absolute: 0,
        before_height_absolute: None,
        before_seconds_absolute: None,
        agg_sig_unsafe: Vec::new(),
        cost: 0,
        removal_amount: 0,
        addition_amount: 0,
    }
}

// --- tx_removals_and_additions ---------------------------------------------

// Two spends, each creating three coins
// (one non-zero amount, two zero-amount with a long hint). Removals are the two
// spent coin ids, additions are all six created coins parented to their spend.
#[test]
fn tx_removals_and_additions_split_removed_and_created_coins() {
    let parent_a = Bytes32::new([0xA0; 32]);
    let parent_b = Bytes32::new([0xB0; 32]);
    let hint = vec![b'1'; 300];

    let puzzle_a = puzzle_for_conditions(vec![
        ConditionWithArgs::CreateCoin(Bytes32::new([2; 32]), 123, vec![]),
        ConditionWithArgs::CreateCoin(Bytes32::new([3; 32]), 0, vec![hint.clone()]),
        ConditionWithArgs::CreateCoin(Bytes32::new([4; 32]), 0, vec![hint.clone()]),
    ]);
    let puzzle_b = puzzle_for_conditions(vec![
        ConditionWithArgs::CreateCoin(Bytes32::new([5; 32]), 123, vec![]),
        ConditionWithArgs::CreateCoin(Bytes32::new([6; 32]), 0, vec![hint.clone()]),
        ConditionWithArgs::CreateCoin(Bytes32::new([7; 32]), 0, vec![hint.clone()]),
    ]);

    let coin_a = coin_of(parent_a, 100, &puzzle_a);
    let coin_b = coin_of(parent_b, 100, &puzzle_b);

    let output = SExp::from(vec![SExp::from(vec![
        spend_output(parent_a, 100, &puzzle_a),
        spend_output(parent_b, 100, &puzzle_b),
    ])]);
    let conds = execute_block_generator_result(&simple_input(quoted_generator(output))).unwrap();

    // Removals: exactly the two spent coin ids.
    let mut removals = removals_for_conditions(&conds);
    removals.sort_by_key(SizedBytes::bytes);
    let mut expected_removals = vec![coin_a.name(), coin_b.name()];
    expected_removals.sort_by_key(SizedBytes::bytes);
    assert_eq!(removals, expected_removals);

    // Additions: every CREATE_COIN, parented to the spend that made it.
    let mut additions = additions_for_conditions(&conds, &[]);
    additions.sort_by_key(|coin| coin.name().bytes());
    let mut expected_additions = vec![
        Coin {
            parent_coin_info: coin_a.name(),
            puzzle_hash: Bytes32::new([2; 32]),
            amount: 123,
        },
        Coin {
            parent_coin_info: coin_a.name(),
            puzzle_hash: Bytes32::new([3; 32]),
            amount: 0,
        },
        Coin {
            parent_coin_info: coin_a.name(),
            puzzle_hash: Bytes32::new([4; 32]),
            amount: 0,
        },
        Coin {
            parent_coin_info: coin_b.name(),
            puzzle_hash: Bytes32::new([5; 32]),
            amount: 123,
        },
        Coin {
            parent_coin_info: coin_b.name(),
            puzzle_hash: Bytes32::new([6; 32]),
            amount: 0,
        },
        Coin {
            parent_coin_info: coin_b.name(),
            puzzle_hash: Bytes32::new([7; 32]),
            amount: 0,
        },
    ];
    expected_additions.sort_by_key(|coin| coin.name().bytes());
    assert_eq!(additions, expected_additions);
}

// Empty conditions yield ([], []).
#[test]
fn empty_conditions_have_no_removals_or_additions() {
    let conds = empty_conditions();
    assert!(removals_for_conditions(&conds).is_empty());
    assert!(additions_for_conditions(&conds, &[]).is_empty());
}

// --- get_spends_for_trusted_block (TEST_GENERATOR) -------------------------

// `TEST_GENERATOR` is a malicious generator: a `830f4240` (quote 1_000_000) loop counter
// drives ~1024 conditions with a runaway CLVM cost. A trusted structural extractor would
// return the single spend (coin 0101..*32 / puzzle 0x80 / amount 123, 1024 conditions)
// without enforcing block cost; the only entry point here,
// `execute_block_generator_result`, is the full cost-accounted validator, so it correctly
// REJECTS this generator. Under the compact-arena VM the rejection is
// `GeneratorRuntimeError`: the runaway loop allocates pairs faster than it accrues cost
// and trips the consensus pair-pool limit (MAX_NUM_PAIRS = 62,500,000) just before the
// block cost roof.
const TEST_GENERATOR_HEX: &str = "ff02ffff01ff02ffff01ff04ffff04ffff04ffff01a00101010101010101010101010101010101010101010101010101010101010101ffff04ffff04ffff0101ffff02ff02ffff04ff02ffff04ff05ffff04ff0bffff04ff17ff80808080808080ffff01ff7bffff80ffff018080808080ff8080ff8080ffff04ffff01ff02ffff03ff17ffff01ff04ff05ffff04ff0bffff02ff02ffff04ff02ffff04ff05ffff04ff0bffff04ffff11ff17ffff010180ff8080808080808080ff8080ff0180ff018080ffff04ffff01ff42ff24ff8568656c6c6fffa0010101010101010101010101010101010101010101010101010101010101010180ffff04ffff01ff43ff24ff8568656c6c6fffa0010101010101010101010101010101010101010101010101010101010101010180ffff04ffff01830f4240ff0180808080";

fn legacy_input(generator: SerializedProgram, height: u32) -> BlockGeneratorInput {
    BlockGeneratorInput {
        transactions_generator: generator,
        generator_refs: Vec::new(),
        constants: MAINNET,
        height,
        flags: BlockGeneratorFlags::default(),
    }
}

#[test]
fn malicious_test_generator_is_rejected_for_exceeding_block_cost() {
    let generator = SerializedProgram::from_hex(TEST_GENERATOR_HEX).unwrap();
    // The strict full-validation path refuses the runaway generator (pair-pool limit,
    // see above).
    assert_eq!(
        execute_block_generator_result(&legacy_input(generator, 100)),
        Err(ChiaError::GeneratorRuntimeError)
    );
}
