use super::{BlockTransactions, RewardBlockClaim, compute_block_fee, create_foliage, g2_infinity};
use crate::blockchain::coin::Coin;
use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::clvm::bls_bindings::{sign, verify_signature};
use crate::consensus::block_generator::canonical_additions_root;
use crate::consensus::block_rewards::{calculate_base_farmer_reward, calculate_pool_reward};
use crate::consensus::coinbase::{create_farmer_coin, create_pool_coin};
use crate::consensus::constants::MAINNET;
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use blst::min_pk::{PublicKey, SecretKey, Signature};

fn ph(byte: u8) -> Bytes32 {
    Bytes32::new([byte; 32])
}

/// A deterministic plot secret key for tests. The plot
/// public key `sk.sk_to_pk()` is the G1 handed to the signer and verified against — the single-key
/// straightforward AugScheme case (taproot/pool aggregation is a farmer/harvester concern; see the
/// module note).
fn plot_sk(seed: u8) -> SecretKey {
    SecretKey::key_gen_v3(&[seed; 32], &[]).expect("deterministic plot sk")
}

/// The plot public key as the wire `Bytes48` (`ProofOfSpace.plot_public_key`).
fn plot_pk_bytes(sk: &SecretKey) -> Bytes48 {
    sk.sk_to_pk().into()
}

/// A real AugScheme plot signer closure: sign the
/// 32-byte message with `sk`, prepending `sk`'s own public key (AugScheme), returning the G2
/// signature as `Bytes96`. `sign` (bls_bindings) uses `sk_to_pk()` as the prepend, so the result
/// verifies against `plot_pk_bytes(sk)` under [`verify_signature`].
fn real_signer(sk: &SecretKey) -> impl Fn(Bytes32, &Bytes48) -> Bytes96 + '_ {
    move |msg: Bytes32, _plot_pk: &Bytes48| -> Bytes96 { sign(sk, msg.as_ref()).into() }
}

/// Verify a `Bytes96` plot signature over `msg` against a `Bytes48` plot public key under AugScheme.
fn verifies(plot_pk: &Bytes48, msg: Bytes32, sig: &Bytes96) -> bool {
    let pk: PublicKey = plot_pk.into();
    let Ok(sig) = Signature::try_from(sig) else {
        return false;
    };
    verify_signature(&pk, msg.as_ref(), &sig)
}

// fee = sum(removals) - sum(additions); negative (minting) is an error.
#[test]
fn compute_block_fee_matches_chia_vectors() {
    let mk = |amts: &[u64]| -> Vec<Coin> {
        amts.iter()
            .enumerate()
            .map(|(i, a)| Coin {
                parent_coin_info: ph(i as u8),
                puzzle_hash: ph(0xF0 | i as u8),
                amount: *a,
            })
            .collect()
    };
    let add_cases: Vec<Vec<u64>> = vec![vec![0], vec![1, 2, 3], vec![]];
    let rem_cases: Vec<Vec<u64>> = vec![vec![0], vec![1, 2, 3], vec![]];
    for add in &add_cases {
        for rem in &rem_cases {
            let additions = mk(add);
            let removals = mk(rem);
            let add_sum: i128 = add.iter().map(|a| i128::from(*a)).sum();
            let rem_sum: i128 = rem.iter().map(|a| i128::from(*a)).sum();
            let expected = rem_sum - add_sum;
            let got = compute_block_fee(&additions, &removals);
            if expected < 0 {
                assert!(got.is_err(), "add={add:?} rem={rem:?} must error (minting)");
            } else {
                assert_eq!(
                    got.expect("non-minting fee"),
                    expected as u64,
                    "add={add:?} rem={rem:?}"
                );
            }
        }
    }
}

