use dg_xch_core::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::{
    calculate_ip_iters, calculate_iterations_quality, calculate_sp_iters, expected_plot_size,
    is_overflow_block,
};
use dg_xch_serialize::hash_256;
use lazy_static::lazy_static;
use num_bigint::BigInt;
use num_traits::abs;
use std::collections::HashMap;

lazy_static! {
    static ref TEST_CONSTANTS: ConsensusConstants = ConsensusConstants {
        num_sps_sub_slot: 32,
        sub_slot_time_target: BigInt::from(300),
        ..Default::default()
    };
}

#[tokio::test]
async fn test_pot_iterations() {
    assert!(!is_overflow_block(&TEST_CONSTANTS, 27).unwrap());
    assert!(!is_overflow_block(&TEST_CONSTANTS, 28).unwrap());
    assert!(is_overflow_block(&TEST_CONSTANTS, 29).unwrap());
    assert!(is_overflow_block(&TEST_CONSTANTS, 30).unwrap());
    assert!(is_overflow_block(&TEST_CONSTANTS, 31).unwrap());
    assert!(is_overflow_block(&TEST_CONSTANTS, 32).is_err());
}

#[tokio::test]
async fn test_calculate_sp_iters() {
    let ssi: u64 = 100001 * 64 * 4;
    assert!(calculate_sp_iters(&TEST_CONSTANTS, ssi, 32).is_err());
    assert!(calculate_sp_iters(&TEST_CONSTANTS, ssi, 31).is_ok());
}

#[tokio::test]
async fn test_calculate_ip_iters() {
    let ssi: u64 = 100001 * 64 * 4;
    let sp_interval_iters = ssi / TEST_CONSTANTS.num_sps_sub_slot as u64;
    //Invalid signage point index
    assert!(calculate_ip_iters(&TEST_CONSTANTS, ssi, 123, 100000).is_err());
    let mut sp_iters = sp_interval_iters * 13;
    //required_iters too high
    assert!(calculate_ip_iters(
        &TEST_CONSTANTS,
        ssi,
        sp_interval_iters as u8,
        sp_interval_iters
    )
    .is_err());
    //required_iters too high
    assert!(calculate_ip_iters(
        &TEST_CONSTANTS,
        ssi,
        sp_interval_iters as u8,
        sp_interval_iters * 12
    )
    .is_err());
    //required_iters too low (0)
    assert!(calculate_ip_iters(&TEST_CONSTANTS, ssi, sp_interval_iters as u8, 0).is_err());

    let mut required_iters = sp_interval_iters - 1;
    let mut ip_iters = calculate_ip_iters(&TEST_CONSTANTS, ssi, 13, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        sp_iters + &TEST_CONSTANTS.num_sp_intervals_extra * sp_interval_iters + required_iters
    );

    required_iters = 1;
    ip_iters = calculate_ip_iters(&TEST_CONSTANTS, ssi, 13, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        sp_iters + &TEST_CONSTANTS.num_sp_intervals_extra * sp_interval_iters + required_iters
    );

    required_iters = ssi * 4 / 300;
    ip_iters = calculate_ip_iters(&TEST_CONSTANTS, ssi, 13, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        sp_iters + &TEST_CONSTANTS.num_sp_intervals_extra * sp_interval_iters + required_iters
    );
    assert!(sp_iters < ip_iters);

    //Overflow
    sp_iters = sp_interval_iters * (TEST_CONSTANTS.num_sps_sub_slot as u64 - 1);
    ip_iters = calculate_ip_iters(
        &TEST_CONSTANTS,
        ssi,
        TEST_CONSTANTS.num_sps_sub_slot as u8 - 1,
        required_iters,
    )
    .unwrap();
    assert_eq!(
        ip_iters,
        (sp_iters + TEST_CONSTANTS.num_sp_intervals_extra * sp_interval_iters + required_iters)
            % ssi
    );
    assert!(sp_iters > ip_iters);
}

#[tokio::test]
async fn test_win_percentage() {
    /*
    Tests that the percentage of blocks won is proportional to the space of each farmer,
    with the assumption that all farmers have access to the same VDF speed.
    */
    let farmer_ks = HashMap::from([(32u8, 100), (33, 100), (34, 100), (35, 100), (36, 100)]);
    let mut farmer_space = HashMap::new();
    for (k, _) in farmer_ks.iter() {
        farmer_space.insert(*k, expected_plot_size(*k));
    }
    let total_space = farmer_space.values().sum::<u64>();
    let mut percentage_space = HashMap::new();
    for (k, sp) in farmer_space.iter() {
        percentage_space.insert(*k, *sp as f64 / total_space as f64);
    }
    let mut wins = HashMap::new();
    for (k, _) in farmer_ks.iter() {
        wins.insert(*k, 0i32);
    }
    let total_slots = 50u32;
    let num_sps = 16u32;
    let sp_interval_iters = 100000000u64 / 32;
    let difficulty = 500000000000u64;
    for slot_index in 0..total_slots {
        for sp_index in 0..num_sps {
            let sp_hash = hash_256(
                slot_index
                    .to_be_bytes()
                    .into_iter()
                    .chain(sp_index.to_be_bytes().into_iter())
                    .collect::<Vec<u8>>(),
            );
            let sp_hash = Bytes32::new(&sp_hash);
            for (k, count) in farmer_ks.iter() {
                for farmer_index in 0i32..*count {
                    let quality = hash_256(
                        slot_index
                            .to_be_bytes()
                            .into_iter()
                            .chain(k.to_be_bytes().into_iter())
                            .chain(farmer_index.to_be_bytes().into_iter())
                            .collect::<Vec<u8>>(),
                    );
                    let quality = Bytes32::new(&quality);
                    let required_iters = calculate_iterations_quality(
                        2u128.pow(25),
                        &quality,
                        *k,
                        difficulty,
                        &sp_hash,
                    );
                    if required_iters < sp_interval_iters {
                        *wins.get_mut(k).unwrap() += 1;
                    }
                }
            }
        }
    }
    let mut win_percentage = HashMap::new();
    for k in farmer_ks.keys() {
        win_percentage.insert(
            *k,
            *wins.get_mut(k).unwrap() as f64 / wins.values().sum::<i32>() as f64,
        );
    }
    for k in farmer_ks.keys() {
        //Win rate is proportional to percentage of space
        assert!(abs(win_percentage.get(k).unwrap() - percentage_space.get(k).unwrap()) < 0.01);
    }
}
