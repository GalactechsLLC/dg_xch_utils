use super::{
    BlockTransactions, FarmerSignatures, calculate_infusion_point_total_iters,
    create_unfinished_block, create_unfinished_block_with_sigs, g2_infinity,
    splice_farmer_foliage_signatures,
};
use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::coin::Coin;
use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use crate::clvm::bls_bindings::{sign, verify_signature};
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
fn verifies(plot_pk: &Bytes48, msg: Bytes32, sig: &Bytes96) -> bool {
    let pk: PublicKey = plot_pk.into();
    let Ok(sig) = Signature::try_from(sig) else {
        return false;
    };
    verify_signature(&pk, msg.as_ref(), &sig)
}
fn mk_pos(plot_public_key: Bytes48) -> ProofOfSpace {
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
fn mk_vdf(challenge: u8, iters: u64) -> VdfInfo {
    VdfInfo {
        challenge: ph(challenge),
        number_of_iterations: iters,
        output: ClassgroupElement::get_default_element(),
    }
}
fn mk_vdf_proof(w: u8) -> VdfProof {
    VdfProof {
        witness_type: w,
        witness: UnsizedBytes::new(vec![0xAA, 0xBB]),
        normalized_to_identity: true,
    }
}
fn pool_target() -> PoolTarget {
    PoolTarget {
        puzzle_hash: ph(0x01),
        max_height: 0,
    }
}

// overflow (sp_iters > ip_iters) adds one sub_slot_iters
#[test]
fn infusion_point_total_iters_overflow_math_matches_chia() {
    // Non-overflow: sp_iters <= ip_iters => start + ip_iters.
    assert_eq!(
        calculate_infusion_point_total_iters(1_000, 10, 20, 100_000),
        1_020
    );
    // Overflow: sp_iters > ip_iters => start + ip_iters + sub_slot_iters.
    assert_eq!(
        calculate_infusion_point_total_iters(1_000, 30, 20, 100_000),
        101_020
    );
    // Boundary: sp_iters == ip_iters is NOT overflow (strict >).
    assert_eq!(calculate_infusion_point_total_iters(0, 20, 20, 100_000), 20);
}

// THE ASSEMBLY TEST. Given a candidate's inputs + SP VDFs + farmer signatures, the produced
// UnfinishedBlock is well-formed: the SP signatures from the declare message land in the reward
// block, the foliage carries the farmer's foliage signatures (verifying against the plot key), the
// foliage↔reward-block-hash tie holds, and the tx-block invariants hold.
#[test]
fn with_sigs_assembles_wellformed_block_carrying_farmer_sigs() {
    let sk = plot_sk(0x5A);
    let plot_pk = plot_pk_bytes(&sk);
    let pos = mk_pos(plot_pk);
    let cc_sp_vdf = mk_vdf(0x10, 1_000);
    let rc_sp_vdf = mk_vdf(0x11, 2_000);

    // Genesis-shaped: height 0, transaction block, no claims/spends.
    // The four farmer signatures — cc/rc "from the declare message", foliage "from SignedValues".
    // We produce cc/rc as real AugScheme signatures so verifies() can confirm them, but the producer
    // treats them as opaque bytes (it does no signing). The foliage sigs are declare-time placeholders.
    let farmer_sigs = FarmerSignatures {
        challenge_chain_sp_signature: sign(&sk, ph(0x30).as_ref()).into(),
        reward_chain_sp_signature: sign(&sk, ph(0x31).as_ref()).into(),
        foliage_block_data_signature: g2_infinity(), // placeholder at declare
        foliage_transaction_block_signature: g2_infinity(),
    };

    let ub = create_unfinished_block_with_sigs(
        &MAINNET,
        123_456,
        7,
        pos.clone(),
        ph(0x20),
        Some(cc_sp_vdf),
        Some(mk_vdf_proof(1)),
        Some(rc_sp_vdf),
        Some(mk_vdf_proof(2)),
        Vec::new(),
        0,
        true,
        &[],
        None,
        MAINNET.genesis_challenge,
        MAINNET.genesis_challenge,
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"emit-seed",
        farmer_sigs,
    )
    .expect("with_sigs builds");

    let rc = &ub.reward_chain_block;
    // SP signatures from the declare message landed verbatim in the reward block.
    assert_eq!(
        rc.challenge_chain_sp_signature,
        farmer_sigs.challenge_chain_sp_signature
    );
    assert_eq!(
        rc.reward_chain_sp_signature,
        farmer_sigs.reward_chain_sp_signature
    );
    assert_eq!(rc.total_iters, 123_456);
    assert_eq!(rc.signage_point_index, 7);
    assert_eq!(rc.challenge_chain_sp_vdf, Some(cc_sp_vdf));
    assert_eq!(rc.reward_chain_sp_vdf, Some(rc_sp_vdf));

    // Placeholder foliage signatures at declare time — the infinity placeholder, NOT zeros.
    assert_eq!(ub.foliage.foliage_block_data_signature, g2_infinity());
    assert_ne!(ub.foliage.foliage_block_data_signature, Bytes96::default());
    assert_eq!(
        ub.foliage.foliage_transaction_block_signature,
        Some(g2_infinity()),
        "tx block carries a (placeholder) ftb signature"
    );

    // foliage.reward_block_hash == reward_chain_block.hash().
    let rc_hash = rc.hash().expect("rc hash");
    assert_eq!(ub.foliage.reward_block_hash, rc_hash);
    // tx-block consistency.
    assert!(ub.foliage_transaction_block.is_some());
    assert!(ub.transactions_info.is_some());
    assert!(ub.foliage.foliage_transaction_block_hash.is_some());
}

// THE SPLICE. A placeholder candidate + a SignedValues reply => the real foliage signatures land in
// the foliage and verify against the plot key; the ftb signature is spliced for a tx block.
#[test]
fn splice_replaces_placeholder_foliage_sigs_and_verifies() {
    let sk = plot_sk(0x77);
    let plot_pk = plot_pk_bytes(&sk);
    let pos = mk_pos(plot_pk);
    let farmer_sigs = FarmerSignatures {
        challenge_chain_sp_signature: g2_infinity(),
        reward_chain_sp_signature: g2_infinity(),
        foliage_block_data_signature: g2_infinity(),
        foliage_transaction_block_signature: g2_infinity(),
    };
    let mut candidate = create_unfinished_block_with_sigs(
        &MAINNET,
        10,
        0,
        pos,
        MAINNET.genesis_challenge,
        None,
        None,
        None,
        None,
        Vec::new(),
        0,
        true,
        &[],
        None,
        MAINNET.genesis_challenge,
        MAINNET.genesis_challenge,
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"splice-seed",
        farmer_sigs,
    )
    .expect("candidate builds");

    // The two hashes the farmer signs (the RequestSignedValues payload).
    let fbd_hash = candidate
        .foliage
        .foliage_block_data
        .hash()
        .expect("fbd hash");
    let ftb_hash = candidate
        .foliage
        .foliage_transaction_block_hash
        .expect("tx block => ftb hash");
    // The farmer's real signatures over those hashes (SignedValues).
    let real_fbd: Bytes96 = sign(&sk, fbd_hash.as_ref()).into();
    let real_ftb: Bytes96 = sign(&sk, ftb_hash.as_ref()).into();

    // Pre-splice: placeholders.
    assert_eq!(
        candidate.foliage.foliage_block_data_signature,
        g2_infinity()
    );

    splice_farmer_foliage_signatures(&mut candidate, real_fbd, real_ftb);

    assert_eq!(candidate.foliage.foliage_block_data_signature, real_fbd);
    assert_eq!(
        candidate.foliage.foliage_transaction_block_signature,
        Some(real_ftb)
    );
    // Both spliced signatures verify against the plot key over the hashes the farmer was given.
    assert!(verifies(&plot_pk, fbd_hash, &real_fbd));
    assert!(verifies(&plot_pk, ftb_hash, &real_ftb));
}

// A non-transaction candidate has no ftb slot: the splice overwrites fbd but leaves ftb None.
#[test]
fn splice_leaves_ftb_none_for_non_transaction_block() {
    let sk = plot_sk(0x33);
    let pos = mk_pos(plot_pk_bytes(&sk));
    let farmer_sigs = FarmerSignatures {
        challenge_chain_sp_signature: g2_infinity(),
        reward_chain_sp_signature: g2_infinity(),
        foliage_block_data_signature: g2_infinity(),
        foliage_transaction_block_signature: g2_infinity(),
    };
    let mut candidate = create_unfinished_block_with_sigs(
        &MAINNET,
        10,
        3,
        pos,
        ph(0x20),
        Some(mk_vdf(0x10, 1)),
        Some(mk_vdf_proof(1)),
        Some(mk_vdf(0x11, 2)),
        Some(mk_vdf_proof(2)),
        Vec::new(),
        10,
        false, // NOT a transaction block
        &[],
        None,
        ph(0xBB),
        ph(0xCC),
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"nontx-splice",
        farmer_sigs,
    )
    .expect("non-tx candidate builds");
    assert!(candidate.foliage.foliage_transaction_block_hash.is_none());

    let fbd_hash = candidate
        .foliage
        .foliage_block_data
        .hash()
        .expect("fbd hash");
    let real_fbd: Bytes96 = sign(&sk, fbd_hash.as_ref()).into();
    splice_farmer_foliage_signatures(&mut candidate, real_fbd, Bytes96::new([0xEE; 96]));

    assert_eq!(candidate.foliage.foliage_block_data_signature, real_fbd);
    assert_eq!(
        candidate.foliage.foliage_transaction_block_signature, None,
        "non-tx block keeps ftb signature None regardless of the supplied value"
    );
}

// EQUIVALENCE: the farmer-supplied path produces the SAME UnfinishedBlock the signer
// path would, when the signer is fed exactly the four signatures — the same bytes go on
// the wire either way.
#[test]
fn with_sigs_equals_signer_path_for_the_same_four_signatures() {
    let sk = plot_sk(0x9C);
    let plot_pk = plot_pk_bytes(&sk);
    let pos = mk_pos(plot_pk);
    let cc_sp_hash = ph(0x40);
    let rc_sp_hash = ph(0x41);

    // A spend so the tx-block path (ftb hash + signature) is exercised.
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
    let tx = BlockTransactions {
        program: crate::clvm::program::SerializedProgram::from_bytes(&[0x80]),
        block_refs: Vec::new(),
        additions: vec![created],
        removals: vec![removed],
        aggregated_signature: g2_infinity(),
        cost: 42,
    };

    // The signer path signs each hash with sk (AugScheme). Build it FIRST so we can read the exact
    // four signatures it produced for the four hashes, then feed those into the with_sigs path.
    let signer_block = create_unfinished_block(
        &MAINNET,
        999,
        5,
        pos.clone(),
        ph(0x20),
        Some(mk_vdf(0x10, 1_000)),
        Some(mk_vdf_proof(1)),
        Some(mk_vdf(0x11, 2_000)),
        Some(mk_vdf_proof(2)),
        cc_sp_hash,
        rc_sp_hash,
        Vec::new(),
        101,
        true,
        &[],
        Some(&tx),
        ph(0xBB),
        ph(0xCC),
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"equiv-seed",
        |msg: Bytes32, _pk: &Bytes48| -> Bytes96 { sign(&sk, msg.as_ref()).into() },
    )
    .expect("signer path builds");

    // The four signatures the signer produced: cc over cc_sp_hash, rc over rc_sp_hash, fbd over the
    // foliage_block_data hash, ftb over the foliage_transaction_block hash.
    let fbd_hash = signer_block
        .foliage
        .foliage_block_data
        .hash()
        .expect("fbd hash");
    let ftb_hash = signer_block
        .foliage
        .foliage_transaction_block_hash
        .expect("ftb hash");
    let farmer_sigs = FarmerSignatures {
        challenge_chain_sp_signature: sign(&sk, cc_sp_hash.as_ref()).into(),
        reward_chain_sp_signature: sign(&sk, rc_sp_hash.as_ref()).into(),
        foliage_block_data_signature: sign(&sk, fbd_hash.as_ref()).into(),
        foliage_transaction_block_signature: sign(&sk, ftb_hash.as_ref()).into(),
    };

    let with_sigs_block = create_unfinished_block_with_sigs(
        &MAINNET,
        999,
        5,
        pos,
        ph(0x20),
        Some(mk_vdf(0x10, 1_000)),
        Some(mk_vdf_proof(1)),
        Some(mk_vdf(0x11, 2_000)),
        Some(mk_vdf_proof(2)),
        Vec::new(),
        101,
        true,
        &[],
        Some(&tx),
        ph(0xBB),
        ph(0xCC),
        pool_target(),
        None,
        ph(0xDD),
        1_600_000_000,
        b"equiv-seed",
        farmer_sigs,
    )
    .expect("with_sigs path builds");

    assert_eq!(
        signer_block, with_sigs_block,
        "farmer-supplied path must be byte-identical to the signer path for the same signatures"
    );
}
