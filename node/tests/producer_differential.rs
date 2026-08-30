// Differential-harvest harness: prove our block producer against REAL mainnet blocks — the
// oracle is the mainnet wire itself. A mainnet `FullBlock` is an
// `UnfinishedBlock` plus the timelord's three infusion VDFs (challenge-chain / reward-chain /
// infused-challenge-chain infusion-point VDFs + their proofs). Strip those and what remains — the
// `RewardChainBlockUnfinished`, the whole `Foliage`, the `FoliageTransactionBlock`, the
// `TransactionsInfo`, the generator + ref-list, the finished sub-slots — is EXACTLY what our producer
// assembles.
//
// The method: for each real `FullBlock`, feed ITS OWN inputs (proof_of_space, the cc/rc signage-point
// VDFs + proofs, signage_point_index, pos_ss_cc_challenge_hash, pool_target + pool_signature,
// farmer_reward_puzzle_hash, total_iters, the REAL farmer signatures out of its own foliage, its real
// timestamp, its generator, and — for a transaction block — its real spend additions/removals) into OUR
// `create_unfinished_block_with_sigs`, and assert the produced `UnfinishedBlock` re-serializes
// BYTE-IDENTICALLY to that block's unfinished portion. Because every farmer-chosen value (the four
// signatures, extension_data, timestamp) is supplied from the real block, the ONLY thing under test is
// our ASSEMBLY: foliage structure + field order, the hashing (unfinished_reward_block_hash,
// foliage_transaction_block_hash, transactions_info_hash), the reward-claim walk (pool/farmer coinbase
// minting), the additions/removals merkle roots, the BIP158 filter, and the RewardChainBlockUnfinished
// packing. A single byte of divergence is a real producer bug.
//
// ── KNOWN, DOCUMENTED DIVERGENCE (consensus-irrelevant) ────────────────────────────────────────────
// `foliage.foliage_block_data.extension_data`: the reference derives it from MT19937, our
// producer derives it as `sha256(seed)` (core/src/consensus/producer.rs::extension_data_from_seed).
// There is NO seed that makes sha256(seed) equal a
// given MT19937 value, and — critically — the current producer public API takes `seed: &[u8]`, NOT
// `extension_data: Bytes32`, so a real extension_data CANNOT be injected through it. This harness
// therefore (1) asserts extension_data is the SOLE differing field, (2) splices the real value in, then
// (3) asserts WHOLE-BLOCK byte identity of everything else. See the report for the API-gap flag.
//
// ── INFUSION-REWRITTEN FOLIAGE FIELD (not a divergence — a mapping rule) ────────────────────────────
// `foliage.reward_block_hash` is NOT a pass-through: on finishing it is rewritten to the FINISHED
// `reward_chain_block.get_hash()`, so a `FullBlock` carries the finished hash while the UnfinishedBlock
// (and our producer, producer.rs:521) carries `get_unfinished().hash()`. The oracle reconstructs the
// unfinished value via `unfinished_reward_block_hash()` — comparing against the block's finished value
// would be a mapping bug. The other on-finish rewrites (prev_block_hash / foliage_transaction_block_hash
// / _signature) resolve to the same values `create_foliage` already emits, so they compare directly; a
// mismatch there would be a real reorg-handling difference and is left loud.
//
// ── HOW TO RUN ─────────────────────────────────────────────────────────────────────────────────────
// Runs with no env: two real transaction blocks end to end:
//   cargo test -p dg_xch_node --test producer_differential -- --nocapture

mod common;

use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::consensus::block_rewards::calculate_base_farmer_reward;
use dg_xch_core::consensus::constants::{ConsensusConstants, MAINNET};
use dg_xch_core::consensus::producer::{
    BlockTransactions, FarmerSignatures, RewardBlockClaim, create_unfinished_block_with_sigs,
    g2_infinity,
};
use dg_xch_core::protocols::full_node::RespondBlocks;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;
use std::path::{Path, PathBuf};

