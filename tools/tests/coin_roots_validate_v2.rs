use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_dev_tools::coin_roots::validate::{
    BlockObservation, CoinMeta, Creation, RangeValidator, SpendRef, TimeLockConditions,
    ValidateError,
};
use sha2::{Digest, Sha256};

fn coin(tag: u64) -> Bytes32 {
    // A stand-in coin id; the core treats it as opaque self-authenticating bytes.
    let mut h = Sha256::new();
    h.update(b"validate_v2.test.coin");
    h.update(tag.to_le_bytes());
    let out: [u8; 32] = h.finalize().into();
    Bytes32::from(out)
}

/// A small hand-built scenario: a genesis-like range (empty seed) of three blocks.
///
/// - block 10 creates c1, c2, c3 (ts 1000)
/// - block 11 creates c4; spends c1 (relative-height ok: 11 >= 10 + 1) and c2
/// - block 12 spends c4 (seconds-relative ok: 1200 >= 1100 + 100)
///
/// Created {c1, c2, c3, c4}; spent {c1, c2, c4}; the sole survivor at boundary 12 is c3.
struct Scenario {
    blocks: Vec<BlockObservation>,
    survivors: Vec<Bytes32>,
    boundary: u32,
    header: Bytes32,
}

fn scenario() -> Scenario {
    let (c1, c2, c3, c4) = (coin(1), coin(2), coin(3), coin(4));
    let meta10 = CoinMeta {
        created_height: 10,
        created_timestamp: 1000,
    };
    let meta11 = CoinMeta {
        created_height: 11,
        created_timestamp: 1100,
    };

    let b10 = BlockObservation {
        height: 10,
        now_height: 10,
        now_timestamp: 1000,
        self_valid: true,
        creations: vec![
            Creation {
                coin_id: c1,
                meta: meta10,
            },
            Creation {
                coin_id: c2,
                meta: meta10,
            },
            Creation {
                coin_id: c3,
                meta: meta10,
            },
        ],
        spends: vec![],
    };
    let b11 = BlockObservation {
        height: 11,
        now_height: 11,
        now_timestamp: 1100,
        self_valid: true,
        creations: vec![Creation {
            coin_id: c4,
            meta: meta11,
        }],
        spends: vec![
            // c1 spent with ASSERT_HEIGHT_RELATIVE 1: 11 >= 10 + 1 -> ok.
            SpendRef {
                coin_id: c1,
                spent_height: 11,
                meta: meta10,
                time_locks: TimeLockConditions {
                    height_relative: Some(1),
                    ..Default::default()
                },
            },
            // c2 spent, no time locks.
            SpendRef {
                coin_id: c2,
                spent_height: 11,
                meta: meta10,
                time_locks: TimeLockConditions::default(),
            },
        ],
    };
    let b12 = BlockObservation {
        height: 12,
        now_height: 12,
        now_timestamp: 1200,
        self_valid: true,
        creations: vec![],
        spends: vec![
            // c4 spent with ASSERT_SECONDS_RELATIVE 100: 1200 >= 1100 + 100 -> ok.
            SpendRef {
                coin_id: c4,
                spent_height: 12,
                meta: meta11,
                time_locks: TimeLockConditions {
                    seconds_relative: Some(100),
                    ..Default::default()
                },
            },
        ],
    };
    // Created {c1,c2,c3,c4}; spent {c1,c2,c4}; survivor at boundary 12 = {c3}.
    Scenario {
        blocks: vec![b10, b11, b12],
        survivors: vec![c3],
        boundary: 12,
        header: coin(0xdead),
    }
}

fn run(blocks: &[&BlockObservation]) -> RangeValidator {
    let mut v = RangeValidator::new();
    for b in blocks {
        v.observe(b);
    }
    v
}

// Seeded Fisher-Yates over indices, no external RNG. LCG constants: Numerical Recipes.
fn permutation(n: usize, seed: u64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..n).collect();
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    for i in (1..n).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = (state >> 33) as usize % (i + 1);
        idx.swap(i, j);
    }
    idx
}

/// (b) THE property: forward and every shuffled order produce identical XOR residual, identical
/// phase-1 root, and reconcile identically.
#[test]
fn order_independence_xor_and_root() {
    let s = scenario();
    let forward: Vec<&BlockObservation> = s.blocks.iter().collect();
    let base = run(&forward);
    let base_xor = base.xor_digest();
    let base_root = base
        .reconcile(&s.survivors, s.boundary, s.header, expected_root(&s))
        .expect("forward order reconciles");

    for seed in 0..64u64 {
        let perm = permutation(s.blocks.len(), seed);
        let shuffled: Vec<&BlockObservation> = perm.iter().map(|&i| &s.blocks[i]).collect();
        let v = run(&shuffled);
        assert_eq!(
            v.xor_digest(),
            base_xor,
            "XOR residual must be order-independent (seed {seed})"
        );
        let root = v
            .reconcile(&s.survivors, s.boundary, s.header, expected_root(&s))
            .expect("shuffled order reconciles");
        assert_eq!(
            root.root_v1, base_root.root_v1,
            "phase-1 root must be order-independent (seed {seed})"
        );
    }
}

