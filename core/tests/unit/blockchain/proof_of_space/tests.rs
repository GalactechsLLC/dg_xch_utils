use super::*;

// The fixture keys behind the plot id vectors below.
fn plot_pk() -> Bytes48 {
    Bytes48::try_from(
        "96b35c22adf93068c9536e016e88251ad715a591d8deabb60917d9c495f45a220ca56b906793c27778d5f7f71fb50b94",
    )
    .expect("hex")
}
fn pool_pk() -> Bytes48 {
    Bytes48::try_from(
        "ac6e995e0f9c307853fa5c79e571de5ec2f2d45e5c2641c0847fef8041916e4d07d5a9200d5aa92ceac3b1bf41ce93b2",
    )
    .expect("hex")
}

fn make_pos(version: u8, has_pool_pk: bool, has_contract: bool, size: u8) -> ProofOfSpace {
    ProofOfSpace {
        challenge: Bytes32::default(),
        pool_public_key: has_pool_pk.then(pool_pk),
        pool_contract_puzzle_hash: has_contract.then(Bytes32::default),
        plot_public_key: plot_pk(),
        version,
        plot_index: 0,
        meta_group: 0,
        strength: 0,
        size,
        proof: ProofBytes::from(vec![0x80]),
    }
}

fn round_trip(pos: &ProofOfSpace) -> ProofOfSpace {
    let bytes = pos
        .to_bytes(ChiaProtocolVersion::default())
        .expect("serializes");
    let parsed =
        ProofOfSpace::from_bytes_exact(&bytes, ChiaProtocolVersion::default()).expect("parses");
    assert_eq!(
        parsed
            .to_bytes(ChiaProtocolVersion::default())
            .expect("re-serializes"),
        bytes,
        "re-serialization is not byte stable"
    );
    parsed
}

#[test]
fn a_v1_proof_serializes_exactly_as_the_field_order_always_did() {
    // A v1 proof carries the plain per-field encoding; those bytes are hashed into every block
    // and protocol message.
    for (has_pool_pk, has_contract) in [(true, false), (false, true), (true, true), (false, false)]
    {
        let pos = make_pos(0, has_pool_pk, has_contract, 32);
        let v = ChiaProtocolVersion::default();
        let mut expected = pos.challenge.to_bytes(v).expect("field");
        expected.extend(pos.pool_public_key.to_bytes(v).expect("field"));
        expected.extend(pos.pool_contract_puzzle_hash.to_bytes(v).expect("field"));
        expected.extend(pos.plot_public_key.to_bytes(v).expect("field"));
        expected.extend(pos.size.to_bytes(v).expect("field"));
        expected.extend(pos.proof.to_bytes(v).expect("field"));
        assert_eq!(pos.to_bytes(v).expect("proof"), expected);
        let parsed = round_trip(&pos);
        assert_eq!(parsed, pos);
    }
}

#[test]
fn a_v2_proof_round_trips_and_drops_size() {
    for (has_pool_pk, has_contract) in [(true, false), (false, true)] {
        let mut pos = make_pos(1, has_pool_pk, has_contract, 0);
        pos.plot_index = 256;
        pos.meta_group = 7;
        pos.strength = 10;
        let parsed = round_trip(&pos);
        assert_eq!(parsed, pos);
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.size, 0);
    }
}

