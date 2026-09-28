// INVARIANT-ASSERT harness for `create_unfinished_block`: assemble an UnfinishedBlock from
// hand-built signage-point inputs and a genesis foliage and assert the structural
// invariants: field propagation into RewardChainBlockUnfinished, the
// foliage↔reward-block-hash tie, the is_transaction_block consistency, and hash stability.
use super::{BlockTransactions, create_unfinished_block, g2_infinity};
use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::coin::Coin;
use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use crate::clvm::bls_bindings::{sign, verify_signature};
use crate::clvm::program::SerializedProgram;
use crate::consensus::constants::MAINNET;
use crate::traits::SizedBytes;
use blst::min_pk::{PublicKey, SecretKey, Signature};

fn ph(byte: u8) -> Bytes32 {
    Bytes32::new([byte; 32])
}

fn plot_sk(seed: u8) -> SecretKey {
    SecretKey::key_gen_v3(&[seed; 32], &[]).expect("deterministic plot sk")
}

fn plot_pk_bytes(sk: &SecretKey) -> Bytes48 {
    sk.sk_to_pk().into()
}

/// Real AugScheme plot signer; the produced signature verifies against
/// `plot_pk_bytes(sk)` under [`verify_signature`].
fn real_signer(sk: &SecretKey) -> impl Fn(Bytes32, &Bytes48) -> Bytes96 + '_ {
    move |msg: Bytes32, _plot_pk: &Bytes48| -> Bytes96 { sign(sk, msg.as_ref()).into() }
}

/// A structural marker signer: it does NOT produce a valid BLS signature; it packs the signed message
/// into the first 32 bytes of the `Bytes96` so a test can assert exactly WHICH hash was signed into
/// WHICH slot (cc_sp vs rc_sp vs foliage). Never fed to `verify_signature`.
fn marker_signer(msg: Bytes32, _plot_pk: &Bytes48) -> Bytes96 {
    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(msg.as_ref());
    Bytes96::new(buf)
}

fn verifies(plot_pk: &Bytes48, msg: Bytes32, sig: &Bytes96) -> bool {
    let pk: PublicKey = plot_pk.into();
    let Ok(sig) = Signature::try_from(sig) else {
        return false;
    };
    verify_signature(&pk, msg.as_ref(), &sig)
}

/// Proof of space carrying an explicit plot public key (the G1 handed to the plot signer).
fn mk_pos_with_key(plot_public_key: Bytes48) -> ProofOfSpace {
    ProofOfSpace {
        version: 0,
        plot_index: 0,
        meta_group: 0,
        strength: 0,
        challenge: ph(0x01),
        pool_public_key: None,
        pool_contract_puzzle_hash: Some(ph(0x02)),
        plot_public_key,
        size: 32,
        proof: ProofBytes::from(vec![0x07u8; 64]),
    }
}

fn mk_pos() -> ProofOfSpace {
    mk_pos_with_key(Bytes48::new([0x03; 48]))
}

fn mk_vdf(challenge: u8, iters: u64) -> VdfInfo {
    VdfInfo {
        challenge: ph(challenge),
        number_of_iterations: iters,
        output: ClassgroupElement::get_default_element(),
    }
}

fn mk_vdf_proof(witness_type: u8) -> VdfProof {
    VdfProof {
        witness_type,
        witness: UnsizedBytes::new(vec![0xAA, 0xBB, 0xCC]),
        normalized_to_identity: true,
    }
}

fn pool_target() -> PoolTarget {
    PoolTarget {
        puzzle_hash: ph(0x01),
        max_height: 0,
    }
}