// The arbitrary producer seed. Its ONLY visible effect is extension_data = sha256(SEED), which the
// harness splices over with the real value (see the module divergence note). Nothing else in the
// produced block depends on it.
const SEED: &[u8] = b"dg_xch-producer-differential-seed";

// ─────────────────────────────────────────── input reconstruction ───────────────────────────────────

/// Decode a block's `transactions_info.reward_claims_incorporated` (a flat `[pool, farmer, pool,
/// farmer, ...]` coin list) back into the `RewardBlockClaim`s our producer re-mints. Each pool coin's
/// parent id is `genesis_challenge[0..16] ++ 16-byte-BE height` and each farmer coin's is
/// `genesis_challenge[16..32] ++ 16-byte-BE height` (core/src/consensus/coinbase.rs). Fees are recovered
/// exactly as `farmer_coin.amount - calculate_base_farmer_reward(height)`, so re-minting reproduces the
/// coin byte-for-byte (the pool coin carries no fees; the producer recomputes its amount from height).
///
/// Returns `Err` if the list is not clean `[pool, farmer]` pairs — that would itself be a finding.
fn reconstruct_reward_claims(
    constants: &ConsensusConstants,
    reward_coins: &[Coin],
) -> Result<Vec<RewardBlockClaim>, String> {
    let gc = constants.genesis_challenge.bytes();
    let (pool_prefix, farmer_prefix) = (&gc[0..16], &gc[16..32]);
    if !reward_coins.len().is_multiple_of(2) {
        return Err(format!(
            "reward_claims_incorporated has odd length {}; expected [pool,farmer] pairs",
            reward_coins.len()
        ));
    }
    let mut claims = Vec::with_capacity(reward_coins.len() / 2);
    for pair in reward_coins.as_chunks::<2>().0 {
        let (pool, farmer) = (&pair[0], &pair[1]);
        let pp = pool.parent_coin_info.bytes();
        let fp = farmer.parent_coin_info.bytes();
        if &pp[0..16] != pool_prefix {
            return Err(format!(
                "reward pair[0] parent prefix {:02x?} != pool prefix (genesis_challenge[0..16])",
                &pp[0..16]
            ));
        }
        if &fp[0..16] != farmer_prefix {
            return Err(format!(
                "reward pair[1] parent prefix {:02x?} != farmer prefix (genesis_challenge[16..32])",
                &fp[0..16]
            ));
        }
        let height = u32::from_be_bytes([pp[28], pp[29], pp[30], pp[31]]);
        let farmer_height = u32::from_be_bytes([fp[28], fp[29], fp[30], fp[31]]);
        if height != farmer_height {
            return Err(format!(
                "reward pair heights disagree: pool {height} vs farmer {farmer_height}"
            ));
        }
        let base = calculate_base_farmer_reward(height);
        let fees = farmer.amount.checked_sub(base).ok_or_else(|| {
            format!(
                "farmer coin amount {} < base reward {base} at height {height}",
                farmer.amount
            )
        })?;
        claims.push(RewardBlockClaim {
            height,
            pool_puzzle_hash: pool.puzzle_hash,
            farmer_puzzle_hash: farmer.puzzle_hash,
            fees,
        });
    }
    Ok(claims)
}

/// The farmer signatures lifted verbatim out of the real block (the whole point: we supply the real
/// sigs so only assembly is under test). SP sigs live on the reward-chain block; the two foliage sigs
/// live on the foliage. A non-transaction block has no `foliage_transaction_block_signature` — the
/// producer ignores the value we pass, so `g2_infinity()` is a harmless placeholder there.
fn farmer_sigs_from(full: &FullBlock) -> FarmerSignatures {
    FarmerSignatures {
        challenge_chain_sp_signature: full.reward_chain_block.challenge_chain_sp_signature,
        reward_chain_sp_signature: full.reward_chain_block.reward_chain_sp_signature,
        foliage_block_data_signature: full.foliage.foliage_block_data_signature,
        foliage_transaction_block_signature: full
            .foliage
            .foliage_transaction_block_signature
            .unwrap_or_else(g2_infinity),
    }
}