// INVARIANT: parent ids = genesis_challenge half ++ 16-byte big-endian height.
// Since block_height is uint32, the 16-byte height is 12 zero bytes then the 4-byte height, so the
// parent id is 16 challenge bytes ++ 12 zero bytes ++ height.to_be_bytes(). pool uses [:16],
// farmer uses [16:]; the two must differ, and rebuilding must be deterministic.
#[test]
fn reward_coin_parent_ids_compose_from_genesis_challenge_and_height() {
    let gc = MAINNET.genesis_challenge;
    let height = 0x00AB_CDEFu32;
    let pool = create_pool_coin(height, ph(0x11), 1, gc);
    let farmer = create_farmer_coin(height, ph(0x22), 1, gc);

    let pool_pid = pool.parent_coin_info.bytes();
    let farmer_pid = farmer.parent_coin_info.bytes();
    assert_eq!(
        &pool_pid[0..16],
        &gc.bytes()[0..16],
        "pool prefix = challenge[:16]"
    );
    assert_eq!(
        &farmer_pid[0..16],
        &gc.bytes()[16..32],
        "farmer prefix = challenge[16:]"
    );
    assert_eq!(
        &pool_pid[16..28],
        &[0u8; 12],
        "12 zero bytes of the 16-byte BE height"
    );
    assert_eq!(
        &pool_pid[28..32],
        &height.to_be_bytes(),
        "height in the low 4 bytes"
    );
    assert_eq!(&farmer_pid[28..32], &height.to_be_bytes());
    assert_ne!(pool_pid, farmer_pid, "pool and farmer parent ids differ");
    // deterministic
    assert_eq!(create_pool_coin(height, ph(0x11), 1, gc), pool);
}

// A non-transaction block returns (Foliage, None, None) and no foliage_transaction fields.
#[test]
fn non_transaction_block_has_no_foliage_transaction_block() {
    let pool_target = PoolTarget {
        puzzle_hash: ph(0x01),
        max_height: 0,
    };
    let sk = plot_sk(0x11);
    let res = create_foliage(
        &MAINNET,
        ph(0xAA), // reward_block_unfinished_hash
        10,       // height
        false,    // is_transaction_block
        &[],
        None,
        ph(0xBB), // prev_block_hash
        ph(0xCC), // prev_transaction_block_hash (unused here)
        pool_target,
        None,               // pool_signature
        plot_pk_bytes(&sk), // plot_public_key
        ph(0xDD),           // farmer_reward_puzzle_hash
        123,                // timestamp
        b"seed-nontx",
        real_signer(&sk),
    )
    .expect("foliage builds");
    assert!(res.foliage_transaction_block.is_none());
    assert!(res.transactions_info.is_none());
    assert!(res.foliage.foliage_transaction_block_hash.is_none());
    assert!(res.foliage.foliage_transaction_block_signature.is_none());
    // foliage_block_data_signature is real (non-placeholder) and verifies against the plot key.
    assert_ne!(
        res.foliage.foliage_block_data_signature,
        Bytes96::default(),
        "signed, not the zero placeholder"
    );
    assert!(verifies(
        &plot_pk_bytes(&sk),
        res.foliage.foliage_block_data.hash().unwrap(),
        &res.foliage.foliage_block_data_signature,
    ));
    assert_eq!(res.foliage.reward_block_hash, ph(0xAA));
    assert_eq!(res.foliage.prev_block_hash, ph(0xBB));
    assert_eq!(
        res.foliage.foliage_block_data.farmer_reward_puzzle_hash,
        ph(0xDD)
    );
}

