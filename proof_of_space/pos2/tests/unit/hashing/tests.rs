use super::*;

fn params(strength: u8, testnet: bool) -> ProofParams {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    ProofParams::new(Bytes32::from(bytes), 28, strength, testnet).expect("params")
}

#[test]
fn testnet_and_mainnet_g_differ() {
    let main = ProofHashing::new(params(2, false));
    let test = ProofHashing::new(params(2, true));
    assert_ne!(main.g(1234), test.g(1234));
    // Mainnet g is the plain hash, with no constant folded in.
    assert_eq!(main.g(1234), main.aes().g_x(1234, AES_G_ROUNDS));
    assert_eq!(
        test.g(1234),
        test.aes().g_x(1234 ^ TESTNET_G_XOR_CONST, AES_G_ROUNDS)
    );
}

#[test]
fn strength_only_adds_rounds_on_table_one() {
    let weak = ProofHashing::new(params(2, false));
    let strong = ProofHashing::new(params(6, false));
    // Table 1 pairings and targets move with strength.
    assert_ne!(
        weak.matching_target(1, 5, 99, 20),
        strong.matching_target(1, 5, 99, 20)
    );
    // Later tables do not.
    assert_eq!(
        weak.matching_target(3, 5, 99, 20),
        strong.matching_target(3, 5, 99, 20)
    );
    assert_eq!(
        weak.pairing_t2(1, 2, 28, 56, 2),
        strong.pairing_t2(1, 2, 28, 56, 2)
    );
}

#[test]
fn a_pairing_is_split_into_its_three_fields() {
    let h = ProofHashing::new(params(2, false));
    let r = h.pairing_t2(0x1234, 0x5678, 28, 56, 2);
    assert!(r.match_info < (1 << 28), "match info exceeded k bits");
    assert!(r.meta < (1u64 << 56), "meta exceeded 2k bits");
    assert!(r.test < 4, "test bits exceeded their width");
}

#[test]
fn table_three_reports_only_filter_bits() {
    let h = ProofHashing::new(params(2, false));
    let r = h.pairing_t3(7, 9, 2);
    assert_eq!(r.match_info, 0);
    assert_eq!(r.meta, 0);
    assert!(r.test < 4);
    // It shares the underlying pairing with table 2, so the filter bits agree.
    assert_eq!(r.test, h.pairing_t2(7, 9, 28, 56, 2).test);
}

#[test]
fn every_chain_link_gets_its_own_challenge() {
    let h = ProofHashing::new(params(2, false));
    let links = h.chaining_challenge_with_plot_id_hash(Bytes32::from([3u8; 32]));
    let mut seen = std::collections::HashSet::new();
    for link in links {
        assert!(seen.insert(link), "chain link challenge repeated");
    }
    // Deterministic, and bound to the challenge.
    assert_eq!(
        links,
        h.chaining_challenge_with_plot_id_hash(Bytes32::from([3u8; 32]))
    );
    assert_ne!(
        links,
        h.chaining_challenge_with_plot_id_hash(Bytes32::from([4u8; 32]))
    );
}
#[test]
fn batch_g_matches_scalar_across_networks_and_lane_boundaries() {
    for testnet in [false, true] {
        for strength in [2, 6] {
            let hashing = ProofHashing::new(params(strength, testnet));
            let inputs: [u32; 17] =
                std::array::from_fn(|index| (index as u32).wrapping_mul(2_654_435_761));
            let mut output = [0; 17];
            hashing.g_batch(&inputs, &mut output);
            assert_eq!(output, inputs.map(|value| hashing.g(value)));
            hashing.g_batch(&[], &mut []);
        }
    }
}
