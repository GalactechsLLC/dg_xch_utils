use super::*;
use dg_xch_core::clvm::sexp::{AtomBuf, SExp};
use dg_xch_puzzles::p2_conditions::puzzle_for_conditions;

#[test]
fn compute_memos_for_spend_reads_nested_memo_list() {
    let parent_coin = Coin {
        parent_coin_info: Bytes32::from([1u8; 32].to_vec()),
        puzzle_hash: Bytes32::from([2u8; 32].to_vec()),
        amount: 500,
    };
    let created_puzzle_hash = Bytes32::from([3u8; 32].to_vec());
    let conditions = vec![make_create_coin_condition(
        created_puzzle_hash,
        123,
        &[
            UnsizedBytes::new(vec![0xaa]),
            UnsizedBytes::new(vec![0xbb, 0xcc]),
        ],
    )];
    let puzzle = puzzle_for_conditions(conditions.clone()).unwrap();
    let full_solution = solution_for_conditions(conditions).unwrap();
    let coin_spend = CoinSpend {
        coin: parent_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(0).serialized().unwrap(),
    };
    println!(
        "coin_spend_solution_hex={}",
        hex::encode(coin_spend.solution.clone())
    );
    println!(
        "coin_spend_puzzle_reveal_hex={}",
        hex::encode(coin_spend.puzzle_reveal.clone())
    );
    assert_eq!(
        dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
            full_solution.serialized().unwrap().to_bytes()
        )
        .to_string(),
        "0xff80ffff01ffff33ffa00303030303030303030303030303030303030303030303030303030303030303ff7bffff81aaff82bbcc808080ff8080"
    );

    let memos = compute_memos_for_spend(&coin_spend).unwrap();
    let created_coin = Coin {
        parent_coin_info: parent_coin.name(),
        puzzle_hash: created_puzzle_hash,
        amount: 123,
    };
    assert_eq!(
        memos.get(&created_coin.name()),
        Some(&vec![vec![0xaa], vec![0xbb, 0xcc]])
    );
}

#[test]
fn compute_memos_for_spend_reads_explicit_empty_memo_list() {
    let parent_coin = Coin {
        parent_coin_info: Bytes32::from([1u8; 32].to_vec()),
        puzzle_hash: Bytes32::from([2u8; 32].to_vec()),
        amount: 500,
    };
    let created_puzzle_hash = Bytes32::from([4u8; 32].to_vec());
    let conditions = vec![make_create_coin_condition(created_puzzle_hash, 123, &[])];
    let puzzle = puzzle_for_conditions(conditions.clone()).unwrap();
    let full_solution = solution_for_conditions(conditions).unwrap();
    let coin_spend = CoinSpend {
        coin: parent_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(0).serialized().unwrap(),
    };
    println!(
        "coin_spend_empty_memos_solution_hex={}",
        hex::encode(coin_spend.solution.clone())
    );
    println!(
        "coin_spend_empty_memos_puzzle_reveal_hex={}",
        hex::encode(coin_spend.puzzle_reveal.clone())
    );
    assert_eq!(
        dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
            full_solution.serialized().unwrap().to_bytes()
        )
        .to_string(),
        "0xff80ffff01ffff33ffa00404040404040404040404040404040404040404040404040404040404040404ff7bff808080ff8080"
    );

    let memos = compute_memos_for_spend(&coin_spend).unwrap();
    let created_coin = Coin {
        parent_coin_info: parent_coin.name(),
        puzzle_hash: created_puzzle_hash,
        amount: 123,
    };
    assert_eq!(memos.get(&created_coin.name()), Some(&Vec::new()));
}

#[test]
fn compute_memos_for_spend_accepts_flattened_legacy_memos() {
    let parent_coin = Coin {
        parent_coin_info: Bytes32::from([1u8; 32].to_vec()),
        puzzle_hash: Bytes32::from([2u8; 32].to_vec()),
        amount: 500,
    };
    let created_puzzle_hash = Bytes32::from([9u8; 32].to_vec());
    let legacy_condition = Program::to(vec![
        SExp::from(ConditionOpcode::CreateCoin),
        SExp::from(created_puzzle_hash),
        SExp::from(123u64),
        SExp::Atom(AtomBuf::new(vec![0xaa])),
        SExp::Atom(AtomBuf::new(vec![0xbb, 0xcc])),
    ]);
    let puzzle = puzzle_for_conditions(vec![legacy_condition.sexp().to_owned()]).unwrap();
    let coin_spend = CoinSpend {
        coin: parent_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(0).serialized().unwrap(),
    };

    let memos = compute_memos_for_spend(&coin_spend).unwrap();
    let created_coin = Coin {
        parent_coin_info: parent_coin.name(),
        puzzle_hash: created_puzzle_hash,
        amount: 123,
    };
    assert_eq!(
        memos.get(&created_coin.name()),
        Some(&vec![vec![0xaa], vec![0xbb, 0xcc]])
    );
}

#[test]
fn compute_memos_for_spend_accepts_helper_style_no_memo_arg() {
    let parent_coin = Coin {
        parent_coin_info: Bytes32::from([1u8; 32].to_vec()),
        puzzle_hash: Bytes32::from([2u8; 32].to_vec()),
        amount: 500,
    };
    let created_puzzle_hash = Bytes32::from([10u8; 32].to_vec());
    let helper_style_condition = Program::to(vec![
        SExp::from(ConditionOpcode::CreateCoin),
        SExp::from(created_puzzle_hash),
        SExp::from(123u64),
    ]);
    let puzzle = puzzle_for_conditions(vec![helper_style_condition.sexp().to_owned()]).unwrap();
    let coin_spend = CoinSpend {
        coin: parent_coin,
        puzzle_reveal: puzzle.serialized().unwrap(),
        solution: Program::to(0).serialized().unwrap(),
    };

    let memos = compute_memos_for_spend(&coin_spend).unwrap();
    let created_coin = Coin {
        parent_coin_info: parent_coin.name(),
        puzzle_hash: created_puzzle_hash,
        amount: 123,
    };
    assert_eq!(memos.get(&created_coin.name()), None);
}
