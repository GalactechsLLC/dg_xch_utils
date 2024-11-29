#[test]
pub fn test_uncurry() {
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use dg_xch_core::clvm::assemble::assemble_text;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::IntoSExp;
    let program = assemble_text(
        "(a (q 2 (q 2 94 (c 2 (c (c 5 (c (sha256 44 5) (c 11 ()))) (c (a 23 47) (c 95 (c (a 46 (c 2 (c 23 ()))) (c (sha256 639 1407 2943) (c -65 (c 383 (c 767 (c 1535 (c 3071 ())))))))))))) (c (q (((-54 . 61) 70 2 . 51) (60 . 4) 1 1 . -53) ((a 2 (i 5 (q 2 50 (c 2 (c 13 (c (sha256 34 (sha256 44 52) (sha256 34 (sha256 34 (sha256 44 92) 9) (sha256 34 11 (sha256 44 ())))) ())))) (q . 11)) 1) (a (i 11 (q 2 (i (= (a 46 (c 2 (c 19 ()))) 2975) (q 2 38 (c 2 (c (a 19 (c 95 (c 23 (c 47 (c -65 (c 383 (c 27 ()))))))) (c 383 ())))) (q 8)) 1) (q 2 (i 23 (q 2 (i (not -65) (q . 383) (q 8)) 1) (q 8)) 1)) 1) (c (c 5 39) (c (+ 11 87) 119)) 2 (i 5 (q 2 (i (= (a (i (= 17 120) (q . 89) ()) 1) (q . -113)) (q 2 122 (c 2 (c 13 (c 11 (c (c -71 377) ()))))) (q 2 90 (c 2 (c (a (i (= 17 120) (q 4 120 (c (a 54 (c 2 (c 19 (c 41 (c (sha256 44 91) (c 43 ())))))) 57)) (q 2 (i (= 17 36) (q 4 36 (c (sha256 32 41) 57)) (q . 9)) 1)) 1) (c (a (i (= 17 120) (q . 89) ()) 1) (c (a 122 (c 2 (c 13 (c 11 (c 23 ()))))) ())))))) 1) (q 4 () (c () 23))) 1) ((a (i 5 (q 4 9 (a 38 (c 2 (c 13 (c 11 ()))))) (q . 11)) 1) 11 34 (sha256 44 88) (sha256 34 (sha256 34 (sha256 44 92) 5) (sha256 34 (a 50 (c 2 (c 7 (c (sha256 44 44) ())))) (sha256 44 ())))) (a (i (l 5) (q 11 (q . 2) (a 46 (c 2 (c 9 ()))) (a 46 (c 2 (c 13 ())))) (q 11 44 5)) 1) (c (c 40 (c 95 ())) (a 126 (c 2 (c (c (c 47 5) (c 95 383)) (c (a 122 (c 2 (c 11 (c 5 (q ()))))) (c 23 (c -65 (c 383 (c (sha256 1279 (a 54 (c 2 (c 9 (c 2815 (c (sha256 44 45) (c 21 ())))))) 5887) (c 1535 (c 3071 ()))))))))))) 2 42 (c 2 (c 95 (c 59 (c (a (i 23 (q 9 45 (sha256 39 (a 54 (c 2 (c 41 (c 87 (c (sha256 44 -71) (c 89 ())))))) -73)) ()) 1) (c 23 (c 5 (c 767 (c (c (c 36 (c (sha256 124 47 383) ())) (c (c 48 (c (sha256 -65 (sha256 124 21 (+ 383 (- 735 43) 767))) ())) 19)) ()))))))))) 1)) (c (q . 0x72dec062874cd4d3aab892a0906688a1ae412b0109982e1797a170add88bdcdc) (c (q . 0x625c2184e97576f5df1be46c15b2b8771c79e4e6f0aa42d3bfecaebe733f4b8c) (c (q 2 (q 2 (q 2 (i 11 (q 2 (i (= 5 (point_add 11 (pubkey_for_exp (sha256 11 (a 6 (c 2 (c 23 ()))))))) (q 2 23 47) (q 8)) 1) (q 4 (c 4 (c 5 (c (a 6 (c 2 (c 23 ()))) ()))) (a 23 47))) 1) (c (q 50 2 (i (l 5) (q 11 (q . 2) (a 6 (c 2 (c 9 ()))) (a 6 (c 2 (c 13 ())))) (q 11 (q . 1) 5)) 1) 1)) (c (q . 0x94a96f7397ff4acb08b6532fd20bb975a2c350c19216fef4ae9f64499bc59fe919bcf7b531dd80a371ad7858bfb288d2) 1)) 1))))"
    ).unwrap().to_program();
    let expected_output =  assemble_text(
        "(a (q 2 94 (c 2 (c (c 5 (c (sha256 44 5) (c 11 ()))) (c (a 23 47) (c 95 (c (a 46 (c 2 (c 23 ()))) (c (sha256 639 1407 2943) (c -65 (c 383 (c 767 (c 1535 (c 3071 ())))))))))))) (c (q (((-54 . 61) 70 2 . 51) (60 . 4) 1 1 . -53) ((a 2 (i 5 (q 2 50 (c 2 (c 13 (c (sha256 34 (sha256 44 52) (sha256 34 (sha256 34 (sha256 44 92) 9) (sha256 34 11 (sha256 44 ())))) ())))) (q . 11)) 1) (a (i 11 (q 2 (i (= (a 46 (c 2 (c 19 ()))) 2975) (q 2 38 (c 2 (c (a 19 (c 95 (c 23 (c 47 (c -65 (c 383 (c 27 ()))))))) (c 383 ())))) (q 8)) 1) (q 2 (i 23 (q 2 (i (not -65) (q . 383) (q 8)) 1) (q 8)) 1)) 1) (c (c 5 39) (c (+ 11 87) 119)) 2 (i 5 (q 2 (i (= (a (i (= 17 120) (q . 89) ()) 1) (q . -113)) (q 2 122 (c 2 (c 13 (c 11 (c (c -71 377) ()))))) (q 2 90 (c 2 (c (a (i (= 17 120) (q 4 120 (c (a 54 (c 2 (c 19 (c 41 (c (sha256 44 91) (c 43 ())))))) 57)) (q 2 (i (= 17 36) (q 4 36 (c (sha256 32 41) 57)) (q . 9)) 1)) 1) (c (a (i (= 17 120) (q . 89) ()) 1) (c (a 122 (c 2 (c 13 (c 11 (c 23 ()))))) ())))))) 1) (q 4 () (c () 23))) 1) ((a (i 5 (q 4 9 (a 38 (c 2 (c 13 (c 11 ()))))) (q . 11)) 1) 11 34 (sha256 44 88) (sha256 34 (sha256 34 (sha256 44 92) 5) (sha256 34 (a 50 (c 2 (c 7 (c (sha256 44 44) ())))) (sha256 44 ())))) (a (i (l 5) (q 11 (q . 2) (a 46 (c 2 (c 9 ()))) (a 46 (c 2 (c 13 ())))) (q 11 44 5)) 1) (c (c 40 (c 95 ())) (a 126 (c 2 (c (c (c 47 5) (c 95 383)) (c (a 122 (c 2 (c 11 (c 5 (q ()))))) (c 23 (c -65 (c 383 (c (sha256 1279 (a 54 (c 2 (c 9 (c 2815 (c (sha256 44 45) (c 21 ())))))) 5887) (c 1535 (c 3071 ()))))))))))) 2 42 (c 2 (c 95 (c 59 (c (a (i 23 (q 9 45 (sha256 39 (a 54 (c 2 (c 41 (c 87 (c (sha256 44 -71) (c 89 ())))))) -73)) ()) 1) (c 23 (c 5 (c 767 (c (c (c 36 (c (sha256 124 47 383) ())) (c (c 48 (c (sha256 -65 (sha256 124 21 (+ 383 (- 735 43) 767))) ())) 19)) ()))))))))) 1))"
    ).unwrap().to_program();
    let expected_args = Program::to(vec![
        Bytes32::from("0x72dec062874cd4d3aab892a0906688a1ae412b0109982e1797a170add88bdcdc").to_sexp(),
        Bytes32::from("0x625c2184e97576f5df1be46c15b2b8771c79e4e6f0aa42d3bfecaebe733f4b8c").to_sexp(),
        assemble_text("(a (q 2 (q 2 (i 11 (q 2 (i (= 5 (point_add 11 (pubkey_for_exp (sha256 11 (a 6 (c 2 (c 23 ()))))))) (q 2 23 47) (q 8)) 1) (q 4 (c 4 (c 5 (c (a 6 (c 2 (c 23 ()))) ()))) (a 23 47))) 1) (c (q 50 2 (i (l 5) (q 11 (q . 2) (a 6 (c 2 (c 9 ()))) (a 6 (c 2 (c 13 ())))) (q 11 (q . 1) 5)) 1) 1)) (c (q . 0x94a96f7397ff4acb08b6532fd20bb975a2c350c19216fef4ae9f64499bc59fe919bcf7b531dd80a371ad7858bfb288d2) 1))").unwrap().to_program().to_sexp(),
    ]);
    let (un_program, un_args) = program.uncurry().unwrap();
    assert_eq!(un_program, expected_output);
    assert_eq!(expected_args, un_args);
}

#[test]
pub fn test_curry_and_treehash() {
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::curry_and_treehash::{calculate_hash_of_quoted_mod_hash, curry_and_treehash};
    let arbitrary_mod = dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::MOD.clone();
    let arbitrary_mod_hash = arbitrary_mod.tree_hash();
    let quoted_mod_hash = calculate_hash_of_quoted_mod_hash(&arbitrary_mod_hash);
    for v in 0..500 {
        let args = vec![Program::to(v), Program::to(v * v), Program::to(v * v * v)];
        let puzzle = arbitrary_mod.curry(&args).unwrap();
        let puzzle_hash_via_curry = puzzle.tree_hash();
        let mut hashed_args = vec![];
        for arg in args {
            hashed_args.push(Program::to(arg).tree_hash());
        }
        let puzzle_hash_via_f = curry_and_treehash(&quoted_mod_hash, &hashed_args);
        assert_eq!(puzzle_hash_via_curry, puzzle_hash_via_f);
    }
}
