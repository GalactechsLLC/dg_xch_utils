use dg_xch_core::clvm::assemble::assemble_text;
use dg_xch_core::clvm::compile::{COMPAT_CHIA, Compiler, OPT_REFERENCE};
use dg_xch_core::clvm::runtime::ClvmRuntime;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::clvm::utils::{CANONICAL_INTS, ENABLE_KECCAK_OPS_OUTSIDE_FORK, MEMPOOL_MODE};
use dg_xch_core::errors::ClvmError;
use std::borrow::Cow;

fn run(source: &str, flags: u32, limit: u64) -> Result<(u64, SExp<'static>), ClvmError> {
    let program = assemble_text(source).unwrap();
    ClvmRuntime::new(limit, flags).run(program.sexp(), &SExp::default())
}

#[test]
fn division_floors_in_both_modes_and_compiler_constant_folding() {
    for flags in [0, MEMPOOL_MODE] {
        for (a, b, expected) in [
            (-1, 2, -1),
            (1, -2, -1),
            (-3, 2, -2),
            (3, -2, -2),
            (-4, 2, -2),
            (-3, -2, 1),
        ] {
            let (_, result) = run(&format!("(/ (q . {a}) (q . {b}))"), flags, 10_000).unwrap();
            assert_eq!(result, SExp::from(expected));
        }
        assert!(run("(/ (q . 1) (q . 0))", flags, 10_000).is_err());
    }
    for optimizations in [0, OPT_REFERENCE] {
        let source = "(mod () (/ -1 2))";
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            optimizations,
            &[],
        );
        let program = compiler.compile().unwrap();
        let (_, result) = ClvmRuntime::new(10_000, MEMPOOL_MODE)
            .run(program.sexp(), &SExp::default())
            .unwrap();
        assert_eq!(result, SExp::from(-1));
    }
}

#[test]
fn softfork_executes_checks_cost_and_discards_result() {
    for flags in [0, MEMPOOL_MODE] {
        let source = "(softfork (q . 160) (q . 0) (q . (q . 42)) (q . ()))";
        assert_eq!(run(source, flags, 241).unwrap(), (241, SExp::default()));
        assert!(run(source, flags, 240).is_err());
        for cost in [1, 159, 161, 1000] {
            assert!(run(&source.replace("160", &cost.to_string()), flags, 10_000).is_err());
        }
        assert!(matches!(
            run(
                "(softfork (q . 1000) (q . 0) (q . (x)) (q . ()))",
                flags,
                10_000
            ),
            Err(ClvmError::Raise(_))
        ));
        // The program receives the supplied environment; the result is still discarded.
        assert_eq!(
            run(
                "(softfork (q . 184) (q . 0) (q . 1) (q . (42)))",
                flags,
                10_000
            )
            .unwrap(),
            (265, SExp::default())
        );
        let nested = "(softfork (q . 381) (q . 0) (q . (softfork (q . 160) (q . 0) (q . (q . 42)) (q . ()))) (q . ()))";
        assert_eq!(run(nested, flags, 10_000).unwrap(), (462, SExp::default()));
        // Both enclosing and sibling values survive guard allocation reclamation.
        let source = format!("(c (concat (q . 0xab) (q . 0xcd)) (c {source} (q . (99))))");
        assert_eq!(
            run(&source, flags, 10_000).unwrap().1,
            assemble_text("(0xabcd () 99)").unwrap().sexp().clone()
        );
    }
}

#[test]
fn softfork_unknown_extensions_and_canonical_integers() {
    for source in [
        "(softfork (q . 1000))",
        "(softfork (q . 1000) (q . 9) (q . (x)) (q . ()))",
        "(softfork (q . 1000) (q . -1) (q . (x)) (q . ()))",
    ] {
        assert!(run(source, 0, 10_000).is_ok());
        assert!(run(source, MEMPOOL_MODE, 10_000).is_err());
    }
    for source in [
        "(softfork (q . 0x0000a0) (q . 0) (q . (q . 42)) (q . ()))",
        "(softfork (q . 160) (q . 0x00) (q . (q . 42)) (q . ()))",
    ] {
        assert!(run(source, MEMPOOL_MODE, 10_000).is_ok());
        assert!(run(source, MEMPOOL_MODE | CANONICAL_INTS, 10_000).is_err());
    }
    let canonical = "(softfork (q . 0x00a0) (q . 0) (q . (q . 42)) (q . ()))";
    assert!(run(canonical, MEMPOOL_MODE | CANONICAL_INTS, 10_000).is_ok());
}

