use super::*;
use crate::node::hardening::{Source, fixture, node};

#[tokio::test]
async fn a_rejected_anchor_span_moves_to_the_next_peer_in_the_same_attempt() {
    use dg_xch_core::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
    use dg_xch_core::blockchain::reward_chain_subslot::RewardChainSubSlot;
    use dg_xch_core::blockchain::subslot_proofs::SubSlotProofs;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;

    let (_directory, node) = node().await;
    let target_height = 220_000u32;
    let base = fixture();
    let span = |declare_ses: bool| -> Vec<dg_xch_core::blockchain::full_block::FullBlock> {
        let mut blocks: Vec<_> = (target_height - 64..=target_height + 31)
            .map(|height| {
                let mut block = base.clone();
                block.reward_chain_block.height = height;
                block
            })
            .collect();
        if declare_ses {
            let vdf = VdfInfo {
                challenge: Bytes32::default(),
                number_of_iterations: 1,
                output: ClassgroupElement::get_default_element(),
            };
            let proof = VdfProof {
                witness_type: 0,
                witness: dg_xch_core::blockchain::unsized_bytes::UnsizedBytes::default(),
                normalized_to_identity: false,
            };
            blocks[0].finished_sub_slots = vec![EndOfSubSlotBundle {
                challenge_chain: ChallengeChainSubSlot {
                    challenge_chain_end_of_slot_vdf: vdf,
                    infused_challenge_chain_sub_slot_hash: None,
                    subepoch_summary_hash: Some(Bytes32::from([0x5e; 32])),
                    new_sub_slot_iters: None,
                    new_difficulty: None,
                },
                infused_challenge_chain: None,
                reward_chain: RewardChainSubSlot {
                    end_of_slot_vdf: vdf,
                    challenge_chain_sub_slot_hash: Bytes32::default(),
                    infused_challenge_chain_sub_slot_hash: None,
                    deficit: 16,
                },
                proofs: SubSlotProofs {
                    challenge_chain_slot_proof: proof.clone(),
                    infused_challenge_chain_slot_proof: None,
                    reward_chain_slot_proof: proof,
                },
            }];
        }
        blocks
    };
    let first = Arc::new(Source {
        blocks: span(true),
        calls: std::sync::atomic::AtomicUsize::new(0),
        id: 1,
    });
    let second = Arc::new(Source {
        blocks: span(false),
        calls: std::sync::atomic::AtomicUsize::new(0),
        id: 2,
    });
    let sources: Vec<Arc<dyn BlockRangeSource>> = vec![
        first.clone() as Arc<dyn BlockRangeSource>,
        second.clone() as _,
    ];
    let validated = ValidatedTip {
        tip: Bytes32::default(),
        wp: Arc::new(WeightProof {
            sub_epochs: Vec::new(),
            sub_epoch_segments: Vec::new(),
            recent_chain_data: Vec::new(),
        }),
        summaries: Arc::new(Vec::new()),
    };
    let anchored = node
        .anchor_with_sources(&sources, &validated, target_height)
        .await
        .expect("a rejected span fails over instead of failing the attempt");
    assert!(!anchored, "junk spans cannot anchor");
    assert!(
        second.calls.load(Ordering::Relaxed) > 0,
        "a span the header pass rejected must burn only its peer — the second peer's span \
         is tried in the same attempt"
    );
}
