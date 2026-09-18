use super::*;

fn params(k: u8, strength: u8) -> ProofParams {
    ProofParams::new(Bytes32::from([1u8; 32]), k, strength, false).expect("valid params")
}

#[test]
fn section_bits_follow_k() {
    assert_eq!(params(28, 2).num_section_bits(), 2);
    assert_eq!(params(28, 2).num_sections(), 4);
    assert_eq!(params(30, 2).num_section_bits(), 4);
    assert_eq!(params(32, 2).num_section_bits(), 6);
    assert_eq!(params(32, 2).num_sections(), 64);
    // Below k28 the width is pinned rather than going negative.
    assert_eq!(params(20, 2).num_section_bits(), 2);
}

#[test]
fn match_key_bits_are_two_for_table_one_and_strength_after() {
    let p = params(28, 5);
    assert_eq!(p.num_match_key_bits(1), 2);
    assert_eq!(p.num_match_key_bits(2), 5);
    assert_eq!(p.num_match_key_bits(3), 5);
}

#[test]
fn the_three_match_info_fields_fill_exactly_k_bits() {
    for k in [28u8, 30, 32] {
        for strength in [2u8, 5, 16] {
            let p = params(k, strength);
            for table in 1..=3 {
                assert_eq!(
                    p.num_section_bits()
                        + p.num_match_key_bits(table)
                        + p.num_match_target_bits(table),
                    u32::from(k),
                    "k {k} strength {strength} table {table}"
                );
            }
        }
    }
}

#[test]
fn a_match_info_round_trips_through_its_three_extractors() {
    let p = params(28, 5);
    for table in 1..=3usize {
        let section = p.num_sections() - 1;
        let key = (1u32 << p.num_match_key_bits(table)) - 2;
        let target = (1u32 << p.num_match_target_bits(table)) - 3;
        let match_info = (section << (28 - p.num_section_bits()))
            | (key << p.num_match_target_bits(table))
            | target;
        assert_eq!(p.extract_section(match_info), section, "table {table}");
        assert_eq!(p.extract_match_key(table, match_info), key, "table {table}");
        assert_eq!(
            p.extract_match_target(table, u64::from(match_info)),
            target,
            "table {table}"
        );
    }
}

#[test]
fn meta_widens_after_table_one() {
    let p = params(28, 2);
    assert_eq!(p.num_meta_bits(1), 28);
    assert_eq!(p.num_meta_bits(2), 56);
    assert_eq!(p.num_pairing_meta_bits(), 56);
}

#[test]
fn chaining_sets_tile_the_fragment_space() {
    let p = params(28, 2);
    assert_eq!(p.chaining_set_size(), 64);
    assert_eq!(p.num_chaining_sets_bits(), 22);
    let first = p.chaining_set_range(0);
    let second = p.chaining_set_range(1);
    assert_eq!(first.start, 0);
    assert_eq!(second.start, first.end + 1);
    assert!(first.contains(first.end));
    assert!(!first.contains(second.start));
}

#[test]
fn strength_is_bounded_on_both_sides() {
    let id = Bytes32::from([1u8; 32]);
    assert!(
        ProofParams::new(id, 28, 1, false).is_err(),
        "strength 1 accepted"
    );
    assert!(
        ProofParams::new(id, 28, 64, false).is_err(),
        "strength 64 accepted"
    );
    // k28 leaves 2 section bits, so the ceiling is 28 - 2 - 1 = 25.
    assert!(ProofParams::new(id, 28, 25, false).is_ok());
    assert!(
        ProofParams::new(id, 28, 26, false).is_err(),
        "over the ceiling accepted"
    );
}
