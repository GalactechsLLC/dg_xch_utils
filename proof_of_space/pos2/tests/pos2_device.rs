use dg_xch_pos2::{core::ProofCore, device, params::ProofParams};

#[test]
fn compact_matching_supports_every_reference_size_and_strength() {
    for k in (18..=32).step_by(2) {
        let maximum = k - if k < 28 { 2 } else { k - 26 } - 1;
        for strength in 2..=maximum {
            for testnet in [false, true] {
                let params = ProofParams::new([0xa5; 32].into(), k, strength, testnet).unwrap();
                let config = dg_xch_pos2::compute::config(&params);
                let core = ProofCore::new(params.clone()).unwrap();
                let left = device::Record {
                    meta: u64::MAX >> (64 - 2 * k),
                    info: u32::MAX >> (32 - k),
                    ..Default::default()
                };
                for table in 2..=3 {
                    for key in [0, (params.num_match_keys(table as usize) - 1) as u32] {
                        let target = device::target(config, table, left, key);
                        assert!(core.validate_match_info_pairing(
                            table as usize,
                            left.meta,
                            left.info,
                            target
                        ));
                    }
                }
            }
        }
    }
}

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