/// Build the `BlockTransactions` (spend payload) from a real transaction block plus its real spend
/// additions/removals. `additions` are the NON-coinbase coins only — reward coins are re-minted from the
/// `RewardBlockClaim`s and prepended by the producer, so passing them here would double-count. `removals`
/// are all spent-coin records. `aggregated_signature`/`cost` come straight off `transactions_info`; the
/// merkle roots and the fee are recomputed by the producer from these coins (order-independent — the
/// additions/removals roots and the BIP158 filter all sort internally).
fn block_transactions_from(
    full: &FullBlock,
    additions: &[CoinRecord],
    removals: &[CoinRecord],
) -> Option<BlockTransactions> {
    let program = full.transactions_generator.clone()?;
    let ti = full.transactions_info.as_ref()?;
    let spend_additions: Vec<Coin> = additions
        .iter()
        .filter(|c| !c.coinbase)
        .map(|c| c.coin)
        .collect();
    let spend_removals: Vec<Coin> = removals.iter().map(|c| c.coin).collect();
    Some(BlockTransactions {
        program,
        block_refs: full.transactions_generator_ref_list.clone(),
        additions: spend_additions,
        removals: spend_removals,
        aggregated_signature: ti.aggregated_signature,
        cost: ti.cost,
    })
}

/// Run OUR producer over a real block's own inputs. `spends` is the real (additions, removals) for a
/// transaction block; pass `None` for a non-transaction block, or `Some` with the real coin sets.
///
/// Returns the produced `UnfinishedBlock`. `Err` means we could not even reconstruct the inputs (e.g. a
/// transaction block whose spends we were not given) — that is a SKIP, not a producer failure.
fn produce_from_real(
    constants: &ConsensusConstants,
    full: &FullBlock,
    spends: Option<(&[CoinRecord], &[CoinRecord])>,
) -> Result<UnfinishedBlock, String> {
    let is_tx = full.is_transaction_block();
    let height = full.reward_chain_block.height;

    // Reward-claim walk: claims incorporate only on a transaction block above genesis.
    let reward_claims = if is_tx && height > 0 {
        let ti = full
            .transactions_info
            .as_ref()
            .ok_or("transaction block without transactions_info")?;
        reconstruct_reward_claims(constants, &ti.reward_claims_incorporated)?
    } else {
        Vec::new()
    };

    // Spend payload: only present when this block carries a generator AND we were handed its spends.
    let transactions = if is_tx && full.transactions_generator.is_some() {
        match spends {
            Some((adds, rems)) => block_transactions_from(full, adds, rems)
                .ok_or("could not build BlockTransactions from real block")?,
            None => {
                return Err(format!(
                    "transaction block {height} with generator needs real spends (unavailable in this fixture set)"
                ));
            }
        }
        .into()
    } else {
        None
    };

    let (prev_transaction_block_hash, timestamp) = full
        .foliage_transaction_block
        .as_ref()
        .map_or((Bytes32::default(), 0u64), |ftb| {
            (ftb.prev_transaction_block_hash, ftb.timestamp)
        });

    create_unfinished_block_with_sigs(
        constants,
        full.reward_chain_block.total_iters, // == infusion_point_total_iters
        full.reward_chain_block.signage_point_index,
        full.reward_chain_block.proof_of_space.clone(),
        full.reward_chain_block.pos_ss_cc_challenge_hash,
        full.reward_chain_block.challenge_chain_sp_vdf,
        full.challenge_chain_sp_proof.clone(),
        full.reward_chain_block.reward_chain_sp_vdf,
        full.reward_chain_sp_proof.clone(),
        full.finished_sub_slots.clone(),
        height,
        is_tx,
        &reward_claims,
        transactions.as_ref(),
        full.foliage.prev_block_hash,
        prev_transaction_block_hash,
        full.foliage.foliage_block_data.pool_target,
        full.foliage.foliage_block_data.pool_signature,
        full.foliage.foliage_block_data.farmer_reward_puzzle_hash,
        timestamp,
        SEED,
        farmer_sigs_from(full),
    )
    .map_err(|e| format!("producer error: {e:?}"))
}