#[test]
fn keccak_extension_is_scoped_to_innermost_guard() {
    assert_eq!(
        run(
            "(softfork (q . 511) (q . 1) (q . (62)) (q . ()))",
            MEMPOOL_MODE,
            10_000
        )
        .unwrap(),
        (592, SExp::default())
    );
    assert!(
        run(
            "(softfork (q . 511) (q . 0) (q . (62)) (q . ()))",
            MEMPOOL_MODE,
            10_000
        )
        .is_err()
    );
    assert!(run("(62)", MEMPOOL_MODE, 10_000).is_err());
    assert!(run("(softfork (q . 1000) (q . 1) (q . (softfork (q . 511) (q . 0) (q . (62)) (q . ()))) (q . ()))", MEMPOOL_MODE, 10_000).is_err());
    let (cost, digest) = run(
        "(62)",
        MEMPOOL_MODE | ENABLE_KECCAK_OPS_OUTSIDE_FORK,
        10_000,
    )
    .unwrap();
    assert_eq!(cost, 371);
    assert_eq!(
        digest,
        assemble_text("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
            .unwrap()
            .sexp()
            .clone()
    );
    // Evaluate guard first, then an opcode outside it: extension must not leak.
    assert!(
        run(
            "(c (62) (softfork (q . 511) (q . 1) (q . (62)) (q . ())))",
            MEMPOOL_MODE,
            10_000
        )
        .is_err()
    );
}

#[test]
fn runtime_can_be_reused_after_guard_failure() {
    let mut runtime = ClvmRuntime::new(10_000, MEMPOOL_MODE);
    let bad = assemble_text("(softfork (q . 1000) (q . 0) (q . (x)) (q . ()))").unwrap();
    assert!(runtime.run(bad.sexp(), &SExp::default()).is_err());
    let good = assemble_text("(q . 42)").unwrap();
    assert_eq!(
        runtime.run(good.sexp(), &SExp::default()).unwrap(),
        (20, SExp::from(42))
    );
}

// Fixed verification vectors from clvmr 0.16.2 op-tests/test-secp256{k,r}1.txt.
const SECP_VECTORS: &[(&str, &str, &str, &str, &str, u64)] = &[
    (
        "secp256k1_verify",
        "13d61f00",
        "0x02390b19842e100324163334b16947f66125b76d4fa4a11b9ccdde9b7398e64076",
        "0x85932e4d075615be881398cc765f9f78204033f0ef5f832ac37e732f5f0cbda2",
        "0x481477e62a1d02268127ae89cc58929e09ad5d30229721965ae35965d098a5f630205a7e69f4cb8084f16c7407ed7312994ffbf87ba5eb1aee16682dd324943e",
        1300000,
    ),
    (
        "secp256r1_verify",
        "1c3a8f00",
        "0x033e1a1b2ccbc35883c60fdfc3f4a02175096ade6271fe85517ca5772594bbd0dc",
        "0x85932e4d075615be881398cc765f9f78204033f0ef5f832ac37e732f5f0cbda2",
        "0xeae2f488080919bd0a7069c24cdd9c6ce2db423861b0c9d4236cdadbd0005f6d8f3709e6eb19249fd9c8bea664aba35218e67ea4b0f2239488dc3147f336e1e6",
        1850000,
    ),
];

#[test]
fn secp_verifies_valid_signatures_rejects_invalid_and_enforces_cost() {
    for &(name, opcode, key, digest, signature, cost) in SECP_VECTORS {
        for flags in [0, MEMPOOL_MODE] {
            let source = format!("(0x{opcode} (q . {key}) (q . {digest}) (q . {signature}))");
            assert_eq!(
                run(&source, flags, cost + 61).unwrap(),
                (cost + 61, SExp::default())
            );
            assert!(run(&source, flags, cost + 60).is_err());
            for (key, digest, signature) in [
                (
                    key,
                    "0x0000000000000000000000000000000000000000000000000000000000000000",
                    signature,
                ),
                ("()", digest, signature),
                (key, "0x01", signature),
                (key, digest, "()"),
                ("(1 2)", digest, signature),
            ] {
                let source = format!("(0x{opcode} (q . {key}) (q . {digest}) (q . {signature}))");
                assert!(
                    run(&source, flags, 10_000_000).is_err(),
                    "{name} accepted {source}"
                );
            }
            for args in [
                "",
                "(q . ()) (q . ())",
                "(q . ()) (q . ()) (q . ()) (q . ())",
            ] {
                assert!(run(&format!("(0x{opcode} {args})"), flags, 10_000_000).is_err());
            }
        }
        let source = format!("(mod (KEY MSG SIG) ({name} KEY MSG SIG))");
        let compiler = Compiler::new(
            Cow::Borrowed(source.as_bytes()),
            COMPAT_CHIA,
            OPT_REFERENCE,
            &[],
        );
        let program = compiler.compile().unwrap();
        let args = assemble_text(&format!("({key} {digest} {signature})")).unwrap();
        assert_eq!(
            ClvmRuntime::new(10_000_000, MEMPOOL_MODE)
                .run(program.sexp(), args.sexp())
                .unwrap()
                .1,
            SExp::default()
        );
    }
}

#[cfg(feature = "bls")]
#[test]
fn bls_guard_executes_pairing_check() {
    let source = "(softfork (q . 3000141) (q . 0) (q . (58)) (q . ()))";
    assert_eq!(
        run(source, MEMPOOL_MODE, 10_000_000).unwrap(),
        (3_000_222, SExp::default())
    );
    assert!(
        run(
            "(softfork (q . 3000141) (q . 0) (q . (58 (q . 1))) (q . ()))",
            MEMPOOL_MODE,
            10_000_000
        )
        .is_err()
    );
}

#[cfg(feature = "falcon")]
#[test]
fn falcon_operator_is_profile_gated_and_checks_signature_and_cost() {
    use dg_xch_core::clvm::falcon_ops::{ENABLE_FALCON_OPS, FALCON_VERIFY_COST};
    use dg_xch_core::signatures::{SignatureSigner, falcon::FalconSigner};
    let signer = FalconSigner::generate().unwrap();
    let message = [42u8; 32];
    let signature = signer.sign_message(&message).unwrap();
    let key = hex::encode(signer.public_key());
    let digest = hex::encode(message);
    let signature = hex::encode(signature);
    let source = format!("(0x4447580101 (q . 0x{key}) (q . 0x{digest}) (q . 0x{signature}))");
    for flags in [ENABLE_FALCON_OPS, ENABLE_FALCON_OPS | MEMPOOL_MODE] {
        assert_eq!(
            run(&source, flags, FALCON_VERIFY_COST + 61).unwrap(),
            (FALCON_VERIFY_COST + 61, SExp::default())
        );
        assert!(run(&source, flags, FALCON_VERIFY_COST + 60).is_err());
        assert!(
            run(
                &source.replace(&digest, &hex::encode([43; 32])),
                flags,
                10_000_000
            )
            .is_err()
        );
        for invalid in [
            source.replace(&key, "01"),
            source.replace(&signature, "01"),
            source.replace(&digest, "01"),
            "(0x4447580101)".into(),
        ] {
            assert!(run(&invalid, flags, 10_000_000).is_err());
        }
    }
    let compiler = Compiler::new(
        Cow::Borrowed(b"(mod (KEY MSG SIG) (falcon_verify KEY MSG SIG))"),
        COMPAT_CHIA,
        OPT_REFERENCE,
        &[],
    );
    let compiled = compiler.compile().unwrap();
    let args = SExp::from(vec![
        SExp::from(signer.public_key().to_vec()),
        SExp::from(message.to_vec()),
        SExp::from(hex::decode(signature).unwrap()),
    ]);
    assert_eq!(
        ClvmRuntime::new(10_000_000, ENABLE_FALCON_OPS)
            .run(compiled.sexp(), &args)
            .unwrap()
            .1,
        SExp::default()
    );
    for flags in [0, MEMPOOL_MODE] {
        assert!(run(&source, flags, 10_000_000).is_err());
    }
}

#[cfg(feature = "falcon")]
#[test]
fn falcon_key_derivation_separates_roles_indices_and_hardened_paths() {
    use dg_xch_core::signatures::{
        SignatureScheme, SignatureSigner,
        falcon::{FalconSigner, FnDsa512V2},
    };
    let key = FalconSigner::from_seed(&[42; 64], 0, 0, false).unwrap();
    assert_eq!(
        hex::encode(dg_xch_core::utils::hash_256(key.public_key())),
        "2412946131d43cf0f086f6a86f5183c8ab11ebf8460a8033d70b5ab90449253e"
    );
    assert_eq!(
        key.public_key(),
        FalconSigner::from_seed(&[42; 64], 0, 0, false)
            .unwrap()
            .public_key()
    );
    for (seed, role, index, hardened) in [
        ([43; 64], 0, 0, false),
        ([42; 64], 1, 0, false),
        ([42; 64], 0, 1, false),
        ([42; 64], 0, 0, true),
    ] {
        assert_ne!(
            key.public_key(),
            FalconSigner::from_seed(&seed, role, index, hardened)
                .unwrap()
                .public_key()
        );
    }
    let signature = key.sign_message(b"recovered identity").unwrap();
    assert!(FnDsa512V2.verify(&key.public_key(), b"recovered identity", &signature));
}
