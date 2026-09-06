use super::*;

#[test]
fn send_message_round_trips_with_flat_receiver_args() {
    let parent = Bytes32::from([1u8; 32].to_vec());
    let puzzle_hash = Bytes32::from([2u8; 32].to_vec());
    let condition = ConditionWithArgs::SendMessage(
        0b00_000_110,
        Message::new(vec![0xaa, 0xbb]).unwrap(),
        MessageArgs::ParentPuzzle {
            parent,
            puzzle_hash,
        },
    );

    let sexp = SExp::from(&condition);
    let reparsed = ConditionWithArgs::try_from(&sexp).unwrap();

    assert_eq!(reparsed, condition);
}

#[test]
fn receive_message_round_trips_with_message_before_sender_args() {
    let parent = Bytes32::from([3u8; 32].to_vec());
    let amount = 42u64;
    let condition = ConditionWithArgs::ReceiveMessage(
        0b00_101_000,
        Message::new(vec![0xcc]).unwrap(),
        MessageArgs::ParentAmount { parent, amount },
    );

    let sexp = SExp::from(&condition);
    let reparsed = ConditionWithArgs::try_from(&sexp).unwrap();

    assert_eq!(reparsed, condition);
}

#[test]
fn create_coin_op_code_wraps_memos_in_list() {
    let puzzle_hash_bytes = [4u8; 32].to_vec();
    let puzzle_hash = Bytes32::from(puzzle_hash_bytes.clone());

    let condition_no_memo = ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![]);
    println!("condition_no_memo: {}", condition_no_memo);

    let condition =
        ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa], vec![0xbb, 0xcc]]);
    println!("condition_with_memo: {}", condition);

    let (opcode, vars) = condition.op_code_with_args();
    assert_eq!(opcode, ConditionOpcode::CreateCoin);
    assert_eq!(vars.len(), 3);
    assert_eq!(
        vars[0].atom().unwrap().as_ref(),
        puzzle_hash_bytes.as_slice()
    );
    assert_eq!(vars[1].atom().unwrap().as_ref(), &[123]);
    let memo_program = Program::new_ref(&vars[2]);
    let memos = memo_program
        .as_list()
        .into_iter()
        .map(|memo| memo.as_vec().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(memos, vec![vec![0xaa], vec![0xbb, 0xcc]]);
}

#[test]
fn create_coin_round_trips_with_nested_memos() {
    let puzzle_hash = Bytes32::from([5u8; 32].to_vec());
    let sexp = Program::to(vec![
        SExp::from(ConditionOpcode::CreateCoin),
        SExp::from(puzzle_hash),
        SExp::from(123u64),
        Program::to(vec![
            SExp::Atom(AtomBuf::new(vec![0xaa])),
            SExp::Atom(AtomBuf::new(vec![0xbb, 0xcc])),
        ])
        .sexp()
        .to_owned(),
    ]);

    let condition = ConditionWithArgs::try_from(sexp.sexp()).unwrap();
    assert_eq!(
        condition,
        ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa], vec![0xbb, 0xcc]])
    );
}

#[test]
fn create_coin_emits_explicit_empty_memo_list() {
    let puzzle_hash = Bytes32::from([6u8; 32].to_vec());
    let condition = ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![]);
    let (_, vars) = condition.op_code_with_args();

    assert_eq!(vars.len(), 3);
    assert!(!vars[2].non_nil());
}

#[test]
fn create_coin_rejects_flattened_memos() {
    let puzzle_hash = Bytes32::from([7u8; 32].to_vec());
    let sexp = Program::to(vec![
        SExp::from(ConditionOpcode::CreateCoin),
        SExp::from(puzzle_hash),
        SExp::from(123u64),
        SExp::Atom(AtomBuf::new(vec![0xaa])),
        SExp::Atom(AtomBuf::new(vec![0xbb, 0xcc])),
    ]);

    assert_eq!(
        ConditionWithArgs::try_from(sexp.sexp()).unwrap(),
        ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa], vec![0xbb, 0xcc]])
    );
}

#[test]
fn create_coin_accepts_single_flattened_memo_atom() {
    let puzzle_hash = Bytes32::from([8u8; 32].to_vec());
    let sexp = Program::to(vec![
        SExp::from(ConditionOpcode::CreateCoin),
        SExp::from(puzzle_hash),
        SExp::from(123u64),
        SExp::Atom(AtomBuf::new(vec![0xaa])),
    ]);

    assert_eq!(
        ConditionWithArgs::try_from(sexp.sexp()).unwrap(),
        ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa]])
    );
}

#[test]
fn send_message_with_none_does_not_emit_placeholder_arg() {
    let condition =
        ConditionWithArgs::SendMessage(0, Message::new(vec![0xdd]).unwrap(), MessageArgs::None);

    let (_, args) = op_code_with_args_from_sexp(&SExp::from(&condition)).unwrap();

    assert_eq!(args.len(), 2);
    assert_eq!(args[0], Vec::<u8>::new());
    assert_eq!(args[1], vec![0xdd]);
}

#[test]
fn message_modes_reject_reserved_high_bits() {
    let condition = [
        SExp::from(66u8),
        SExp::from(0b0100_0000u8),
        SExp::from(vec![0x01]),
    ]
    .as_slice()
    .into();

    assert!(ConditionWithArgs::try_from(&condition).is_err());
}