fn expected_root(s: &Scenario) -> Bytes32 {
    let forward: Vec<&BlockObservation> = s.blocks.iter().collect();
    run(&forward)
        .phase1_root(s.boundary, s.header)
        .expect("phase-1 root derivable")
        .root_v1
}

/// (c) RED-FIRST: a corrupted metadata fold breaks cancellation — the fold is load-bearing.
#[test]
fn corrupted_metadata_breaks_cancellation() {
    let mut s = scenario();
    // Corrupt the created-height the spender uses to remove c1 (block 11, spend 0): claim c1 was
    // born at height 9 instead of 10. The REMOVE leaf now differs from the ADD leaf.
    s.blocks[1].spends[0].meta.created_height = 9;
    let forward: Vec<&BlockObservation> = s.blocks.iter().collect();
    let v = run(&forward);
    let err = v
        .reconcile(&s.survivors, s.boundary, s.header, expected_clean_root())
        .expect_err("wrong metadata must fail closed");
    // XOR no longer cancels the coin, so either the XOR check or the root check fires — both are
    // fail-closed. (Assert it is not an accepted reconcile.)
    assert!(
        matches!(
            err,
            ValidateError::XorResidualMismatch | ValidateError::RootMismatch { .. }
        ),
        "expected fail-closed XOR/root mismatch, got {err:?}"
    );
}

/// A separate corruption that changes bucket-C VALIDITY, proving the condition check consumes
/// the same scalar: c1 spent with ASSERT_HEIGHT_RELATIVE 5 but claimed born at height 10 while
/// now=11 -> 11 < 10+5 -> condition fails at observe time.
#[test]
fn timelock_uses_metadata() {
    let mut s = scenario();
    s.blocks[1].spends[0].time_locks.height_relative = Some(5);
    let forward: Vec<&BlockObservation> = s.blocks.iter().collect();
    let v = run(&forward);
    let err = v
        .reconcile(&s.survivors, s.boundary, s.header, expected_clean_root())
        .expect_err("relative timelock must fail");
    assert!(
        matches!(err, ValidateError::TimeLock { .. }),
        "expected TimeLock, got {err:?}"
    );
}

/// Ephemeral coin (created and spent in the same block) with ASSERT_HEIGHT_RELATIVE 1 must fail:
/// age 0 < 1. created_height == now_height falls out of the fold automatically.
#[test]
fn ephemeral_relative_timelock_fails() {
    let e = coin(99);
    let meta = CoinMeta {
        created_height: 20,
        created_timestamp: 2000,
    };
    let block = BlockObservation {
        height: 20,
        now_height: 20,
        now_timestamp: 2000,
        self_valid: true,
        creations: vec![Creation { coin_id: e, meta }],
        spends: vec![SpendRef {
            coin_id: e,
            spent_height: 20,
            meta,
            time_locks: TimeLockConditions {
                height_relative: Some(1),
                ..Default::default()
            },
        }],
    };
    let v = run(&[&block]);
    let err = v
        .reconcile(&[], 20, coin(0xbeef), Bytes32::from([0u8; 32]))
        .expect_err("ephemeral age-0 relative-1 must fail");
    assert!(
        matches!(err, ValidateError::TimeLock { .. }),
        "expected TimeLock, got {err:?}"
    );
}

/// (d) A dropped creation breaks cancellation (a spend with no matching ADD).
#[test]
fn dropped_coin_breaks_cancellation() {
    let mut s = scenario();
    // Drop c3's creation from block 10 (still a survivor in the hints -> unmatched).
    s.blocks[0].creations.retain(|c| c.coin_id != coin(3));
    let forward: Vec<&BlockObservation> = s.blocks.iter().collect();
    let v = run(&forward);
    let err = v
        .reconcile(&s.survivors, s.boundary, s.header, expected_clean_root())
        .expect_err("dropped coin must fail closed");
    assert!(
        matches!(
            err,
            ValidateError::SpendOfMissingCoin { .. }
                | ValidateError::XorResidualMismatch
                | ValidateError::SurvivorHintMismatch { .. }
                | ValidateError::RootMismatch { .. }
        ),
        "expected fail-closed, got {err:?}"
    );
}

/// (d') An extra spend of a coin that was never created breaks cancellation.
#[test]
fn extra_spend_breaks_cancellation() {
    let mut s = scenario();
    let ghost = coin(0xffff);
    s.blocks[2].spends.push(SpendRef {
        coin_id: ghost,
        spent_height: 12,
        meta: CoinMeta {
            created_height: 5,
            created_timestamp: 500,
        },
        time_locks: TimeLockConditions::default(),
    });
    let forward: Vec<&BlockObservation> = s.blocks.iter().collect();
    let v = run(&forward);
    let err = v
        .reconcile(&s.survivors, s.boundary, s.header, expected_clean_root())
        .expect_err("extra spend must fail closed");
    assert!(
        matches!(
            err,
            ValidateError::SpendOfMissingCoin { .. } | ValidateError::XorResidualMismatch
        ),
        "expected fail-closed, got {err:?}"
    );
}

// The clean-scenario expected root, for the red tests (they must fail before/regardless of it).
fn expected_clean_root() -> Bytes32 {
    let s = scenario();
    expected_root(&s)
}