// The genesis transaction block: height 0, no reward claims, no spends. TransactionsInfo has
// generator_hash = zeros, generator_refs_hash = [1;32], signature = infinity (0xc0..),
// fees = 0, cost = 0, empty reward_claims_incorporated. The empty addition/removal merkle
// sets are all-zeros, and the empty BIP158 filter is [0] so filter_hash = sha256([0]).
#[test]
fn genesis_transaction_block_defaults_match_chia() {
    let pool_target = PoolTarget {
        puzzle_hash: ph(0x01),
        max_height: 0,
    };
    let sk = plot_sk(0x22);
    let res = create_foliage(
        &MAINNET,
        ph(0xAA),
        0,                         // genesis height
        true,                      // genesis is a transaction block
        &[],                       // no reward claims at height 0
        None,                      // no spends
        MAINNET.genesis_challenge, // prev_block_hash
        MAINNET.genesis_challenge, // prev_transaction_block_hash
        pool_target,
        None,               // pool_signature
        plot_pk_bytes(&sk), // plot_public_key
        ph(0xDD),
        1_600_000_000,
        b"genesis-seed",
        real_signer(&sk),
    )
    .expect("genesis foliage builds");

    let info = res.transactions_info.expect("genesis tx info");
    assert_eq!(
        info.generator_root,
        Bytes32::default(),
        "no generator => zeros"
    );
    assert_eq!(
        info.generator_refs_root,
        Bytes32::new([1u8; 32]),
        "empty refs => [1;32]"
    );
    assert_eq!(
        info.aggregated_signature,
        g2_infinity(),
        "empty sig => G2 infinity 0xc0.."
    );
    assert_ne!(
        info.aggregated_signature,
        Bytes96::default(),
        "must NOT be all-zeros Bytes96"
    );
    assert_eq!(info.fees, 0);
    assert_eq!(info.cost, 0);
    assert!(info.reward_claims_incorporated.is_empty());

    let ftb = res.foliage_transaction_block.expect("genesis ftb");
    assert_eq!(
        ftb.additions_root,
        Bytes32::default(),
        "empty additions merkle => zeros"
    );
    assert_eq!(
        ftb.removals_root,
        Bytes32::default(),
        "empty removals merkle => zeros"
    );
    assert_eq!(
        ftb.filter_hash,
        Bytes32::new(hash_256(vec![0u8])),
        "empty BIP158 filter [0] => sha256([0])"
    );
    assert_eq!(ftb.prev_transaction_block_hash, MAINNET.genesis_challenge);
    assert_eq!(
        res.foliage.foliage_transaction_block_hash,
        Some(ftb.hash().unwrap())
    );
    // Both foliage plot signatures are real and verify against the plot key under AugScheme.
    let ftb_sig = res
        .foliage
        .foliage_transaction_block_signature
        .expect("ftb signed");
    assert!(verifies(&plot_pk_bytes(&sk), ftb.hash().unwrap(), &ftb_sig));
    assert!(verifies(
        &plot_pk_bytes(&sk),
        res.foliage.foliage_block_data.hash().unwrap(),
        &res.foliage.foliage_block_data_signature,
    ));
}

// INVARIANT: a transaction block above genesis mints the reward coins from its claims. Pool amount
// == calculate_pool_reward(height); farmer amount == calculate_base_farmer_reward(height) + fees;
// reward_claims_incorporated preserves [pool, farmer] order; and those coins are exactly the block's
// additions, so additions_root == canonical_additions_root([pool, farmer]).
#[test]
fn reward_claims_mint_pool_and_farmer_coins() {
    let claim = RewardBlockClaim {
        height: 100,
        pool_puzzle_hash: ph(0x33),
        farmer_puzzle_hash: ph(0x44),
        fees: 555,
    };
    let pool_target = PoolTarget {
        puzzle_hash: ph(0x01),
        max_height: 0,
    };
    let sk = plot_sk(0x33);
    let res = create_foliage(
        &MAINNET,
        ph(0xAA),
        101,  // height (> 0 so claims are incorporated)
        true, // transaction block
        std::slice::from_ref(&claim),
        None,
        ph(0xBB),
        ph(0xCC),
        pool_target,
        None,               // pool_signature
        plot_pk_bytes(&sk), // plot_public_key
        ph(0xDD),
        1_600_000_000,
        b"reward-seed",
        // Two reward puzzle hashes => a non-empty filter (real BIP158 now); this test only
        // exercises the coin/roots invariants, not the filter_hash value.
        real_signer(&sk),
    )
    .expect("reward foliage builds");

    let info = res.transactions_info.expect("tx info");
    assert_eq!(info.reward_claims_incorporated.len(), 2, "pool + farmer");
    let expected_pool = create_pool_coin(
        claim.height,
        claim.pool_puzzle_hash,
        calculate_pool_reward(claim.height),
        MAINNET.genesis_challenge,
    );
    let expected_farmer = create_farmer_coin(
        claim.height,
        claim.farmer_puzzle_hash,
        calculate_base_farmer_reward(claim.height) + claim.fees,
        MAINNET.genesis_challenge,
    );
    assert_eq!(
        info.reward_claims_incorporated[0], expected_pool,
        "pool first"
    );
    assert_eq!(
        info.reward_claims_incorporated[1], expected_farmer,
        "farmer second"
    );
    assert_eq!(
        info.reward_claims_incorporated[0].amount,
        calculate_pool_reward(claim.height)
    );
    assert_eq!(
        info.reward_claims_incorporated[1].amount,
        calculate_base_farmer_reward(claim.height) + claim.fees
    );

    let ftb = res.foliage_transaction_block.expect("ftb");
    let expected_root = canonical_additions_root(&[expected_pool, expected_farmer]).expect("root");
    assert_eq!(
        ftb.additions_root, expected_root,
        "additions are exactly the reward coins"
    );
    assert_eq!(
        ftb.removals_root,
        Bytes32::default(),
        "no removals => empty merkle"
    );
}

