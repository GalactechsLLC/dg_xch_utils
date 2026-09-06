use super::*;

fn core(k: u8, strength: u8) -> ProofCore {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    ProofCore::new(ProofParams::new(Bytes32::from(bytes), k, strength, false).expect("params"))
        .expect("core")
}

#[test]
fn matching_sections_are_a_permutation() {
    for k in [28u8, 30, 32] {
        let c = core(k, 2);
        let count = c.params().num_sections();
        let mut seen = std::collections::HashSet::new();
        for section in 0..count {
            let matched = c.matching_section(section);
            assert!(matched < count, "section {matched} out of range");
            assert!(seen.insert(matched), "matching_section is not injective");
        }
        assert_eq!(seen.len() as u32, count);
    }
}

#[test]
fn the_inverse_undoes_the_matching_section() {
    for k in [28u8, 30, 32] {
        let c = core(k, 2);
        for section in 0..c.params().num_sections() {
            assert_eq!(
                c.inverse_matching_section(c.matching_section(section)),
                section,
                "k{k} section {section}"
            );
        }
    }
}

#[test]
fn a_section_never_matches_itself() {
    let c = core(28, 2);
    for section in 0..c.params().num_sections() {
        assert_ne!(c.matching_section(section), section);
    }
}

#[test]
fn pairings_survive_only_when_their_filter_bits_are_zero() {
    // Two filter bits, so roughly a quarter of pairs survive.
    let c = core(28, 2);
    let mut survived = 0;
    let total = 4000u32;
    for i in 0..total {
        if c.pairing_t1(i, i.wrapping_mul(7919) & 0x0FFF_FFFF)
            .is_some()
        {
            survived += 1;
        }
    }
    let rate = f64::from(survived) / f64::from(total);
    assert!(
        (0.15..0.35).contains(&rate),
        "table 1 survival rate was {rate}"
    );
}

#[test]
fn a_surviving_table_one_pairing_carries_both_x_values() {
    let c = core(28, 2);
    let k = u32::from(c.params().k());
    for i in 0..5000u32 {
        let (x_l, x_r) = (i, i.wrapping_mul(7919) & 0x0FFF_FFFF);
        if let Some(p) = c.pairing_t1(x_l, x_r) {
            assert_eq!((p.meta >> k) as u32, x_l);
            assert_eq!((p.meta & ((1u64 << k) - 1)) as u32, x_r);
            return;
        }
    }
    panic!("no table 1 pairing survived");
}

#[test]
fn a_challenge_opens_four_mutually_exclusive_sets() {
    let c = core(28, 2);
    let sets = c.select_challenge_sets(Bytes32::from([7u8; 32]));
    for (i, index) in sets.indexes.iter().enumerate() {
        assert_eq!(
            *index as usize % NUM_CHALLENGE_SETS,
            i,
            "set {i} landed in the wrong residue"
        );
        assert!(*index < c.params().num_chaining_sets());
    }
    // Ranges are disjoint and follow their index.
    for (index, range) in sets.indexes.iter().zip(sets.ranges.iter()) {
        assert_eq!(*range, c.params().chaining_set_range(u64::from(*index)));
    }
    assert_ne!(sets, c.select_challenge_sets(Bytes32::from([8u8; 32])));
}