// The genesis unfinished block: height 0, transaction block, no claims / no spends. Assert every
// signage-point field propagates into the reward block, the sp VDF proofs carry into the
// UnfinishedBlock, the foliage commits the reward block's hash, is_transaction_block is consistent,
// and the reward block's hash is stable across rebuilds.
#[test]
fn genesis_unfinished_block_invariants_match_chia() {
    let cc_sp_vdf = mk_vdf(0x10, 1_000);
    let rc_sp_vdf = mk_vdf(0x11, 2_000);
    let total_iters: u128 = 123_456;
    let sp_index: u8 = 7;
    let cc_challenge = ph(0x20);
    // Pre-resolved signage-point message hashes (cc_sp_hash / rc_sp_hash). Distinct so the
    // marker signer can prove each landed in the correct reward-block slot.
    let cc_sp_hash = ph(0x30);
    let rc_sp_hash = ph(0x31);

    let build = || {
        create_unfinished_block(
            &MAINNET,
            total_iters,
            sp_index,
            mk_pos(),
            cc_challenge,
            Some(cc_sp_vdf),
            Some(mk_vdf_proof(1)),
            Some(rc_sp_vdf),
            Some(mk_vdf_proof(2)),
            cc_sp_hash,
            rc_sp_hash,
            Vec::new(),                // finished_sub_slots
            0,                         // height (genesis)
            true,                      // is_transaction_block
            &[],                       // no reward claims at height 0
            None,                      // no spends
            MAINNET.genesis_challenge, // prev_block_hash
            MAINNET.genesis_challenge, // prev_transaction_block_hash
            pool_target(),
            None,
            ph(0xDD),
            1_600_000_000,
            b"genesis-unfinished",
            marker_signer,
        )
    };
    let ub = build().expect("genesis unfinished block builds");

    let rc = &ub.reward_chain_block;
    // total_iters / signage_point_index / pos challenge propagate.
    assert_eq!(rc.total_iters, total_iters);
    assert_eq!(rc.signage_point_index, sp_index);
    assert_eq!(rc.pos_ss_cc_challenge_hash, cc_challenge);
    assert_eq!(rc.proof_of_space, mk_pos());
    // signage-point VDFs propagate into the reward block.
    assert_eq!(rc.challenge_chain_sp_vdf, Some(cc_sp_vdf));
    assert_eq!(rc.reward_chain_sp_vdf, Some(rc_sp_vdf));
    // real signing: the marker signer proves cc_sp_hash was signed into the cc slot and
    // rc_sp_hash into the rc slot (not the zero placeholder, and not swapped).
    assert_eq!(
        rc.challenge_chain_sp_signature,
        marker_signer(cc_sp_hash, &rc.proof_of_space.plot_public_key),
        "cc slot signs cc_sp_hash"
    );
    assert_eq!(
        rc.reward_chain_sp_signature,
        marker_signer(rc_sp_hash, &rc.proof_of_space.plot_public_key),
        "rc slot signs rc_sp_hash"
    );
    assert_ne!(rc.challenge_chain_sp_signature, super::Bytes96::default());
    assert_ne!(rc.reward_chain_sp_signature, super::Bytes96::default());
    assert_ne!(
        rc.challenge_chain_sp_signature, rc.reward_chain_sp_signature,
        "cc and rc signatures are over different hashes"
    );

    // signage-point VDF proofs carried into the UnfinishedBlock unchanged.
    assert_eq!(ub.challenge_chain_sp_proof, Some(mk_vdf_proof(1)));
    assert_eq!(ub.reward_chain_sp_proof, Some(mk_vdf_proof(2)));
    assert!(ub.finished_sub_slots.is_empty());

    // foliage.reward_block_hash == reward_chain_block.hash().
    let rc_hash = rc.hash().expect("rc hash");
    assert_eq!(ub.foliage.reward_block_hash, rc_hash);

    // is_transaction_block consistency: ftb present <=> tx_info present (and both present at genesis).
    assert_eq!(
        ub.foliage_transaction_block.is_some(),
        ub.transactions_info.is_some()
    );
    assert!(ub.foliage_transaction_block.is_some());
    assert!(ub.transactions_info.is_some());
    assert_eq!(
        ub.foliage.foliage_transaction_block_hash.is_some(),
        ub.foliage_transaction_block.is_some()
    );

    // no spend generator => transactions_generator None; the ref list is a bare empty Vec.
    assert!(ub.transactions_generator.is_none());
    assert_eq!(ub.transactions_generator_ref_list, Vec::<u32>::new());

    // partial_hash stability: identical inputs => identical reward-block hash and identical block.
    let ub2 = build().expect("rebuild");
    assert_eq!(
        ub2.reward_chain_block.hash().unwrap(),
        rc_hash,
        "partial_hash stable"
    );
    assert_eq!(ub2, ub, "identical inputs => identical UnfinishedBlock");
}

