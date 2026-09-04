use super::{
    BlockGeneratorFlags, BlockGeneratorInput, execute_block_generator_result,
    simple_solution_generator,
};
use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::sexp::{AtomBuf, PairBuf, SExp};
use crate::consensus::constants::MAINNET;
use crate::traits::SizedBytes;
use num_bigint::BigInt;
use std::sync::Arc;

fn atom(bytes: Vec<u8>) -> SExp<'static> {
    SExp::Atom(AtomBuf::Owned(Arc::new(bytes)))
}

// The producer's output MUST round-trip through our own validator: assemble a plain generator
// from a coin spend whose puzzle creates a coin, run it through execute_block_generator_result,
// and get exactly that spend + created coin back.
#[test]
fn simple_generator_round_trips_through_our_validator() {
    let created_ph = Bytes32::from([0x11u8; 32]);
    let created_amount = 500u64;

    // puzzle = (q . ((51 created_ph created_amount)))  — returns one CREATE_COIN, any solution.
    let create_coin = SExp::from(vec![
        SExp::from(&BigInt::from(51u8)), // CREATE_COIN opcode
        atom(created_ph.bytes().to_vec()),
        SExp::from(&BigInt::from(created_amount)),
    ]);
    let conditions = SExp::from(vec![create_coin]);
    let puzzle = SExp::Pair(PairBuf::Owned((
        Arc::new(atom(vec![1u8])),
        Arc::new(conditions),
    )));
    let puzzle_reveal = sexp_to_bytes(&puzzle).expect("serialize puzzle");
    let solution = sexp_to_bytes(&atom(vec![])).expect("serialize nil solution");

    let coin = Coin {
        parent_coin_info: Bytes32::from([0x22u8; 32]),
        puzzle_hash: Bytes32::from([0u8; 32]), // unused by the generator (derived from reveal)
        amount: 1000,
    };
    let spend = CoinSpend {
        coin,
        puzzle_reveal,
        solution,
    };

    let generator = simple_solution_generator(&[spend]).expect("assemble generator");
    // It is a SIMPLE generator: (q . …) => 0xff 0x01 …
    assert!(
        generator.as_ref().starts_with(&[0xff, 0x01]),
        "expected a simple (quoted) generator"
    );

    let height = MAINNET.hard_fork_height + 4000;
    let input = BlockGeneratorInput {
        transactions_generator: generator,
        generator_refs: Vec::new(),
        constants: MAINNET,
        height,
        flags: BlockGeneratorFlags::for_height(&MAINNET, height),
    };
    assert!(
        input.flags.simple_generator,
        "test must exercise the simple path"
    );

    let conds = execute_block_generator_result(&input).expect("generator runs in our validator");
    assert_eq!(conds.spends.len(), 1, "exactly the one spend we assembled");
    let created: Vec<_> = conds.spends[0]
        .create_coin
        .iter()
        .filter(|c| c.puzzle_hash == created_ph && c.amount == created_amount)
        .collect();
    assert_eq!(
        created.len(),
        1,
        "the assembled spend created the expected coin ({created_ph}, {created_amount})"
    );
}

