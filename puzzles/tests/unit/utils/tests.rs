use super::*;

#[test]
fn create_coin_condition_wraps_memos_in_list() {
    let puzzle_hash = Bytes32::from([5u8; 32].to_vec());
    let condition = make_create_coin_condition(
        puzzle_hash,
        123,
        &[
            UnsizedBytes::new(vec![0xaa]),
            UnsizedBytes::new(vec![0xbb, 0xcc]),
        ],
    );
    println!(
        "create_coin_with_memos_hex={}",
        dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
            Program::to(condition.clone())
                .serialized()
                .unwrap()
                .to_bytes(),
        )
    );
    assert_eq!(
        dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
            Program::to(condition.clone())
                .serialized()
                .unwrap()
                .to_bytes(),
        )
        .to_string(),
        "0xff33ffa00505050505050505050505050505050505050505050505050505050505050505ff7bffff81aaff82bbcc8080"
    );

    assert_eq!(condition.len(), 4);
    let memos = Program::new_ref(&condition[3])
        .as_list()
        .into_iter()
        .map(|memo| memo.as_vec().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(memos, vec![vec![0xaa], vec![0xbb, 0xcc]]);
}

#[test]
fn create_coin_condition_emits_explicit_empty_memo_list() {
    let puzzle_hash = Bytes32::from([6u8; 32].to_vec());
    let condition = make_create_coin_condition(puzzle_hash, 123, &[]);
    println!(
        "create_coin_empty_memos_hex={}",
        dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
            Program::to(condition.clone())
                .serialized()
                .unwrap()
                .to_bytes(),
        )
    );
    assert_eq!(
        dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::new(
            Program::to(condition.clone())
                .serialized()
                .unwrap()
                .to_bytes(),
        )
        .to_string(),
        "0xff33ffa00606060606060606060606060606060606060606060606060606060606060606ff7bff8080"
    );

    assert_eq!(condition.len(), 4);
}