#[test]
fn the_version_lives_in_the_contract_prefix_byte() {
    let v = ChiaProtocolVersion::default();
    // The prefix sits after the challenge and the pool key Option, so its offset moves with
    // that Option's presence.
    let offset = |has_pool_pk: bool| 32 + 1 + if has_pool_pk { 48 } else { 0 };
    let absent = make_pos(0, true, false, 32).to_bytes(v).expect("proof");
    let present = make_pos(0, true, true, 32).to_bytes(v).expect("proof");
    assert_eq!(
        absent
            .iter()
            .zip(present.iter())
            .position(|(a, b)| a != b)
            .expect("prefix byte differs"),
        offset(true)
    );
    assert_eq!(absent[offset(true)], 0b00);
    assert_eq!(present[offset(true)], 0b01);
    assert_eq!(
        make_pos(1, true, false, 0).to_bytes(v).expect("proof")[offset(true)],
        0b10
    );
    assert_eq!(
        make_pos(1, false, true, 0).to_bytes(v).expect("proof")[offset(false)],
        0b11
    );

    // Any higher bit is malformed for both versions.
    for version in [0u8, 1] {
        for bit in 2..8 {
            let size = if version == 0 { 32 } else { 0 };
            let mut buf = make_pos(version, true, false, size)
                .to_bytes(v)
                .expect("proof");
            buf[offset(true)] |= 1 << bit;
            assert!(
                ProofOfSpace::from_bytes_exact(&buf, v).is_err(),
                "prefix bit {bit} accepted on version {version}"
            );
        }
    }
}

#[test]
fn a_v2_proof_needs_exactly_one_pool_binding() {
    let v = ChiaProtocolVersion::default();
    // Both set and neither set are rejected at parse; the v1 path stays lenient.
    let both = make_pos(1, true, true, 0).to_bytes(v).expect("proof");
    assert!(ProofOfSpace::from_bytes_exact(&both, v).is_err());
    let neither = make_pos(1, false, false, 0).to_bytes(v).expect("proof");
    assert!(ProofOfSpace::from_bytes_exact(&neither, v).is_err());
    assert!(
        make_pos(2, true, false, 0).to_bytes(v).is_err(),
        "unknown version serialized"
    );
}

#[test]
fn v2_plot_group_ids_match_the_reference_vectors() {
    for (strength, pool, contract, expected) in [
        (
            0u8,
            Some(pool_pk()),
            None,
            "5457cccc4cd79900da4235cf5ca7d978a1993581376e76dfb089c274225419d1",
        ),
        (
            10,
            Some(pool_pk()),
            None,
            "e9d517de0ccfa94baf9e94b39dd0e8afce0451ec27635f43f2aa9b2f429d0501",
        ),
        (
            0,
            None,
            Some(Bytes32::from([1u8; 32])),
            "210d1a307d26acb3fcfa02208061fc6b80e3fbb9ca5f3e4a596b7521d87ccd79",
        ),
        (
            5,
            None,
            Some(Bytes32::from([1u8; 32])),
            "824d7b67ab4269c91eb0a2fe10cb48a1c1ad8cfa8a642387d49d5c3c3acbc3bd",
        ),
    ] {
        assert_eq!(
            calculate_plot_group_id_v2(strength, plot_pk(), pool, contract),
            Bytes32::try_from(expected).expect("hex"),
            "strength {strength}"
        );
    }
}

#[test]
fn v2_plot_ids_match_the_reference_vectors() {
    for (strength, plot_index, meta_group, pool, contract, expected) in [
        (
            0u8,
            0u16,
            0u8,
            Some(pool_pk()),
            None,
            "d3692a5d4fbfe1061053d4afada80d8f0b58b87b46c170e7087716a72091def0",
        ),
        (
            10,
            256,
            7,
            Some(pool_pk()),
            None,
            "2316eadc21d38c4e8740eb9efd49a0c2014a5b1ef992f5ae0b2d1fda01a4b034",
        ),
        (
            0,
            0,
            0,
            None,
            Some(Bytes32::from([1u8; 32])),
            "03b09cab4bfdbcd1e626d93888a72f002d3948459c23cde52e9dd8d72dd9ae04",
        ),
        (
            5,
            100,
            3,
            None,
            Some(Bytes32::from([1u8; 32])),
            "d575860c249ace41a656fe0d97719127f839fae55e6c32ffd7743b5a8a2eae4d",
        ),
    ] {
        assert_eq!(
            calculate_plot_id_v2(strength, plot_pk(), pool, contract, plot_index, meta_group),
            Bytes32::try_from(expected).expect("hex"),
            "strength {strength} index {plot_index}"
        );
    }
}

