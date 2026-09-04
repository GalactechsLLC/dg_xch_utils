use super::*;
use std::collections::HashSet;

#[test]
fn the_same_seed_and_index_derive_the_same_plot() {
    let a = PlotKeys::derive(7, 0).expect("derives");
    let b = PlotKeys::derive(7, 0).expect("derives");
    assert_eq!(a.plot_id(2, 0, 0), b.plot_id(2, 0, 0));
    assert_eq!(a.plot_public_key, b.plot_public_key);
    assert_eq!(a.master.to_bytes(), b.master.to_bytes());
}

#[test]
fn every_seed_and_index_gives_a_distinct_plot_id() {
    let mut seen = HashSet::new();
    for campaign_seed in 0..8u64 {
        for plot_index in 0..8u16 {
            let keys = PlotKeys::derive(campaign_seed, u32::from(plot_index)).expect("derives");
            assert!(
                seen.insert(keys.plot_id(2, plot_index, 0)),
                "plot id collided at ({campaign_seed}, {plot_index})"
            );
        }
    }
}

#[test]
fn the_plot_id_is_the_v2_derivation_a_proof_makes() {
    // A verifier recomputes the id from the proof fields alone, so the plot must be created
    // under exactly this derivation or nothing it farms will verify.
    let keys = PlotKeys::derive(3, 5).expect("derives");
    assert_eq!(
        keys.plot_id(2, 5, 0),
        calculate_plot_id_v2(
            2,
            keys.plot_public_key,
            Some(keys.pool_public_key),
            None,
            5,
            0
        )
    );
    // Strength, index and meta group all separate ids on the same keys.
    assert_ne!(keys.plot_id(2, 5, 0), keys.plot_id(3, 5, 0));
    assert_ne!(keys.plot_id(2, 5, 0), keys.plot_id(2, 6, 0));
    assert_ne!(keys.plot_id(2, 5, 0), keys.plot_id(2, 5, 1));
}

#[test]
fn the_plot_public_key_aggregates_the_local_and_farmer_keys() {
    let keys = PlotKeys::derive(11, 2).expect("derives");
    let expected = generate_plot_public_key(&keys.local.sk_to_pk(), &keys.farmer.sk_to_pk(), false)
        .expect("aggregates");
    assert_eq!(keys.plot_public_key.bytes(), expected.to_bytes());
}