// A non-transaction block: no foliage_transaction_block, no transactions_info, and a first-signage-
// point reward block (all sp VDFs / proofs None). Foliage still commits the reward block's hash.
#[test]
fn non_transaction_unfinished_block_has_no_tx_members() {
    let ub = create_unfinished_block(
        &MAINNET,
        999,
        3,
        mk_pos(),
        ph(0x20),
        None,     // no cc sp vdf (first sp of a sub-slot)
        None,     // no cc sp proof
        None,     // no rc sp vdf
        None,     // no rc sp proof
        ph(0x30), // cc_sp_hash
        ph(0x31), // rc_sp_hash
        Vec::new(),
        10,    // height
        false, // NOT a transaction block
        &[],
        None,
        ph(0xBB),
        ph(0xCC),
        pool_target(),
        None,
        ph(0xDD),
        123,
        b"nontx",
        marker_signer,
    )
    .expect("non-tx unfinished block builds");

    assert!(ub.foliage_transaction_block.is_none());
    assert!(ub.transactions_info.is_none());
    assert_eq!(
        ub.foliage_transaction_block.is_some(),
        ub.transactions_info.is_some()
    );
    assert!(ub.foliage.foliage_transaction_block_hash.is_none());
    assert!(ub.reward_chain_block.challenge_chain_sp_vdf.is_none());
    assert!(ub.reward_chain_block.reward_chain_sp_vdf.is_none());
    assert!(ub.challenge_chain_sp_proof.is_none());
    assert!(ub.reward_chain_sp_proof.is_none());
    assert_eq!(
        ub.foliage.reward_block_hash,
        ub.reward_chain_block.hash().unwrap()
    );
    assert!(ub.transactions_generator.is_none());
    assert_eq!(ub.transactions_generator_ref_list, Vec::<u32>::new());
}

// A transaction block carrying a spend generator: transactions_generator and the ref list propagate
// from the BlockTransactions payload.
#[test]
fn transaction_generator_and_ref_list_propagate() {
    let removed = Coin {
        parent_coin_info: ph(0x55),
        puzzle_hash: ph(0x66),
        amount: 1_000,
    };
    let created = Coin {
        parent_coin_info: removed.name(),
        puzzle_hash: ph(0x77),
        amount: 900,
    };
    let program = SerializedProgram::from_bytes(&[0x80]);
    let tx = BlockTransactions {
        program: program.clone(),
        block_refs: vec![3, 5, 8],
        additions: vec![created],
        removals: vec![removed],
        aggregated_signature: g2_infinity(),
        cost: 42,
    };
    let ub = create_unfinished_block(
        &MAINNET,
        777,
        2,
        mk_pos(),
        ph(0x20),
        Some(mk_vdf(0x10, 500)),
        Some(mk_vdf_proof(1)),
        Some(mk_vdf(0x11, 600)),
        Some(mk_vdf_proof(2)),
        ph(0x30), // cc_sp_hash
        ph(0x31), // rc_sp_hash
        Vec::new(),
        101,  // height (> 0)
        true, // transaction block
        &[],  // isolate the spend path from reward claims
        Some(&tx),
        ph(0xBB),
        ph(0xCC),
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"spend-unfinished",
        // non-empty byte_array_tx (a removal) => the real chia_block_filter runs; its filter_hash
        // value is not asserted here (this test isolates generator / ref-list propagation).
        marker_signer,
    )
    .expect("spend unfinished block builds");

    assert_eq!(ub.transactions_generator, Some(program));
    assert_eq!(ub.transactions_generator_ref_list, vec![3, 5, 8]);
    assert!(ub.foliage_transaction_block.is_some());
    assert!(ub.transactions_info.is_some());
    assert_eq!(ub.transactions_info.as_ref().unwrap().fees, 100);
    assert_eq!(
        ub.foliage.reward_block_hash,
        ub.reward_chain_block.hash().unwrap()
    );
}

