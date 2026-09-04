use super::*;

#[test]
fn rejects_out_of_order_segments() {
    let segment = |sub_epoch_n| SubEpochChallengeSegment {
        sub_epoch_n,
        sub_slots: vec![],
        rc_slot_end_info: None,
    };
    assert!(matches!(
        validate_segment_order(&[segment(1), segment(0)]),
        Err(WeightProofError::Malformed(_))
    ));
}

/// Real-prod-data phase-2 accept path: a live-fetched MAINNET weight proof (tip height 9,054,698,
/// weight 55,606,644,880). Reconstruct the summary chain from the proof's `SubEpochData` and prove
/// its last summary's `ses_hash` equals the sub-epoch-summary hash actually committed on-chain (read
/// from the recent chain's finished sub-slots). This is ground truth from mainnet itself — it only
/// passes with the corrected 6-field `SubEpochSummary` (the missing `challenge_merkle_root` shifted
/// the hash by one trailing byte). Independent of the reference's golden; both must agree.
#[test]
fn phase2_anchor_matches_on_real_mainnet_proof() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("real mainnet fixture at {}: {e}", path.display()));
    let mut cur = std::io::Cursor::new(bytes.as_slice());
    let wp = WeightProof::from_bytes(&mut cur, ChiaProtocolVersion::default())
        .expect("real mainnet weight proof deserializes");
    assert_eq!(wp.sub_epochs.len(), 23_579, "fixture sub-epoch count");

    let c = &dg_xch_core::consensus::constants::MAINNET;
    let (last_ses_hash, _height) = get_last_ses_hash(c.sub_epoch_blocks, &wp.recent_chain_data)
        .expect("on-chain sub-epoch-summary hash present in recent chain");
    let (summaries, _total, _weights) = map_sub_epoch_summaries(
        c.sub_epoch_blocks,
        c.genesis_challenge,
        &wp.sub_epochs,
        c.difficulty_starting,
    )
    .expect("reconstruct summaries");
    assert_eq!(summaries.len(), wp.sub_epochs.len());

    let last = summaries.last().expect("summaries non-empty");
    assert_eq!(
        ses_hash(last).expect("hash last summary"),
        last_ses_hash,
        "reconstructed last-summary hash must equal mainnet's on-chain sub-epoch-summary commitment"
    );

    // Full phase-2 entry point accepts the real proof and returns summaries + total + weight list.
    let (out, _total, weights) =
        validate_sub_epoch_summaries(&wp, c).expect("phase 2 accepts the real mainnet proof");
    assert_eq!(out.len(), 23_579);
    // The weight list has one entry per sub-epoch: (n-1) accrued in-loop + 1 trailing close = n.
    assert_eq!(weights.len(), out.len());
}

#[test]
fn mt19937_getrandbits_and_randbelow_match_cpython() {
    // getrandbits sequence on one rng (seed 00..1f), then choice(range(n)) on fresh rng each.
    let seed: Vec<u8> = (0u8..32).collect();
    let mut r = py_random::PyRandom::new(&seed);
    let got: Vec<u64> = [1u32, 4, 8, 15, 32, 33, 64]
        .iter()
        .map(|&k| r.getrandbits(k))
        .collect();
    assert_eq!(
        got,
        vec![
            1,
            5,
            231,
            1175,
            2_810_937_230,
            6_094_649_597,
            11_238_162_993_324_450_277
        ]
    );
    for (n, want) in [
        (1u64, 0u64),
        (2, 1),
        (3, 1),
        (5, 2),
        (10, 5),
        (100, 40),
        (236, 81),
    ] {
        let mut rr = py_random::PyRandom::new(&seed);
        assert_eq!(rr.randbelow(n), want, "choice(range({n}))");
    }
}

#[test]
fn mt19937_matches_cpython_random_vectors() {
    let seed: Vec<u8> = (0u8..32).collect();
    let mut r = py_random::PyRandom::new(&seed);
    let want: [f64; 5] = [
        0.9592884430034848,
        0.904383003978874,
        0.6544723243938474,
        0.5561377199205005,
        0.6092220395645366,
    ];
    for w in want {
        assert_eq!(
            r.random().to_bits(),
            w.to_bits(),
            "random() must be bit-exact vs CPython"
        );
    }

    let seed2: Vec<u8> = vec![0u8, 0u8].into_iter().chain(0u8..30).collect();
    let mut r2 = py_random::PyRandom::new(&seed2);
    let want2: [f64; 3] = [0.48331334892446376, 0.18455929743918742, 0.01555251624192];
    for w in want2 {
        assert_eq!(r2.random().to_bits(), w.to_bits());
    }
}

#[test]
fn phase1_accepts_on_real_mainnet_proof() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("real mainnet fixture: {e}"));
    let wp = WeightProof::from_bytes(
        &mut std::io::Cursor::new(bytes.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("deserialize real mainnet weight proof");
    let c = &dg_xch_core::consensus::constants::MAINNET;
    let (summaries, _total, weights) = validate_sub_epoch_summaries(&wp, c).expect("phase 2");
    validate_sub_epoch_sampling(&wp, &summaries, &weights, c)
        .expect("phase 1 accepts the real mainnet proof (RNG sample set is covered by segments)");
}

#[test]
fn phase3_matches_weight_on_real_mainnet_proof() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/weight_proof_mainnet_9054698.bin");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("real mainnet fixture: {e}"));
    let wp = WeightProof::from_bytes(
        &mut std::io::Cursor::new(bytes.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("deserialize real mainnet weight proof");
    let c = &dg_xch_core::consensus::constants::MAINNET;
    let (summaries, total, _weights) = validate_sub_epoch_summaries(&wp, c).expect("phase 2");

    validate_summaries_weight(&wp, &summaries, total, c)
        .expect("phase 3 accepts: summaries weight equals the recent chain's boundary weight");

    // A one-unit perturbation of the accumulated weight must be rejected.
    assert!(matches!(
        validate_summaries_weight(&wp, &summaries, total + 1, c),
        Err(WeightProofError::Rejected(_))
    ));
}

#[test]
fn an_incomplete_validator_fails_closed_never_accepts() {
    let wp = WeightProof {
        sub_epochs: vec![],
        sub_epoch_segments: vec![],
        recent_chain_data: vec![],
    };
    // empty → Malformed (never Ok(true))
    assert!(matches!(
        validate_weight_proof(&wp, &dg_xch_core::consensus::constants::MAINNET),
        Err(WeightProofError::Malformed(_))
    ));
}