// ─────────────────────────────────────────── the assertion ──────────────────────────────────────────

fn ser<T: ChiaSerialize>(t: &T) -> Vec<u8> {
    t.to_bytes(ChiaProtocolVersion::default())
        .expect("streamable encode")
}

/// Lowercase hex, dependency-free (node's dev-deps do not include `hex`).
fn hexs(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The unfinished value of `foliage.reward_block_hash`. This field is INFUSION-REWRITTEN when a
/// block is finished — it becomes the FINISHED `reward_chain_block.get_hash()`, and header
/// validation enforces `full.foliage.reward_block_hash == full.reward_chain_block.get_hash()`.
/// So a `FullBlock` carries the FINISHED hash, while the
/// UnfinishedBlock our producer emits carries `get_unfinished().hash()` (producer.rs:521, correct). The
/// oracle must reconstruct the unfinished value — comparing our unfinished hash against the block's
/// finished hash would be a mapping bug (they MUST differ). It is an infusion-only field, like the IP
/// VDFs, and is the only foliage field that needs reconstruction (prev_block_hash /
/// foliage_transaction_block_hash / _signature are also rewritten on finishing but to the same final
/// values `create_foliage` already produces, so they still compare directly).
fn unfinished_reward_block_hash(full: &FullBlock) -> Bytes32 {
    full.reward_chain_block
        .get_unfinished()
        .hash()
        .expect("unfinished reward block hash")
}

/// The `UnfinishedBlock` the real `FullBlock` implies: drop the three infusion-point VDFs (and their
/// proofs, and the transaction flag) via `RewardChainBlock::get_unfinished()`, and reconstruct the one
/// infusion-rewritten foliage field (`reward_block_hash`, see [`unfinished_reward_block_hash`]).
/// Everything else (the rest of foliage, foliage_transaction_block, transactions_info, generator,
/// ref-list, finished sub-slots) is identical unfinished ↔ full. This is the ground-truth oracle.
fn expected_unfinished(full: &FullBlock) -> UnfinishedBlock {
    let mut foliage = full.foliage;
    foliage.reward_block_hash = unfinished_reward_block_hash(full);
    UnfinishedBlock {
        finished_sub_slots: full.finished_sub_slots.clone(),
        reward_chain_block: full.reward_chain_block.get_unfinished(),
        challenge_chain_sp_proof: full.challenge_chain_sp_proof.clone(),
        reward_chain_sp_proof: full.reward_chain_sp_proof.clone(),
        foliage,
        foliage_transaction_block: full.foliage_transaction_block,
        transactions_info: full.transactions_info.clone(),
        transactions_generator: full.transactions_generator.clone(),
        transactions_generator_ref_list: full.transactions_generator_ref_list.clone(),
    }
}

/// The outcome of one differential check.
enum Outcome {
    /// Byte-identical after splicing the one documented extension_data divergence.
    Pass,
    /// A real producer bug: some field OTHER than extension_data diverged. Carries the field path and
    /// both values.
    Fail(String),
    /// Inputs could not be reconstructed (transaction block with unavailable spends). Not a bug.
    Skip(String),
}

/// Compare produced vs expected field by field, return the FIRST divergence (excluding extension_data,
/// which is checked separately by the caller). Named field paths so a mismatch is immediately
/// actionable.
fn first_real_divergence(produced: &UnfinishedBlock, full: &FullBlock) -> Option<String> {
    macro_rules! cmp {
        ($name:literal, $p:expr, $e:expr) => {{
            let (p, e) = (&$p, &$e);
            if p != e {
                return Some(format!(
                    "{}: produced={} expected={}",
                    $name,
                    hexs(&ser(p)),
                    hexs(&ser(e))
                ));
            }
        }};
    }

    // RewardChainBlockUnfinished — the packing under test (total_iters, sp_index, pos challenge, PoS,
    // the two SP VDFs, the two SP signatures).
    cmp!(
        "reward_chain_block",
        produced.reward_chain_block,
        full.reward_chain_block.get_unfinished()
    );
    cmp!(
        "finished_sub_slots",
        produced.finished_sub_slots,
        full.finished_sub_slots
    );
    cmp!(
        "challenge_chain_sp_proof",
        produced.challenge_chain_sp_proof,
        full.challenge_chain_sp_proof
    );
    cmp!(
        "reward_chain_sp_proof",
        produced.reward_chain_sp_proof,
        full.reward_chain_sp_proof
    );

    // Foliage, field by field — extension_data deliberately excluded (checked by the caller).
    let (pf, ef) = (&produced.foliage, &full.foliage);
    cmp!(
        "foliage.prev_block_hash",
        pf.prev_block_hash,
        ef.prev_block_hash
    );
    // reward_block_hash is infusion-rewritten (see unfinished_reward_block_hash): compare our unfinished
    // value against the reconstructed unfinished hash, NOT the FullBlock's finished value.
    cmp!(
        "foliage.reward_block_hash",
        pf.reward_block_hash,
        unfinished_reward_block_hash(full)
    );
    cmp!(
        "foliage.foliage_block_data.unfinished_reward_block_hash",
        pf.foliage_block_data.unfinished_reward_block_hash,
        ef.foliage_block_data.unfinished_reward_block_hash
    );
    cmp!(
        "foliage.foliage_block_data.pool_target",
        pf.foliage_block_data.pool_target,
        ef.foliage_block_data.pool_target
    );
    cmp!(
        "foliage.foliage_block_data.pool_signature",
        pf.foliage_block_data.pool_signature,
        ef.foliage_block_data.pool_signature
    );
    cmp!(
        "foliage.foliage_block_data.farmer_reward_puzzle_hash",
        pf.foliage_block_data.farmer_reward_puzzle_hash,
        ef.foliage_block_data.farmer_reward_puzzle_hash
    );
    cmp!(
        "foliage.foliage_block_data_signature",
        pf.foliage_block_data_signature,
        ef.foliage_block_data_signature
    );
    cmp!(
        "foliage.foliage_transaction_block_hash",
        pf.foliage_transaction_block_hash,
        ef.foliage_transaction_block_hash
    );
    cmp!(
        "foliage.foliage_transaction_block_signature",
        pf.foliage_transaction_block_signature,
        ef.foliage_transaction_block_signature
    );

    cmp!(
        "foliage_transaction_block",
        produced.foliage_transaction_block,
        full.foliage_transaction_block
    );
    cmp!(
        "transactions_info",
        produced.transactions_info,
        full.transactions_info
    );
    cmp!(
        "transactions_generator",
        produced.transactions_generator,
        full.transactions_generator
    );
    cmp!(
        "transactions_generator_ref_list",
        produced.transactions_generator_ref_list,
        full.transactions_generator_ref_list
    );
    None
}

/// Full differential check for one real block.
fn differential_check(
    constants: &ConsensusConstants,
    full: &FullBlock,
    spends: Option<(&[CoinRecord], &[CoinRecord])>,
) -> Outcome {
    let mut produced = match produce_from_real(constants, full, spends) {
        Ok(b) => b,
        Err(reason) => return Outcome::Skip(reason),
    };

    // Substantiate the documented divergence: our extension_data IS sha256(SEED), and it is the value
    // that differs from the real block. (If it happened to already match — it won't — the splice below
    // is a no-op and the block still passes.)
    let produced_ext = produced.foliage.foliage_block_data.extension_data;
    let real_ext = full.foliage.foliage_block_data.extension_data;
    let expected_ext = Bytes32::from(hash_256(SEED));
    if produced_ext != expected_ext {
        return Outcome::Fail(format!(
            "extension_data derivation changed: producer gave {}, expected sha256(SEED)={} \
             (the producer no longer derives extension_data as sha256(seed) — update this harness)",
            hexs(&produced_ext.bytes()),
            hexs(&expected_ext.bytes())
        ));
    }

    // Splice the real extension_data (documented, consensus-irrelevant; producer.rs:48-60), then every
    // remaining byte must match.
    produced.foliage.foliage_block_data.extension_data = real_ext;

    if let Some(diff) = first_real_divergence(&produced, full) {
        return Outcome::Fail(diff);
    }

    // Headline: after the single documented splice, the WHOLE unfinished block is byte-identical.
    let expected = expected_unfinished(full);
    if ser(&produced) != ser(&expected) {
        return Outcome::Fail(
            "whole-block bytes differ but no single field diverged (comparison gap — investigate)"
                .to_string(),
        );
    }
    Outcome::Pass
}

// ─────────────────────────────────────────── committed fixtures ────────────────────────────

// Two real mainnet TRANSACTION blocks that ship in-repo with their real spend additions/removals. This
// runs with no env var and exercises the FULL assembly: reward-claim walk (3 and 2 claims), the merkle
// roots, the BIP158 filter, the generator + ref-list, and every foliage hash.
#[test]
fn producer_matches_real_mainnet_fixture_blocks() {
    let heights = [5_000_000u32, 5_000_004u32];
    let mut passed = 0usize;
    let mut first_failure: Option<(u32, String)> = None;

    for &h in &heights {
        let full = common::load_full_block(h);
        let (adds, rems) = common::load_adds_rems(h);
        assert!(
            full.is_transaction_block(),
            "fixture {h} is expected to be a transaction block"
        );
        match differential_check(&MAINNET, &full, Some((&adds, &rems))) {
            Outcome::Pass => {
                passed += 1;
                eprintln!(
                    "[PASS] height {h}: produced UnfinishedBlock is byte-identical to mainnet"
                );
            }
            Outcome::Fail(diff) => {
                eprintln!("[FAIL] height {h}: {diff}");
                first_failure.get_or_insert((h, diff));
            }
            Outcome::Skip(reason) => panic!("fixture {h} must be checkable, got skip: {reason}"),
        }
    }

    eprintln!(
        "fixture tier: {passed}/{} blocks byte-identical{}",
        heights.len(),
        first_failure
            .as_ref()
            .map_or(String::new(), |(h, d)| format!(
                "; first failure @ {h}: {d}"
            ))
    );
    assert!(
        first_failure.is_none(),
        "producer diverged from a real mainnet block: {first_failure:?}"
    );
    assert_eq!(passed, heights.len());
}

// A guard on the reconstruction itself: decoding reward_claims_incorporated and re-encoding it through
// the producer must reproduce the exact coin list of a known fixture (so a decode bug can't masquerade
// as a producer pass). Uses height 5_000_000 (3 reward-claim pairs).
#[test]
fn reward_claim_reconstruction_round_trips_fixture() {
    let full = common::load_full_block(5_000_000);
    let ti = full.transactions_info.as_ref().expect("tx info");
    let claims =
        reconstruct_reward_claims(&MAINNET, &ti.reward_claims_incorporated).expect("decode claims");
    assert_eq!(
        claims.len() * 2,
        ti.reward_claims_incorporated.len(),
        "pair count"
    );

    // Re-mint and compare to the real reward coins (order preserved: [pool, farmer] per claim).
    use dg_xch_core::consensus::block_rewards::calculate_pool_reward;
    use dg_xch_core::consensus::coinbase::{create_farmer_coin, create_pool_coin};
    let mut reminted: Vec<Coin> = Vec::new();
    for c in &claims {
        reminted.push(create_pool_coin(
            c.height,
            c.pool_puzzle_hash,
            calculate_pool_reward(c.height),
            MAINNET.genesis_challenge,
        ));
        reminted.push(create_farmer_coin(
            c.height,
            c.farmer_puzzle_hash,
            calculate_base_farmer_reward(c.height) + c.fees,
            MAINNET.genesis_challenge,
        ));
    }
    assert_eq!(
        reminted, ti.reward_claims_incorporated,
        "re-minted reward coins must byte-match the real block's reward_claims_incorporated"
    );
    // Silence unused import in the non-corpus build path.
    let _ = Bytes96::default();
}
