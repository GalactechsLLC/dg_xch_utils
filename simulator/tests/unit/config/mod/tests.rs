use super::*;
use crate::error::ValidationTier;
use num_bigint::BigInt;

fn config(network: ChiaNetwork, consensus: ConsensusOverrides) -> SimConfig {
    SimConfig {
        network,
        consensus,
        harness: HarnessConfig {
            campaign_seed: 1,
            n_runs: 1,
            horizon_blocks: 1,
        },
    }
}

#[test]
fn the_stock_networks_validate() {
    for network in [ChiaNetwork::Mainnet, ChiaNetwork::Simulator] {
        config(network, ConsensusOverrides::default())
            .constants()
            .unwrap_or_else(|e| panic!("{network:?} rejected by its own rules: {e}"));
    }
}

#[test]
fn a_tiny_discriminant_is_accepted() {
    let c = config(
        ChiaNetwork::Simulator,
        ConsensusOverrides {
            discriminant_size_bits: Some(BigInt::from(16)),
            ..Default::default()
        },
    )
    .constants()
    .expect("16 bits is positive, under 1024, and a multiple of 8");
    assert_eq!(c.discriminant_size_bits, 16);
}

#[test]
fn a_discriminant_off_the_byte_boundary_is_a_range_error() {
    let e = config(
        ChiaNetwork::Mainnet,
        ConsensusOverrides {
            discriminant_size_bits: Some(BigInt::from(20)),
            ..Default::default()
        },
    )
    .constants()
    .expect_err("20 is not a multiple of 8");
    assert_eq!(e.tier, ValidationTier::Range);
    assert_eq!(e.field, "consensus.discriminant_size_bits");
}

#[test]
fn sub_slot_iters_must_divide_into_signage_points() {
    let e = config(
        ChiaNetwork::Mainnet,
        ConsensusOverrides {
            sub_slot_iters_starting: Some(1_000),
            num_sps_sub_slot: Some(64),
            ..Default::default()
        },
    )
    .constants()
    .expect_err("1000 is not a multiple of 64");
    assert_eq!(e.tier, ValidationTier::CrossField);
    assert!(e.field.contains("sub_slot_iters_starting"), "{}", e.field);
}

#[test]
fn epoch_blocks_must_be_a_multiple_of_sub_epoch_blocks() {
    let e = config(
        ChiaNetwork::Mainnet,
        ConsensusOverrides {
            epoch_blocks: Some(4_609),
            ..Default::default()
        },
    )
    .constants()
    .expect_err("4609 is not a multiple of 384");
    assert_eq!(e.tier, ValidationTier::CrossField);
    assert!(e.field.contains("epoch_blocks"), "{}", e.field);
}

#[test]
fn max_sub_slot_blocks_is_bounded_on_both_sides() {
    let low = config(
        ChiaNetwork::Mainnet,
        ConsensusOverrides {
            max_sub_slot_blocks: Some(16),
            ..Default::default()
        },
    )
    .constants()
    .expect_err("16 does not exceed slot_blocks_target 32");
    assert!(low.field.contains("slot_blocks_target"), "{}", low.field);

    let high = config(
        ChiaNetwork::Mainnet,
        ConsensusOverrides {
            max_sub_slot_blocks: Some(192),
            ..Default::default()
        },
    )
    .constants()
    .expect_err("192 is not below sub_epoch_blocks/2 of 192");
    assert!(high.field.contains("sub_epoch_blocks"), "{}", high.field);
}

#[test]
fn an_empty_campaign_is_a_range_error() {
    let mut c = config(ChiaNetwork::Simulator, ConsensusOverrides::default());
    c.harness.n_runs = 0;
    let e = c.constants().expect_err("zero runs");
    assert_eq!(e.tier, ValidationTier::Range);
    assert_eq!(e.field, "harness.n_runs");
}

#[test]
fn a_network_round_trips_through_serde_by_its_lowercase_name() {
    let c = config(ChiaNetwork::Simulator, ConsensusOverrides::default());
    let json = serde_json::to_string(&c).expect("serialize");
    assert!(json.contains("\"simulator\""), "{json}");
    let back: SimConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, c);
}