#[test]
fn solution_generator_wire_format_is_stable() {
    use crate::consensus::block_generator::solution_generator_from_coin_spends;

    let puzzle1 = hex::decode(concat!(
        "ff02ffff01ff02ffff01ff02ffff03ff0bffff01ff02ffff03ffff09ff05ffff",
        "1dff0bffff1effff0bff0bffff02ff06ffff04ff02ffff04ff17ff8080808080",
        "808080ffff01ff02ff17ff2f80ffff01ff088080ff0180ffff01ff04ffff04ff",
        "04ffff04ff05ffff04ffff02ff06ffff04ff02ffff04ff17ff80808080ff8080",
        "8080ffff02ff17ff2f808080ff0180ffff04ffff01ff32ff02ffff03ffff07ff",
        "0580ffff01ff0bffff0102ffff02ff06ffff04ff02ffff04ff09ff80808080ff",
        "ff02ff06ffff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff0580",
        "80ff0180ff018080ffff04ffff01b08cf5533a94afae0f4613d3ea565e47abc5",
        "373415967ef5824fd009c602cb629e259908ce533c21de7fd7a68eb96c52d0ff",
        "018080"
    ))
    .unwrap();
    let solution1 = hex::decode(concat!(
        "ff80ffff01ffff3dffa080115c1c71035a2cd60a49499fb9e5cb55be8d6e25e8",
        "680bfc0409b7acaeffd48080ff8080"
    ))
    .unwrap();
    let puzzle2 = hex::decode(concat!(
        "ff01ffff33ffa01b7ab2079fa635554ad9bd4812c622e46ee3b1875a7813afba",
        "127bb0cc9794f9ff887f808e9291e6c00080ffff33ffa06f184a7074c925ef86",
        "88ce56941eb8929be320265f824ec7e351356cc745d38aff887f808e9291e6c0",
        "008080"
    ))
    .unwrap();
    let solution2 = hex::decode("80").unwrap();

    let coin1 = Coin {
        parent_coin_info: Bytes32::parse(
            &hex::decode("ccd5bb71183532bff220ba46c268991a00000000000000000000000000036840")
                .unwrap(),
        )
        .unwrap(),
        puzzle_hash: Bytes32::parse(
            &hex::decode("fcc78a9e396df6ceebc217d2446bc016e0b3d5922fb32e5783ec5a85d490cfb6")
                .unwrap(),
        )
        .unwrap(),
        amount: 1_750_000_000_000,
    };
    let coin2 = Coin {
        parent_coin_info: Bytes32::parse(
            &hex::decode("ccd5bb71183532bff220ba46c268991a00000000000000000000000000000000")
                .unwrap(),
        )
        .unwrap(),
        puzzle_hash: Bytes32::parse(
            &hex::decode("d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc")
                .unwrap(),
        )
        .unwrap(),
        amount: 18_375_000_000_000_000_000,
    };

    let spends = [
        CoinSpend {
            coin: coin1,
            puzzle_reveal: crate::clvm::program::SerializedProgram::from(puzzle1.clone()),
            solution: crate::clvm::program::SerializedProgram::from(solution1.clone()),
        },
        CoinSpend {
            coin: coin2,
            puzzle_reveal: crate::clvm::program::SerializedProgram::from(puzzle2.clone()),
            solution: crate::clvm::program::SerializedProgram::from(solution2.clone()),
        },
    ];

    let generator = solution_generator_from_coin_spends(&spends).expect("assemble");
    let expected = hex::decode(
        [
            "ff01ffffffa0",
            "ccd5bb71183532bff220ba46c268991a00000000000000000000000000000000",
            "ff",
            "ff01ffff33ffa01b7ab2079fa635554ad9bd4812c622e46ee3b1875a7813afba",
            "127bb0cc9794f9ff887f808e9291e6c00080ffff33ffa06f184a7074c925ef86",
            "88ce56941eb8929be320265f824ec7e351356cc745d38aff887f808e9291e6c0",
            "008080",
            "ff8900ff011d2523cd8000ff",
            "80",
            "80ffffa0",
            "ccd5bb71183532bff220ba46c268991a00000000000000000000000000036840",
            "ff",
            "ff02ffff01ff02ffff01ff02ffff03ff0bffff01ff02ffff03ffff09ff05ffff",
            "1dff0bffff1effff0bff0bffff02ff06ffff04ff02ffff04ff17ff8080808080",
            "808080ffff01ff02ff17ff2f80ffff01ff088080ff0180ffff01ff04ffff04ff",
            "04ffff04ff05ffff04ffff02ff06ffff04ff02ffff04ff17ff80808080ff8080",
            "8080ffff02ff17ff2f808080ff0180ffff04ffff01ff32ff02ffff03ffff07ff",
            "0580ffff01ff0bffff0102ffff02ff06ffff04ff02ffff04ff09ff80808080ff",
            "ff02ff06ffff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff0580",
            "80ff0180ff018080ffff04ffff01b08cf5533a94afae0f4613d3ea565e47abc5",
            "373415967ef5824fd009c602cb629e259908ce533c21de7fd7a68eb96c52d0ff",
            "018080",
            "ff8601977420dc00ff",
            "ff80ffff01ffff3dffa080115c1c71035a2cd60a49499fb9e5cb55be8d6e25e8",
            "680bfc0409b7acaeffd48080ff8080",
            "808080",
        ]
        .concat(),
    )
    .unwrap();
    assert_eq!(
        generator.as_ref(),
        expected.as_slice(),
        "solution_generator_from_coin_spends must emit the reference bytes exactly"
    );
}

