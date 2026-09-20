use dg_xch_pos2::{core::ProofCore, device, params::ProofParams};

#[test]
fn device_hashes_targets_and_fragments_match_native_cpu() {
    for k in [18, 20, 22, 24, 26, 28, 30, 32] {
        for strength in [2, 3, 5] {
            for testnet in [false, true] {
                let plot_id = std::array::from_fn(|index| (index * 7) as u8);
                let params = ProofParams::new(plot_id.into(), k, strength, testnet).unwrap();
                let core = ProofCore::new(params).unwrap();
                let config = device::Config {
                    plot_id,
                    k: k.into(),
                    strength: strength.into(),
                    testnet: u32::from(testnet),
                };
                for value in [0, 1, 255, 65537, ((1u64 << k) - 1) as u32] {
                    let entry = device::generate(config, value);
                    assert_eq!(entry.info, core.hashing.g(value));
                    for table in 1..=3 {
                        for key in 0..4 {
                            let info = device::target(config, table, entry, key);
                            assert!(core.validate_match_info_pairing(
                                table as usize,
                                entry.meta,
                                entry.info,
                                info
                            ));
                        }
                    }
                    let mask = u64::MAX >> (64 - 2 * k);
                    let input = (u64::from(value) * 1234567) & mask;
                    assert_eq!(
                        device::fragment(config, input),
                        core.fragment_codec.encode_bits(input)
                    );
                }
                for value in 0..32 {
                    let left = device::generate(config, value);
                    let right = device::generate(config, value + 1);
                    let result = device::pair(config, 1, left, right);
                    let expected = core.pairing_t1(value, value + 1);
                    assert_eq!(result.valid == 1, expected.is_some());
                    if let Some(expected) = expected {
                        assert_eq!(result.meta, expected.meta);
                        assert_eq!(result.info, expected.match_info);
                    }
                }
            }
        }
    }
}
