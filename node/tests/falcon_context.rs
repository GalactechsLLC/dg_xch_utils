#![cfg(feature = "falcon")]

use dg_xch_core::{
    blockchain::full_block::FullBlock,
    consensus::chain_definition::ChainDefinition,
    signatures::{
        extension::FalconAuthorization,
        falcon::FalconSigner,
        falcon_block::{FalconHeaderAuthorization, public_key_id},
    },
};
use dg_xch_node::{chain_primitives::ChainPrimitives, primitives::ConsensusPrimitives};

#[test]
fn validation_context_cannot_reuse_evidence_from_a_mutated_or_different_block() {
    let constants = ChainDefinition::falcon_development("verified context test".into())
        .constants()
        .unwrap();
    let genesis = constants.genesis_challenge;
    let signer = FalconSigner::generate().unwrap();
    let mut proofs: Vec<_> = (0..5)
        .map(|i| FalconHeaderAuthorization::sign(&signer, genesis, &[i; 32]).unwrap())
        .collect();
    let mut block: FullBlock = serde_json::from_str(include_str!(
        "../../full-node/tests/fixtures/full_block_5000000.json"
    ))
    .unwrap();
    block.reward_chain_block.challenge_chain_sp_signature = proofs[0].reference(genesis);
    block.reward_chain_block.reward_chain_sp_signature = proofs[1].reference(genesis);
    block.foliage.foliage_block_data_signature = proofs[2].reference(genesis);
    block.foliage.foliage_transaction_block_signature = Some(proofs[3].reference(genesis));
    block.foliage.foliage_block_data.pool_signature = Some(proofs[4].reference(genesis));
    let mut auth = FalconAuthorization {
        labrador: None,
        genesis,
        header: proofs.clone(),
        transactions: vec![],
    };
    auth.canonicalize();
    block.authorization = auth.into();
    let unbound = ChainPrimitives::new(&constants).unwrap();
    let bound = unbound.for_block(&block, &constants).unwrap().unwrap();
    let key = public_key_id(&proofs[0].public_key);
    let message = &proofs[0].message;
    let reference = proofs[0].reference(genesis);
    assert!(!unbound.check_block_context(&block));
    assert!(!unbound.verify_header_signature(&key, message, &reference));
    assert!(bound.check_block_context(&block));
    assert!(bound.verify_header_signature(&key, message, &reference));
    assert!(!bound.verify_header_signature(&key, b"changed message", &reference));

    let mut changed = block.clone();
    changed.authorization.as_mut().unwrap().header[0].signature[50] ^= 1;
    assert!(!bound.check_block_context(&changed));
    assert!(unbound.for_block(&changed, &constants).is_err());
    changed = block.clone();
    changed
        .authorization
        .as_mut()
        .unwrap()
        .transactions
        .push(proofs[0].signature);
    assert!(!bound.check_block_context(&changed));
    changed = block.clone();
    changed.transactions_generator = None;
    changed.authorization.as_mut().unwrap().labrador = Some(vec![0; 54]);
    assert!(!bound.check_block_context(&changed));
    assert!(unbound.for_block(&changed, &constants).is_err());
    changed = block.clone();
    changed.authorization.as_mut().unwrap().header.pop();
    assert!(unbound.for_block(&changed, &constants).is_err());
    changed = block.clone();
    changed.foliage.foliage_block_data_signature = Default::default();
    assert!(unbound.for_block(&changed, &constants).is_err());

    // A valid signature for a different message must not satisfy an old reference.
    proofs[0] = FalconHeaderAuthorization::sign(&signer, genesis, b"another block").unwrap();
    changed = block.clone();
    changed.authorization.as_mut().unwrap().header = proofs;
    changed.authorization.as_mut().unwrap().canonicalize();
    assert!(!bound.check_block_context(&changed));
    assert!(unbound.for_block(&changed, &constants).is_err());
    let bls = ChainDefinition::dgx().constants().unwrap();
    assert!(
        ChainPrimitives::new(&bls)
            .unwrap()
            .for_block(&block, &bls)
            .is_err()
    );
}