#[test]
fn compressed_generator_uses_backrefs() {
    use crate::consensus::block_generator::compressed_solution_generator_from_coin_spends;

    let spends = backref_fixture_spends();
    let generator =
        compressed_solution_generator_from_coin_spends(&spends).expect("assemble compressed");
    let expected = hex::decode(
        [
            "ff01ffffffa0",
            "ccd5bb71183532bff220ba46c268991a00000000000000000000000000000000",
            "ff",
            "ff01ffff33ffa01b7ab2079fa635554ad9bd4812c622e46ee3b1875a7813afba",
            "127bb0cc9794f9ff887f808e9291e6c00080ffff33ffa06f184a7074c925ef86",
            "88ce56941eb8929be320265f824ec7e351356cc745d38a",
            "fe3b",
            "80ff8900ff011d2523cd8000ff8080ffffa0",
            "ccd5bb71183532bff220ba46c268991a00000000000000000000000000036840",
            "ff",
            "ff02ffff01ff02ffff01ff02ffff03ff0bffff01ff02ffff03ffff09ff05ffff",
            "1dff0bffff1effff0bff0bffff02ff06ffff04ff02ffff04ff17ff8080808080",
            "808080ffff01ff02ff17ff2f80ffff01ff088080ff0180ffff01ff04ffff04ff",
            "04ffff04ff05ffff04ff",
            "fe8401",
            "6b6b7fff80808080ff",
            "fe820d",
            "b78080",
            "ff0180",
            "ffff04ffff01ff32ff02ffff03ffff07ff0580ffff01ff0bffff0102ffff02ff",
            "06ffff04ff02ffff04ff09ff80808080ffff02ff06ffff04ff02ffff04ff0dff",
            "8080808080ffff01ff0bffff0101",
            "ff0580",
            "80ff0180",
            "ff0180",
            "80ffff04ffff01b08cf5533a94afae0f4613d3ea565e47abc5373415967ef582",
            "4fd009c602cb629e259908ce533c21de7fd7a68eb96c52d0",
            "ff0180",
            "80ff8601977420dc00ffff80ffff01ffff3dffa080115c1c71035a2cd60a4949",
            "9fb9e5cb55be8d6e25e8680bfc0409b7acaeffd48080ff8080808080",
        ]
        .concat(),
    )
    .unwrap();
    assert_eq!(
        generator.as_ref(),
        expected.as_slice(),
        "compressed_solution_generator must emit the reference back-reference bytes exactly"
    );
}

fn backref_fixture_spends() -> Vec<CoinSpend> {
    use crate::clvm::program::SerializedProgram;
    let puzzle1 = hex::decode(concat!(
        "ff02ffff01ff02ffff01ff02ffff03ff0bffff01ff02ffff03ffff09ff05ffff",
        "1dff0bffff1effff0bff0bffff02ff06ffff04ff02ffff04ff17ff8080808080",
        "808080ffff01ff02ff17ff2f80ffff01ff088080ff0180ffff01ff04ffff04ff",
        "04ffff04ff05ffff04ffff02ff06ffff04ff02ffff04ff17ff80808080ff8080",
        "8080ffff02ff17ff2f808080ff0180ffff04ffff01ff32ff02ffff03ffff07ff",
        "0580ffff01ff0bffff0102ffff02ff06ffff04ff02ffff04ff09ff80808080ff",
        "ff02ff06ffff04ff02ffff04ff0dff8080808080ffff01ff0bffff0101ff0580",
        "80ff0180ff018080ffff04ffff01b08cf5533a94afae0f4613d3ea565e47abc5",
        "373415967ef5824fd009c602cb629e259908ce533c21de7fd7a68eb96c52d0ff",
        "018080"
    ))
    .unwrap();
    let solution1 = hex::decode(concat!(
        "ff80ffff01ffff3dffa080115c1c71035a2cd60a49499fb9e5cb55be8d6e25e8",
        "680bfc0409b7acaeffd48080ff8080"
    ))
    .unwrap();
    let puzzle2 = hex::decode(concat!(
        "ff01ffff33ffa01b7ab2079fa635554ad9bd4812c622e46ee3b1875a7813afba",
        "127bb0cc9794f9ff887f808e9291e6c00080ffff33ffa06f184a7074c925ef86",
        "88ce56941eb8929be320265f824ec7e351356cc745d38aff887f808e9291e6c0",
        "008080"
    ))
    .unwrap();
    let solution2 = hex::decode("80").unwrap();
    let coin1 = Coin {
        parent_coin_info: Bytes32::parse(
            &hex::decode("ccd5bb71183532bff220ba46c268991a00000000000000000000000000036840")
                .unwrap(),
        )
        .unwrap(),
        puzzle_hash: Bytes32::parse(
            &hex::decode("fcc78a9e396df6ceebc217d2446bc016e0b3d5922fb32e5783ec5a85d490cfb6")
                .unwrap(),
        )
        .unwrap(),
        amount: 1_750_000_000_000,
    };
    let coin2 = Coin {
        parent_coin_info: Bytes32::parse(
            &hex::decode("ccd5bb71183532bff220ba46c268991a00000000000000000000000000000000")
                .unwrap(),
        )
        .unwrap(),
        puzzle_hash: Bytes32::parse(
            &hex::decode("d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc")
                .unwrap(),
        )
        .unwrap(),
        amount: 18_375_000_000_000_000_000,
    };
    vec![
        CoinSpend {
            coin: coin1,
            puzzle_reveal: SerializedProgram::from(puzzle1),
            solution: SerializedProgram::from(solution1),
        },
        CoinSpend {
            coin: coin2,
            puzzle_reveal: SerializedProgram::from(puzzle2),
            solution: SerializedProgram::from(solution2),
        },
    ]
}

