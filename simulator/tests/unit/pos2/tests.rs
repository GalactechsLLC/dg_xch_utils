use super::*;
use dg_xch_pos::pos2::bits::expand_bits;
use dg_xch_pos::pos2::constants::TOTAL_XS_IN_PROOF;
use dg_xch_pos::pos2::params::ProofParams;
use dg_xch_pos::pos2::validator::ProofValidator;

const K: u8 = 18;
const STRENGTH: u8 = 2;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dgxch_sim_pos2_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_plot_set_is_created_once_and_reused() {
    let dir = scratch("reuse");
    let first = PlotSet::setup(&dir, 1, 2, K, STRENGTH, false).expect("setup");
    assert_eq!(first.plots.len(), 2);
    assert!(first.plots.iter().all(|p| p.created), "plots were not made");
    assert!(first.plots.iter().all(|p| p.path.exists()));

    let second = PlotSet::setup(&dir, 1, 2, K, STRENGTH, false).expect("setup");
    assert!(
        second.plots.iter().all(|p| !p.created),
        "plots were remade instead of reused"
    );
    // Same seed, same ids, same files.
    for (a, b) in first.plots.iter().zip(second.plots.iter()) {
        assert_eq!(a.plot_id, b.plot_id);
        assert_eq!(a.path, b.path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_different_campaign_seed_gives_different_plots() {
    let dir = scratch("seeds");
    let a = PlotSet::setup(&dir, 1, 1, K, STRENGTH, false).expect("setup");
    let b = PlotSet::setup(&dir, 2, 1, K, STRENGTH, false).expect("setup");
    assert_ne!(a.plots[0].plot_id, b.plots[0].plot_id);
    assert_ne!(a.plots[0].path, b.plots[0].path);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn farming_produces_a_proof_the_validator_accepts() {
    // A challenge that hits yields a proof the consensus verifier accepts.
    let dir = scratch("farm");
    let set = PlotSet::setup(&dir, 7, 8, K, STRENGTH, false).expect("setup");

    let mut checked = 0;
    for attempt in 0u8..64 {
        let mut challenge = [0u8; 32];
        challenge[0] = attempt;
        let hits = set
            .qualities_for_challenge(Bytes32::from(challenge))
            .expect("qualities");
        for (plot_index, quality) in hits {
            let proof = set.solve(plot_index, &quality);
            if proof.is_empty() {
                continue;
            }
            let plot = &set.plots[plot_index];
            let validator = ProofValidator::new(
                ProofParams::new(plot.plot_id, K, STRENGTH, false).expect("params"),
            )
            .expect("validator");
            let xs = expand_bits(&proof, K).expect("proof expands");
            assert_eq!(xs.len(), TOTAL_XS_IN_PROOF);
            let xs: [u32; TOTAL_XS_IN_PROOF] = xs.try_into().expect("128 x values");
            let fragments = validator
                .validate_full_proof(&xs, Bytes32::from(challenge))
                .expect("a farmed proof must validate");
            assert_eq!(fragments, quality.chain_links);
            checked += 1;
            if checked >= 2 {
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        checked > 0,
        "no challenge produced a proof across 64 attempts"
    );
}

#[test]
fn a_farmed_proof_passes_the_consensus_verifier() {
    // Derive the pos challenge as a signage point does, farm it, wrap the result in a v2
    // ProofOfSpace, and run it through verify_and_get_quality_string.
    use dg_xch_core::blockchain::proof_of_space::ProofBytes;
    use dg_xch_core::blockchain::proof_of_space::{
        ProofOfSpace, calculate_pos_challenge, calculate_prefix_bits_v2, passes_plot_filter,
    };
    use dg_xch_core::consensus::constants::MAINNET;
    use dg_xch_core::consensus::overrides::{ConsensusOverrides, apply_overrides};
    use dg_xch_pos::pos2::quality::quality_hash;
    use dg_xch_pos::verify_and_get_quality_string;

    let dir = scratch("consensus");
    let set = PlotSet::setup(&dir, 11, 8, K, STRENGTH, false).expect("setup");
    let constants = apply_overrides(
        MAINNET,
        &ConsensusOverrides {
            plot_size_v2: Some(K),
            ..Default::default()
        },
    );

    let mut verified = 0;
    'search: for attempt in 0u8..=255 {
        let original = Bytes32::from([attempt; 32]);
        let sp = Bytes32::from([attempt ^ 0x5A; 32]);
        for (index, plot) in set.plots.iter().enumerate() {
            if !passes_plot_filter(
                calculate_prefix_bits_v2(&constants, 0),
                plot.plot_id,
                original,
                sp,
            ) {
                continue;
            }
            let pos_challenge = calculate_pos_challenge(plot.plot_id, original, sp);
            for (_, chain) in set
                .qualities_for_challenge(pos_challenge)
                .expect("qualities")
                .into_iter()
                .filter(|(i, _)| *i == index)
            {
                let proof = set.solve(index, &chain);
                if proof.is_empty() {
                    continue;
                }
                let pos = ProofOfSpace::v2(
                    pos_challenge,
                    Some(plot.keys.pool_public_key),
                    None,
                    plot.keys.plot_public_key,
                    u16::try_from(index).expect("index"),
                    0,
                    STRENGTH,
                    ProofBytes::from(proof),
                );
                let quality = verify_and_get_quality_string(&pos, &constants, original, sp, 0)
                    .expect("a farmed proof must clear the consensus gate");
                assert_eq!(quality, quality_hash(&chain.chain_links, STRENGTH));
                verified += 1;
                if verified >= 2 {
                    break 'search;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(verified > 0, "no farmed proof cleared the consensus gate");
}
