use super::*;
use dg_xch_core::consensus::constants::SIMULATOR;
use dg_xch_core::consensus::overrides::{ConsensusOverrides, apply_overrides};

const K: u8 = 18;
const STRENGTH: u8 = 2;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dgxch_sim_factory_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// SIMULATOR with the v2 plot size at k18, the v2 plot filter disabled so every plot is a
/// candidate, and a low difficulty so a farmed proof lands in the infusion range.
fn constants() -> ConsensusConstants {
    apply_overrides(
        SIMULATOR,
        &ConsensusOverrides {
            plot_size_v2: Some(K),
            number_zero_bits_plot_filter_v2: Some(0),
            // With the mainnet 2^67 factor a k18 proof's required-iters sits far above the
            // infusion range and never wins; 2^25 brings it into a range where a small plot
            // set finds a genesis proof.
            difficulty_constant_factor: Some(2u128.pow(25)),
            difficulty_starting: Some(7),
            // A tiny discriminant makes the real VDF finish in microseconds and validate
            // unchanged; a small sub-slot keeps ip_iters low so the proof is a handful of
            // squarings. 65536 = 64 * 1024, so it still divides the signage points evenly.
            discriminant_size_bits: Some(num_bigint::BigInt::from(16)),
            sub_slot_iters_starting: Some(65_536),
            ..Default::default()
        },
    )
}

#[test]
fn a_genesis_proof_is_farmed_and_assembled_into_an_unfinished_block() {
    let c = constants();
    let dir = scratch("genesis");
    let plots = PlotSet::setup(&dir, 3, 4, K, STRENGTH, false).expect("plots");

    let farmed = farm_genesis(&c, &plots, c.difficulty_starting, c.sub_slot_iters_starting)
        .expect("a genesis proof must be farmable");
    assert_eq!(farmed.proof_of_space.version, 1, "genesis proof is v2");
    assert!(farmed.iters.required_iters >= 1);

    let ub = build_genesis_unfinished(&c, &farmed, Bytes32::from([0xAB; 32]), 1_700_000_000)
        .expect("the producer must accept a farmed v2 proof");

    // The assembled block carries the farmed proof, and that proof still verifies against the
    // genesis challenge — the pos2 farming path and the producer agree.
    assert_eq!(
        ub.reward_chain_block.proof_of_space, farmed.proof_of_space,
        "the unfinished block did not carry the farmed proof"
    );
    assert!(
        dg_xch_pos::verify_and_get_quality_string(
            &ub.reward_chain_block.proof_of_space,
            &c,
            c.genesis_challenge,
            c.genesis_challenge,
            0,
        )
        .is_some(),
        "the embedded proof no longer verifies"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_farmed_genesis_block_is_accepted_by_the_engine() {
    use dg_xch_node::engine::{AddBlockOutcome, Engine};
    use dg_xch_node::primitives::NativePrimitives;
    use dg_xch_stores::SqliteStore;

    let c = constants();
    let dir = scratch("engine");
    let plots = PlotSet::setup(&dir, 5, 4, K, STRENGTH, false).expect("plots");

    let farmed = farm_genesis(&c, &plots, c.difficulty_starting, c.sub_slot_iters_starting)
        .expect("farm genesis");
    let ub = build_genesis_unfinished(&c, &farmed, Bytes32::from([0xAB; 32]), 1_700_000_000)
        .expect("assemble");
    let full = build_genesis_full(&c, &ub, &farmed).expect("finish");
    assert_eq!(full.reward_chain_block.height, 0, "genesis is height 0");

    // A block farmed and finished from scratch clears consensus and becomes the peak; the
    // small-discriminant VDFs are real, so add_block's VDF gates run.
    let db = tempfile::tempdir().expect("tempdir");
    let store = SqliteStore::open(&db.path().join("sim.sqlite"))
        .await
        .expect("open store");
    let mut engine = Engine::new(store, NativePrimitives, c);
    let outcome = engine.add_block(&full).await.expect("add_block");
    assert!(
        matches!(outcome, AddBlockOutcome::NewPeak { height: 0 }),
        "genesis did not become the peak: {outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