// END-TO-END REAL SIGNING: build a genesis UnfinishedBlock with a real plot key and the
// AugScheme signer, then assert ALL FOUR plot signatures verify against the proof-of-space's
// plot public key.
#[test]
fn all_four_plot_signatures_verify_end_to_end() {
    let sk = plot_sk(0x7E);
    let plot_pk = plot_pk_bytes(&sk);
    let cc_sp_hash = ph(0x30);
    let rc_sp_hash = ph(0x31);

    let ub = create_unfinished_block(
        &MAINNET,
        123_456,
        7,
        mk_pos_with_key(plot_pk), // real plot public key in the proof of space
        ph(0x20),
        Some(mk_vdf(0x10, 1_000)),
        Some(mk_vdf_proof(1)),
        Some(mk_vdf(0x11, 2_000)),
        Some(mk_vdf_proof(2)),
        cc_sp_hash,
        rc_sp_hash,
        Vec::new(),
        0,    // genesis height
        true, // transaction block
        &[],
        None,
        MAINNET.genesis_challenge,
        MAINNET.genesis_challenge,
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"genesis-signed",
        real_signer(&sk),
    )
    .expect("signed genesis unfinished block builds");

    let rc = &ub.reward_chain_block;
    // 1 + 2: the two signage-point signatures verify over their pre-resolved hashes.
    assert!(
        verifies(&plot_pk, cc_sp_hash, &rc.challenge_chain_sp_signature),
        "challenge_chain_sp_signature verifies"
    );
    assert!(
        verifies(&plot_pk, rc_sp_hash, &rc.reward_chain_sp_signature),
        "reward_chain_sp_signature verifies"
    );

    // 3: foliage_block_data_signature verifies over foliage_data.get_hash().
    assert!(
        verifies(
            &plot_pk,
            ub.foliage.foliage_block_data.hash().unwrap(),
            &ub.foliage.foliage_block_data_signature,
        ),
        "foliage_block_data_signature verifies"
    );

    // 4: foliage_transaction_block_signature verifies over foliage_transaction_block.get_hash().
    let ftb = ub
        .foliage_transaction_block
        .as_ref()
        .expect("genesis ftb present");
    let ftb_sig = ub
        .foliage
        .foliage_transaction_block_signature
        .expect("ftb signed");
    assert!(
        verifies(&plot_pk, ftb.hash().unwrap(), &ftb_sig),
        "foliage_transaction_block_signature verifies"
    );

    // The (ftb_hash Some) == (ftb_sig Some) invariant still holds under real signing.
    assert_eq!(
        ub.foliage.foliage_transaction_block_hash.is_some(),
        ub.foliage.foliage_transaction_block_signature.is_some()
    );

    // None of the four are the zero placeholder.
    for sig in [
        &rc.challenge_chain_sp_signature,
        &rc.reward_chain_sp_signature,
        &ub.foliage.foliage_block_data_signature,
        &ftb_sig,
    ] {
        assert_ne!(*sig, Bytes96::default(), "no zero-placeholder signatures");
    }

    // WRONG KEY: the sp signature must not verify against a different plot public key.
    let wrong_pk = plot_pk_bytes(&plot_sk(0xE7));
    assert!(
        !verifies(&wrong_pk, cc_sp_hash, &rc.challenge_chain_sp_signature),
        "cc_sp_signature must fail against the wrong plot key"
    );
}