#[test]
fn v1_is_never_phased_out_before_hard_fork_two() {
    use crate::consensus::constants::MAINNET;
    // MAINNET's HARD_FORK2_HEIGHT is a never-activate sentinel, so no real height triggers it.
    assert!(!is_v1_phased_out(&[0u8; 64], 0, &MAINNET));
    assert!(!is_v1_phased_out(&[0u8; 64], 10_000_000, &MAINNET));
}

#[test]
fn v1_phase_out_retires_more_plots_as_the_cutoff_approaches() {
    use crate::consensus::constants::MAINNET;
    use crate::consensus::overrides::{ConsensusOverrides, apply_overrides};
    // Bring hard fork 2 to a real height with a small epoch so the window is walkable.
    let c = apply_overrides(
        MAINNET,
        &ConsensusOverrides {
            hard_fork2_height: Some(1_000),
            epoch_blocks: Some(100),
            plot_v1_phase_out_epoch_bits: Some(3),
            ..Default::default()
        },
    );
    // 7 epochs wide (mask 7). At the fork height none are retired yet; past the cut-off all are.
    let cut_off = v1_cut_off_height(&c) as u32;
    let retired = |h: u32| {
        (0u16..=255)
            .filter(|i| {
                let mut proof = vec![0u8; 64];
                proof[0] = *i as u8;
                is_v1_phased_out(&proof, h, &c)
            })
            .count()
    };
    let at_fork = retired(1_000);
    let midway = retired((1_000 + cut_off) / 2);
    assert!(
        at_fork < midway,
        "retirement did not grow: {at_fork} then {midway}"
    );
    assert_eq!(
        retired(cut_off + 1),
        256,
        "all v1 plots must be gone past the cut-off"
    );
}

#[test]
fn the_v2_plot_filter_follows_its_own_schedule() {
    use crate::consensus::constants::MAINNET;
    use crate::consensus::overrides::{ConsensusOverrides, apply_overrides};
    let c = apply_overrides(
        MAINNET,
        &ConsensusOverrides {
            plot_filter_v2_first_adjustment_height: Some(1_000),
            plot_filter_v2_second_adjustment_height: Some(2_000),
            plot_filter_v2_third_adjustment_height: Some(3_000),
            ..Default::default()
        },
    );
    assert_eq!(calculate_prefix_bits_v2(&c, 0), 5);
    assert_eq!(calculate_prefix_bits_v2(&c, 999), 5);
    assert_eq!(calculate_prefix_bits_v2(&c, 1_000), 4);
    assert_eq!(calculate_prefix_bits_v2(&c, 2_000), 3);
    assert_eq!(calculate_prefix_bits_v2(&c, 3_000), 2);
    // On stock mainnet constants the schedule has not activated at any real height.
    assert_eq!(calculate_prefix_bits_v2(&MAINNET, 10_000_000), 5);
    // The v1 filter is untouched by the v2 fields.
    assert_eq!(calculate_prefix_bits(&MAINNET, 0), 9);
}

#[test]
fn get_plot_id_routes_by_version() {
    let v2 = make_pos(1, true, false, 0);
    assert_eq!(
        v2.get_plot_id().expect("valid binding"),
        calculate_plot_id_v2(0, plot_pk(), Some(pool_pk()), None, 0, 0)
    );
    let v1 = make_pos(0, true, false, 32);
    assert_eq!(
        v1.get_plot_id().expect("valid binding"),
        calculate_plot_id_public_key(pool_pk(), plot_pk())
    );
    assert!(make_pos(1, true, true, 0).get_plot_id().is_none());
    assert!(make_pos(1, false, false, 0).get_plot_id().is_none());
}