// The compressed generator must (a) round-trip through the back-ref DECODER to the SAME program
// the plain form encodes, (b) be strictly smaller when a subtree repeats, and (c) run through OUR
// validator to the IDENTICAL cost and conditions as the plain form. Built from three spends of
// the SAME create-coin puzzle (distinct parents), so the 34-byte puzzle reveal repeats and
// compresses — the packing lever, proven end to end against our own validator.
#[test]
fn compressed_generator_round_trips_and_validates_like_plain() {
    use crate::clvm::parser::{sexp_from_bytes_backrefs, sexp_to_bytes};
    use crate::consensus::block_generator::{
        compressed_solution_generator_from_coin_spends, solution_generator_from_coin_spends,
    };
    use std::io::Cursor;

    let created_ph = Bytes32::from([0x11u8; 32]);
    let created_amount = 500u64;
    // puzzle = (q . ((51 created_ph created_amount))) — one CREATE_COIN, any solution. Identical
    // across every spend, so its serialized reveal is a repeated subtree the back-ref serializer
    // deduplicates.
    let create_coin = SExp::from(vec![
        SExp::from(&BigInt::from(51u8)),
        atom(created_ph.bytes().to_vec()),
        SExp::from(&BigInt::from(created_amount)),
    ]);
    let conditions = SExp::from(vec![create_coin]);
    let puzzle = SExp::Pair(PairBuf::Owned((
        Arc::new(atom(vec![1u8])),
        Arc::new(conditions),
    )));
    let puzzle_reveal = sexp_to_bytes(&puzzle).expect("serialize puzzle");
    let solution = sexp_to_bytes(&atom(vec![])).expect("serialize nil solution");

    let spends: Vec<CoinSpend> = (0u8..3)
        .map(|i| CoinSpend {
            coin: Coin {
                parent_coin_info: Bytes32::from([0x40u8 + i; 32]),
                puzzle_hash: Bytes32::from([0u8; 32]),
                amount: 1000 + u64::from(i),
            },
            puzzle_reveal: puzzle_reveal.clone(),
            solution: solution.clone(),
        })
        .collect();

    let plain = solution_generator_from_coin_spends(&spends).expect("plain");
    let compressed = compressed_solution_generator_from_coin_spends(&spends).expect("compressed");

    // (d) the packing lever: repeated puzzle reveal ⇒ strictly smaller.
    assert!(
        compressed.as_ref().len() < plain.as_ref().len(),
        "compressed ({}) must be smaller than plain ({}) when the puzzle repeats",
        compressed.as_ref().len(),
        plain.as_ref().len()
    );

    // (a) round-trip: both decode (via the back-ref decoder validation uses) to the same tree.
    let plain_tree =
        sexp_from_bytes_backrefs(&mut Cursor::new(plain.as_ref())).expect("decode plain");
    let compressed_tree =
        sexp_from_bytes_backrefs(&mut Cursor::new(compressed.as_ref())).expect("decode compressed");
    assert_eq!(
        compressed_tree, plain_tree,
        "compressed must decode to the identical program"
    );

    // (c) validates to identical cost + conditions under our own validator.
    let height = MAINNET.hard_fork_height + 4000;
    let run = |prog: crate::clvm::program::SerializedProgram| {
        execute_block_generator_result(&BlockGeneratorInput {
            transactions_generator: prog,
            generator_refs: Vec::new(),
            constants: MAINNET,
            height,
            flags: BlockGeneratorFlags::for_height(&MAINNET, height),
        })
        .expect("generator runs in our validator")
    };
    let plain_len = plain.as_ref().len() as u64;
    let compressed_len = compressed.as_ref().len() as u64;
    let plain_conds = run(plain);
    let compressed_conds = run(compressed);
    // (c) same program ⇒ same conditions; the ONLY cost difference is the byte cost, which drops
    // by exactly the serialized-size saving × cost_per_byte. This is the packing win made
    // concrete: fewer bytes ⇒ lower block cost ⇒ more room under MAX_BLOCK_COST_CLVM.
    assert!(
        compressed_conds.cost < plain_conds.cost,
        "compressed cost {} must be below plain cost {}",
        compressed_conds.cost,
        plain_conds.cost
    );
    assert_eq!(
        plain_conds.cost - compressed_conds.cost,
        (plain_len - compressed_len) * MAINNET.cost_per_byte,
        "the whole cost delta is the byte-cost saving; execution + condition cost is unchanged"
    );
    assert_eq!(
        compressed_conds.spends.len(),
        plain_conds.spends.len(),
        "same spends recovered"
    );
    assert_eq!(compressed_conds.spends.len(), 3, "all three spends present");
}