// A block with spends: fees flow from compute_block_fee(additions, removals) into TransactionsInfo,
// and removals appear in removals_root. (No CLVM generator needed for these foliage invariants.)
#[test]
fn transaction_block_fee_and_removals_flow_into_info() {
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
    let pool_target = PoolTarget {
        puzzle_hash: ph(0x01),
        max_height: 0,
    };
    let sk = plot_sk(0x44);
    let res = create_foliage(
        &MAINNET,
        ph(0xAA),
        101,
        true,
        &[], // isolate the spend path from reward claims
        Some(&tx),
        ph(0xBB),
        ph(0xCC),
        pool_target,
        None,               // pool_signature
        plot_pk_bytes(&sk), // plot_public_key
        ph(0xDD),
        1_600_000_000,
        b"spend-seed",
        real_signer(&sk),
    )
    .expect("spend foliage builds");
    let info = res.transactions_info.expect("tx info");
    assert_eq!(info.fees, 100, "1000 removed - 900 created");
    assert_eq!(info.cost, 42);
    let ftb = res.foliage_transaction_block.expect("ftb");
    assert_ne!(
        ftb.removals_root,
        Bytes32::default(),
        "one removal => non-empty merkle"
    );
}

// THE ROUND TRIP. Four hashes are plot-signed per block. Prove the BLS layer
// (bls_bindings::sign / verify_signature, AugScheme DST) closes that loop: sign each of the
// four message hashes with a fixed-seed plot key and assert every signature VERIFIES against
// the plot public key; then assert a signature made with the WRONG key FAILS.
#[test]
fn plot_signatures_round_trip_and_wrong_key_fails() {
    let sk = plot_sk(0x5A);
    let plot_pk = plot_pk_bytes(&sk);
    let signer = real_signer(&sk);

    // The four signing-point messages, standing in for foliage_data.get_hash(),
    // foliage_transaction_block.get_hash(), cc_sp_hash, rc_sp_hash.
    let messages = [
        Bytes32::new(hash_256(b"foliage_block_data")),
        Bytes32::new(hash_256(b"foliage_transaction_block")),
        Bytes32::new(hash_256(b"cc_sp_hash")),
        Bytes32::new(hash_256(b"rc_sp_hash")),
    ];

    for (i, msg) in messages.iter().copied().enumerate() {
        let sig = signer(msg, &plot_pk);
        assert_ne!(
            sig,
            Bytes96::default(),
            "message {i}: real signature, not the zero placeholder"
        );
        assert!(
            verifies(&plot_pk, msg, &sig),
            "message {i}: signature must verify against the plot public key (AugScheme)"
        );
    }

    // WRONG KEY: a signature over the same message from a different plot key must NOT verify against
    // the original plot public key.
    let wrong_sk = plot_sk(0xA5);
    let wrong_pk = plot_pk_bytes(&wrong_sk);
    assert_ne!(plot_pk, wrong_pk, "distinct plot keys");
    let msg = messages[0];
    let good_sig = signer(msg, &plot_pk);
    assert!(
        !verifies(&wrong_pk, msg, &good_sig),
        "correct signature must FAIL against the wrong plot public key"
    );
    let wrong_sig = real_signer(&wrong_sk)(msg, &wrong_pk);
    assert!(
        !verifies(&plot_pk, msg, &wrong_sig),
        "wrong-key signature must FAIL against the correct plot public key"
    );

    // Sanity: sign() prepends sk_to_pk(), so a signature verifies ONLY under its own signer's key —
    // confirm the two are not cross-verifiable in either direction.
    assert!(verifies(&wrong_pk, msg, &wrong_sig));
    assert!(!verifies(&plot_pk, msg, &wrong_sig));
}
