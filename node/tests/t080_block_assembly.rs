use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_with_args::{ConditionWithArgs, Message};
use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::proof_of_space::{ProofBytes, ProofOfSpace};
use dg_xch_core::blockchain::signage_point::SignagePoint;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::clvm::parser::is_canonical_serialization;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, CoinSpendContext, ConditionValidationContext,
    TransactionBlockValidationInput, conditions_from_spend_bundle, execute_block_generator_result,
    validate_block_aggregate_signature, validate_transaction_block,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::protocols::farmer::DeclareProofOfSpace;
use dg_xch_core::traits::SizedBytes;
use dg_xch_node::farmer::{CandidateIters, CandidatePrev, assemble_candidate};
use dg_xch_node::mempool::Mempool;
use dg_xch_stores::{CoinStore, SqliteStore};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

async fn store() -> SqliteStore {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("dg_xch_t080_{}_{n}.sqlite", std::process::id()));
    SqliteStore::open(&path).await.expect("open store")
}

// The candidate is built for a POST-SF9 mainnet height so the emitted generator must satisfy the
// SF9 form (SIMPLE_GENERATOR | CANONICAL_INTS | LIMIT_SPENDS at soft_fork9_height = 8,655,000
// mainnet).
const PEAK_HEIGHT: u32 = MAINNET.soft_fork9_height + 100;
const PEAK_TIMESTAMP: u64 = 1_760_000_000;
// The height of the block being built (the CLVM flag-ladder key).
const BUILD_HEIGHT: u32 = PEAK_HEIGHT + 1;
// BLOCK_OVERHEAD = QUOTE_BYTES * COST_PER_BYTE + QUOTE_EXECUTION_COST.
const BLOCK_OVERHEAD: u64 = 2 * MAINNET.cost_per_byte + 20;
const TIMEOUT: Duration = Duration::from_secs(2);

fn easy_puzzle_hash() -> Bytes32 {
    Program::to(1_u8).tree_hash()
}

// A real runnable bundle over the `1` puzzle (echoes its solution as the condition list).
fn bundle_with_conditions(coins: &[Coin], conditions: Vec<SExp<'static>>) -> SpendBundle {
    let puzzle = Program::to(1_u8);
    let solution = Program::to(conditions);
    let empty_solution = Program::to(Vec::<SExp>::new());
    let mut infinity = [0_u8; 96];
    infinity[0] = 0xc0;
    let coin_spends = coins
        .iter()
        .enumerate()
        .map(|(i, coin)| CoinSpend {
            coin: *coin,
            puzzle_reveal: puzzle.serialized().expect("puzzle serializes"),
            // Conditions ride on the FIRST spend only; the rest are empty-condition spends.
            solution: if i == 0 {
                solution.serialized().expect("solution serializes")
            } else {
                empty_solution.serialized().expect("solution serializes")
            },
        })
        .collect();
    SpendBundle {
        coin_spends,
        aggregated_signature: Bytes96::from(infinity),
    }
}

// A fee-paying bundle: spend `coin`, re-create `coin.amount - fee` — `fee` mojos to the farmer.
fn fee_bundle(coin: &Coin, fee: u64) -> SpendBundle {
    bundle_with_conditions(
        std::slice::from_ref(coin),
        vec![
            SExp::from(&ConditionWithArgs::CreateCoin(
                Bytes32::from([0x77u8; 32]),
                coin.amount - fee,
                vec![],
            ))
            .to_owned(),
        ],
    )
}

async fn seed_coins(store: &SqliteStore, tag: u8, amount: u64, count: usize) -> Vec<Coin> {
    let coins: Vec<Coin> = (0..count)
        .map(|i| {
            let mut parent = [tag; 32];
            parent[0] = (i >> 16) as u8;
            parent[1] = (i >> 8) as u8;
            parent[2] = i as u8;
            Coin {
                parent_coin_info: Bytes32::from(parent),
                puzzle_hash: easy_puzzle_hash(),
                amount,
            }
        })
        .collect();
    let records: Vec<CoinRecord> = coins
        .iter()
        .map(|coin| CoinRecord {
            coin: *coin,
            coinbase: false,
            confirmed_block_index: PEAK_HEIGHT,
            spent: false,
            spent_block_index: 0,
            timestamp: PEAK_TIMESTAMP,
        })
        .collect();
    store
        .apply_block(PEAK_HEIGHT, PEAK_TIMESTAMP, &records, &[])
        .await
        .expect("apply coins");
    coins
}

async fn seed_coin(store: &SqliteStore, tag: u8, amount: u64) -> Coin {
    seed_coins(store, tag, amount, 1).await[0]
}

fn mempool_at_peak() -> Mempool {
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(PEAK_HEIGHT, PEAK_TIMESTAMP);
    mp
}

// Admit a REAL bundle: conditions computed by the exact admission path the server runs
// (conditions_from_spend_bundle at the next block's height).
async fn admit_real(mp: &mut Mempool, store: &SqliteStore, bundle: SpendBundle) -> Bytes32 {
    let conds =
        conditions_from_spend_bundle(&bundle, BUILD_HEIGHT, &MAINNET).expect("bundle runs clean");
    mp.admit(store, bundle, conds).await.expect("admitted")
}

// Admit a real bundle but OVERRIDE the admission cost — the selection-heuristic tests' lever (the
// mempool trusts caller-validated conds; selection reads item.cost, the re-run runs the real
// spends).
async fn admit_with_cost(
    mp: &mut Mempool,
    store: &SqliteStore,
    bundle: SpendBundle,
    cost: u64,
) -> Bytes32 {
    let mut conds =
        conditions_from_spend_bundle(&bundle, BUILD_HEIGHT, &MAINNET).expect("bundle runs clean");
    conds.cost = cost;
    mp.admit(store, bundle, conds).await.expect("admitted")
}

// ---- the winning-declare fixture (the same shapes farmer.rs's own tests use) ---------------------

fn declare(index: u8, cc_sp: u8, challenge_hash: Bytes32) -> DeclareProofOfSpace {
    DeclareProofOfSpace {
        challenge_hash,
        challenge_chain_sp: Bytes32::from([cc_sp; 32]),
        signage_point_index: index,
        reward_chain_sp: Bytes32::from([cc_sp.wrapping_add(1); 32]),
        proof_of_space: ProofOfSpace {
            version: 0,
            plot_index: 0,
            meta_group: 0,
            strength: 0,
            challenge: Bytes32::from([9; 32]),
            pool_public_key: Some(Bytes48::from([1; 48])),
            pool_contract_puzzle_hash: None,
            plot_public_key: Bytes48::from([2; 48]),
            size: 32,
            proof: ProofBytes::from(vec![0u8; 64]),
        },
        challenge_chain_sp_signature: Default::default(),
        reward_chain_sp_signature: Default::default(),
        farmer_puzzle_hash: Bytes32::from([3; 32]),
        pool_target: None,
        pool_signature: None,
        include_signature_source_data: false,
    }
}

fn iters() -> CandidateIters {
    CandidateIters {
        required_iters: 100,
        sp_iters: 10,
        ip_iters: 20,
        infusion_point_total_iters: 30,
        candidate_sp_total_iters: 10,
    }
}

fn sp(cc_challenge: u8) -> SignagePoint {
    use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
    use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
    use dg_xch_core::blockchain::vdf_info::VdfInfo;
    use dg_xch_core::blockchain::vdf_proof::VdfProof;
    let vdf = |c: u8| VdfInfo {
        challenge: Bytes32::from([c; 32]),
        number_of_iterations: 1,
        output: ClassgroupElement::get_default_element(),
    };
    let proof = VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::default(),
        normalized_to_identity: false,
    };
    SignagePoint {
        cc_vdf: Some(vdf(cc_challenge)),
        cc_proof: Some(proof.clone()),
        rc_vdf: Some(vdf(cc_challenge.wrapping_add(1))),
        rc_proof: Some(proof),
    }
}

fn tx_candidate_prev() -> CandidatePrev {
    CandidatePrev {
        is_transaction_block: true,
        prev_block_hash: Bytes32::from([0x50; 32]),
        prev_transaction_block_hash: Bytes32::from([0x51; 32]),
        prev_transaction_block_height: PEAK_HEIGHT,
        reward_claims: Vec::new(),
    }
}

// Assemble the winning candidate exactly as try_build_candidate does, with `transactions`.
fn assemble(
    transactions: Option<&dg_xch_core::consensus::producer::BlockTransactions>,
) -> dg_xch_core::blockchain::unfinished_block::UnfinishedBlock {
    let d = declare(2, 5, MAINNET.genesis_challenge);
    let signage = sp(5);
    let pool_target = PoolTarget {
        puzzle_hash: Bytes32::from([8; 32]),
        max_height: 0,
    };
    let (candidate, _request) = assemble_candidate(
        &MAINNET,
        &d,
        Bytes32::from([42; 32]),
        Some(&signage),
        Vec::new(),
        &iters(),
        BUILD_HEIGHT,
        &tx_candidate_prev(),
        transactions,
        pool_target,
        Bytes32::from([3; 32]),
        PEAK_TIMESTAMP + 20,
        MAINNET.genesis_challenge,
    )
    .expect("candidate assembles");
    candidate
}

// ── Test 1 (was RED — the empty-block gap; now the end-to-end green) ────────────────────────────────
// A winning candidate assembled while the mempool holds a fee-paying resident item carries the
// block generator, the item's fee, and the re-run cost — and the assembled transaction body passes
// OUR OWN t070-hardened validation with the SF9 rules live (the strongest available check: the
// producer is gated by the proven consumer).
#[tokio::test]
async fn winning_candidate_includes_fee_paying_mempool_item() {
    let store = store().await;
    let coin = seed_coin(&store, 0xA1, 1_000_000).await;
    let fee = 100_000u64;
    let mut mp = mempool_at_peak();
    admit_real(&mut mp, &store, fee_bundle(&coin, fee)).await;
    assert_eq!(mp.len(), 1, "the fee-paying item is resident");

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("a resident fee-paying item must yield a block generator");
    let candidate = assemble(Some(&tx));

    let ti = candidate
        .transactions_info
        .clone()
        .expect("a transaction-block candidate carries transactions_info");
    assert!(
        candidate.transactions_generator.is_some(),
        "winning candidate with a fee-paying resident mempool item must carry a block generator"
    );
    assert_eq!(ti.fees, fee, "the included item's fee lands in ti.fees");
    assert_eq!(ti.cost, tx.cost, "ti.cost is the builder's re-run cost");
    assert!(
        candidate.transactions_generator_ref_list.is_empty(),
        "post-SF9: generator ref list must be empty"
    );

    // Drive the assembled body through the full transaction-block validation (T0-1 rules), exactly
    // as the engine validates a received block: SF9 live (prev tx height past soft_fork9_height),
    // condition context = the previous transaction block's frame + the spent coin's birth record.
    let generator = candidate
        .transactions_generator
        .clone()
        .expect("generator present");
    let mut coin_context = HashMap::new();
    coin_context.insert(
        coin.name(),
        CoinSpendContext {
            birth_height: Some(PEAK_HEIGHT),
            birth_seconds: Some(PEAK_TIMESTAMP),
            spent_height: Some(PEAK_HEIGHT),
            spent_seconds: Some(PEAK_TIMESTAMP),
        },
    );
    let ctx = ConditionValidationContext {
        block_height: PEAK_HEIGHT,
        previous_transaction_block_timestamp: Some(PEAK_TIMESTAMP),
        coin_context,
    };
    let result = validate_transaction_block(&TransactionBlockValidationInput {
        generator_input: BlockGeneratorInput {
            transactions_generator: generator,
            generator_refs: Vec::new(),
            constants: MAINNET,
            height: BUILD_HEIGHT,
            flags: BlockGeneratorFlags::for_height(&MAINNET, BUILD_HEIGHT),
        },
        transactions_info: &ti,
        foliage_transaction_block: candidate.foliage_transaction_block.as_ref(),
        condition_context: Some(&ctx),
        prev_transaction_block_height: PEAK_HEIGHT,
    })
    .expect("the assembled block passes our own transaction-body validation (rules live)");
    assert_eq!(result.conditions.spends.len(), 1);
    assert_eq!(result.fee_summary.reserve_fee, 0);
}

// Resident ⇒ includable: an item admitted with an effective ASSERT_HEIGHT exactly at the pool's
// peak (the strictest resident lock — `assert_height <= peak`, t060 assert_pool_valid_at_peak)
// assembles into a block that validates under the SAME previous-transaction-block frame.
#[tokio::test]
async fn resident_timelocked_item_is_includable_at_the_peak_frame() {
    let store = store().await;
    let coin = seed_coin(&store, 0xA2, 500_000).await;
    let fee = 1_000u64;
    let bundle = bundle_with_conditions(
        std::slice::from_ref(&coin),
        vec![
            SExp::from(&ConditionWithArgs::CreateCoin(
                Bytes32::from([0x78u8; 32]),
                coin.amount - fee,
                vec![],
            ))
            .to_owned(),
            SExp::from(&ConditionWithArgs::AssertHeightAbsolute(PEAK_HEIGHT)).to_owned(),
        ],
    );
    let mut mp = mempool_at_peak();
    admit_real(&mut mp, &store, bundle).await;
    assert_eq!(mp.len(), 1, "assert_height == peak is resident, not parked");

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("resident timelocked item is includable");
    let candidate = assemble(Some(&tx));
    let ti = candidate.transactions_info.clone().expect("ti");
    let mut coin_context = HashMap::new();
    coin_context.insert(
        coin.name(),
        CoinSpendContext {
            birth_height: Some(PEAK_HEIGHT),
            birth_seconds: Some(PEAK_TIMESTAMP),
            spent_height: Some(PEAK_HEIGHT),
            spent_seconds: Some(PEAK_TIMESTAMP),
        },
    );
    let ctx = ConditionValidationContext {
        block_height: PEAK_HEIGHT,
        previous_transaction_block_timestamp: Some(PEAK_TIMESTAMP),
        coin_context,
    };
    validate_transaction_block(&TransactionBlockValidationInput {
        generator_input: BlockGeneratorInput {
            transactions_generator: candidate.transactions_generator.clone().expect("generator"),
            generator_refs: Vec::new(),
            constants: MAINNET,
            height: BUILD_HEIGHT,
            flags: BlockGeneratorFlags::for_height(&MAINNET, BUILD_HEIGHT),
        },
        transactions_info: &ti,
        foliage_transaction_block: candidate.foliage_transaction_block.as_ref(),
        condition_context: Some(&ctx),
        prev_transaction_block_height: PEAK_HEIGHT,
    })
    .expect("assert_height == peak validates against the same prev-tx-block frame it was admitted under");
}

// ── Test 2: fee-priority ordering + the seq (FIFO) tiebreak ───────────────────────────────────────
// `priority DESC, seq ASC` — the highest fee per virtual cost assembles first; equal-priority
// items assemble in admission order.
#[tokio::test]
async fn fee_priority_orders_selection_with_seq_tiebreak() {
    let store = store().await;
    let low_first = seed_coin(&store, 0xB1, 1_000_000).await;
    let high = seed_coin(&store, 0xB2, 1_000_000).await;
    let low_second = seed_coin(&store, 0xB3, 1_000_000).await;
    let mut mp = mempool_at_peak();
    // Admission order: low fee, high fee, low fee (identical bundle shape ⇒ identical cost, so
    // fee alone orders priority; the two low-fee items tie and must keep admission order).
    admit_real(&mut mp, &store, fee_bundle(&low_first, 50_000)).await;
    admit_real(&mut mp, &store, fee_bundle(&high, 200_000)).await;
    admit_real(&mut mp, &store, fee_bundle(&low_second, 50_000)).await;

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("three includable items");
    let removal_ids: Vec<Bytes32> = tx.removals.iter().map(Coin::name).collect();
    assert_eq!(
        removal_ids,
        vec![high.name(), low_first.name(), low_second.name()],
        "selection order: highest fee-per-cost first, then the equal-fee items in seq (FIFO) order"
    );
    assert_eq!(
        tx.additions.len(),
        3,
        "each item's CREATE_COIN addition rides along in the same order"
    );
}

// ── Test 3a: the 6,000-spend cap enforced at build ────────────────────────────────────────────────
// An item that would push the block past MAX_SPENDS_PER_BLOCK is skipped; a later, smaller item
// that still fits is taken; selection stops at the cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spend_cap_6000_enforced_at_build() {
    let store = store().await;
    // 5,999 one-mojo-fee spends: high aggregate fee ⇒ highest priority.
    let big_coins = seed_coins(&store, 0xC1, 1_000, 5_999).await;
    let two_coins = seed_coins(&store, 0xC2, 3, 2).await;
    let one_coin = seed_coin(&store, 0xC3, 1).await;
    let mut mp = mempool_at_peak();
    // No additions ⇒ fee = spent amount; fees (5.999M, 6, 1) order the three items.
    admit_real(&mut mp, &store, bundle_with_conditions(&big_coins, vec![])).await;
    admit_real(&mut mp, &store, bundle_with_conditions(&two_coins, vec![])).await;
    admit_real(
        &mut mp,
        &store,
        bundle_with_conditions(std::slice::from_ref(&one_coin), vec![]),
    )
    .await;

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("block assembles");
    assert_eq!(
        tx.removals.len(),
        6_000,
        "5,999-spend item + the 1-spend item fill the cap; the 2-spend item is skipped"
    );
    let removal_ids: std::collections::HashSet<Bytes32> =
        tx.removals.iter().map(Coin::name).collect();
    assert!(
        !removal_ids.contains(&two_coins[0].name()),
        "the item that would exceed MAX_SPENDS_PER_BLOCK is skipped"
    );
    assert!(
        removal_ids.contains(&one_coin.name()),
        "a later item that still fits under the cap is taken"
    );
}

// ── Test 3b: MAX_SKIPPED_ITEMS — the loop breaks ON the tenth cost-full skip ─────────────────────
// After ten items that don't fit, selection gives up even on items that WOULD fit.
#[tokio::test]
async fn skip_heuristic_breaks_after_max_skipped_items() {
    let budget = MAINNET.max_block_cost_clvm - BLOCK_OVERHEAD;
    for (skippers, expect_small) in [(10usize, false), (9usize, true)] {
        let store = store().await;
        let filler = seed_coin(&store, 0xD1, 1_000_000_000_000_000).await;
        let filler2 = seed_coin(&store, 0xD4, 1_000_000_000_000_000).await;
        let skip_coins = seed_coins(&store, 0xD2, 1_000_000_000_000, 10).await;
        let small = seed_coin(&store, 0xD3, 1).await;
        let mut mp = mempool_at_peak();
        // Fillers: highest priority, together consume all but 100M of the budget (≥
        // MIN_COST_THRESHOLD, so selection keeps going). Two items — each stays under the
        // 5.5B per-tx mempool cap.
        let max_tx = MAINNET.max_block_cost_clvm / 2;
        admit_with_cost(
            &mut mp,
            &store,
            bundle_with_conditions(std::slice::from_ref(&filler), vec![]),
            max_tx,
        )
        .await;
        admit_with_cost(
            &mut mp,
            &store,
            bundle_with_conditions(std::slice::from_ref(&filler2), vec![]),
            budget - 100_000_000 - max_tx,
        )
        .await;
        // The skippers: each 200M (doesn't fit in the remaining 100M), mid priority.
        for coin in skip_coins.iter().take(skippers) {
            admit_with_cost(
                &mut mp,
                &store,
                bundle_with_conditions(std::slice::from_ref(coin), vec![]),
                200_000_000,
            )
            .await;
        }
        // The small item: fits (1M), lowest priority — reached only if the loop survives the skips.
        admit_with_cost(
            &mut mp,
            &store,
            bundle_with_conditions(std::slice::from_ref(&small), vec![]),
            1_000_000,
        )
        .await;

        let tx = mp
            .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
            .expect("the filler assembles");
        let removal_ids: std::collections::HashSet<Bytes32> =
            tx.removals.iter().map(Coin::name).collect();
        assert!(removal_ids.contains(&filler.name()), "filler included");
        assert!(removal_ids.contains(&filler2.name()), "filler2 included");
        assert!(
            skip_coins
                .iter()
                .take(skippers)
                .all(|c| !removal_ids.contains(&c.name())),
            "cost-full items are skipped, never included"
        );
        assert_eq!(
            removal_ids.contains(&small.name()),
            expect_small,
            "{skippers} skips: the small fitting item is {}",
            if expect_small {
                "still reached (skip budget not exhausted)"
            } else {
                "abandoned (break ON the tenth skip)"
            }
        );
    }
}

// ── Test 3c: MIN_COST_THRESHOLD — stop once the remaining budget is below a typical spend ────────
// After adding an item that leaves < 6M cost, selection stops without considering further items,
// even ones that would fit.
#[tokio::test]
async fn min_cost_threshold_stops_selection() {
    let budget = MAINNET.max_block_cost_clvm - BLOCK_OVERHEAD;
    let store = store().await;
    let filler = seed_coin(&store, 0xE1, 1_000_000_000_000_000).await;
    let filler2 = seed_coin(&store, 0xE3, 1_000_000_000_000_000).await;
    let small = seed_coin(&store, 0xE2, 1).await;
    let mut mp = mempool_at_peak();
    // Two fillers (each under the 5.5B per-tx cap) that together leave 5,999,999 <
    // MIN_COST_THRESHOLD after inclusion.
    let max_tx = MAINNET.max_block_cost_clvm / 2;
    admit_with_cost(
        &mut mp,
        &store,
        bundle_with_conditions(std::slice::from_ref(&filler), vec![]),
        max_tx,
    )
    .await;
    admit_with_cost(
        &mut mp,
        &store,
        bundle_with_conditions(std::slice::from_ref(&filler2), vec![]),
        budget - 5_999_999 - max_tx,
    )
    .await;
    // Would fit (1M < 5,999,999) but must never be considered.
    admit_with_cost(
        &mut mp,
        &store,
        bundle_with_conditions(std::slice::from_ref(&small), vec![]),
        1_000_000,
    )
    .await;

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("the fillers assemble");
    let removal_ids: std::collections::HashSet<Bytes32> =
        tx.removals.iter().map(Coin::name).collect();
    assert!(removal_ids.contains(&filler.name()));
    assert!(removal_ids.contains(&filler2.name()));
    assert!(
        !removal_ids.contains(&small.name()),
        "selection stops at the low-budget threshold; the fitting small item is not considered"
    );
}

// ── Test 4: cost accounting — re-run cost, item cost, BLOCK_OVERHEAD ─────────────────────────────
// Cost accounting: a mempool item's admission cost is its conditions + execution + byte cost as
// if solution_generator-wrapped MINUS the quote wrapper (mempool_manager BLOCK_OVERHEAD); the
// assembled single-item block's true cost is therefore EXACTLY item.cost + BLOCK_OVERHEAD, and the
// builder's declared cost must equal an independent re-run of the emitted generator.
#[tokio::test]
async fn cost_accounting_matches_rerun_and_block_overhead() {
    let store = store().await;
    let coin = seed_coin(&store, 0xF1, 900_000).await;
    let mut mp = mempool_at_peak();
    let name = admit_real(&mut mp, &store, fee_bundle(&coin, 40_000)).await;
    let item_cost = mp.get(&name).expect("resident").cost;

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("assembles");
    assert_eq!(
        tx.cost,
        item_cost + BLOCK_OVERHEAD,
        "single-item block: generator cost == admission cost + BLOCK_OVERHEAD (quote bytes + quote execution)"
    );

    // Independent re-run of the emitted program — the builder's declared cost is the truth.
    let conds = execute_block_generator_result(&BlockGeneratorInput {
        transactions_generator: tx.program.clone(),
        generator_refs: Vec::new(),
        constants: MAINNET,
        height: BUILD_HEIGHT,
        flags: BlockGeneratorFlags::for_height(&MAINNET, BUILD_HEIGHT),
    })
    .expect("emitted generator re-runs");
    assert_eq!(
        tx.cost, conds.cost,
        "declared cost == independent re-run cost"
    );

    // Multi-item: the selection accounting is conservative — the true block cost never exceeds the
    // sum of admission costs + BLOCK_OVERHEAD (each item over-pays the shared wrapper bytes).
    let coin2 = seed_coin(&store, 0xF2, 700_000).await;
    let mut mp2 = mempool_at_peak();
    let n1 = admit_real(&mut mp2, &store, fee_bundle(&coin, 40_000)).await;
    let n2 = admit_real(&mut mp2, &store, fee_bundle(&coin2, 30_000)).await;
    let sum: u64 = mp2.get(&n1).unwrap().cost + mp2.get(&n2).unwrap().cost;
    let tx2 = mp2
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("assembles");
    assert!(
        tx2.cost <= sum + BLOCK_OVERHEAD,
        "multi-item block cost {} must not exceed Σ item costs {} + overhead {}",
        tx2.cost,
        sum,
        BLOCK_OVERHEAD
    );
    let fees = dg_xch_core::consensus::producer::compute_block_fee(&tx2.additions, &tx2.removals)
        .expect("fee computes");
    assert_eq!(fees, 70_000, "both items' fees selected");
}

// ── Test 4b: back-reference compression packs spends a plain generator can't ──────────────────────
// The point of create_block_generator2's compressed generator: a spend set whose PLAIN (byte-cost)
// sum exceeds MAX_BLOCK_COST_CLVM still fits once the repeated subtree is back-reference compressed,
// so the extra spend(s) make it into the block. Four single-coin bundles carry the IDENTICAL large
// solution (a ~250 KiB create-coin memo). Plain, that's ~4 × 250 KiB of duplicated bytes — over the
// block limit; compressed, the solution is serialized once and referenced, so all four fit and the
// block validates. Proves the cost-limit cutoff admits more spends under compression.
#[tokio::test]
async fn backref_compression_packs_extra_spend_over_plain_limit() {
    let store = store().await;
    // Four coins over the identity (`1`) puzzle — the same puzzle_hash bundle_with_conditions uses.
    let coins = seed_coins(&store, 0xC7, 900_000, 4).await;
    // One big memo, shared by every spend's solution ⇒ the ~250 KiB solution subtree repeats and
    // back-reference compresses. 250 KiB × 12000 cost/byte ≈ 3.0e9 per spend: three fit the 11e9
    // block plain, the fourth overflows — but compressed it references the one solution and fits.
    let memo = vec![0xABu8; 250_000];
    let create_ph = Bytes32::from([0x55u8; 32]);
    let conditions = || {
        vec![
            SExp::from(&ConditionWithArgs::CreateCoin(
                create_ph,
                860_000,
                vec![memo.clone()],
            ))
            .to_owned(),
        ]
    };

    let mut mp = mempool_at_peak();
    let mut names = Vec::new();
    for coin in &coins {
        names.push(
            admit_real(
                &mut mp,
                &store,
                bundle_with_conditions(std::slice::from_ref(coin), conditions()),
            )
            .await,
        );
    }
    assert_eq!(mp.len(), 4, "all four large spends resident");

    // The plain accounting: the sum of the four items' byte-dominated admission costs exceeds the
    // block limit, so a plain generator could not include all four.
    let costs: Vec<u64> = names
        .iter()
        .map(|n| mp.get(n).expect("resident").cost)
        .collect();
    let total_plain: u64 = costs.iter().sum();
    assert!(
        total_plain > MAINNET.max_block_cost_clvm,
        "the four spends must overflow the block uncompressed (Σ {total_plain} > {})",
        MAINNET.max_block_cost_clvm
    );

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("the compressed block assembles");
    let removal_ids: std::collections::HashSet<Bytes32> =
        tx.removals.iter().map(Coin::name).collect();
    for coin in &coins {
        assert!(
            removal_ids.contains(&coin.name()),
            "every spend — including the one a plain generator would drop — is packed via compression"
        );
    }
    assert_eq!(
        removal_ids.len(),
        4,
        "exactly the four spends, all included"
    );
    // The emitted (compressed) block is valid: its re-run cost is under the limit and strictly below
    // the plain sum — the bytes the shared solution would have cost, saved.
    assert!(
        tx.cost <= MAINNET.max_block_cost_clvm,
        "compressed block cost {} within the limit",
        tx.cost
    );
    assert!(
        tx.cost < total_plain,
        "compression saved cost: {} < plain Σ {}",
        tx.cost,
        total_plain
    );
    assert!(
        is_canonical_serialization(tx.program.as_ref()),
        "compressed generator is canonical CLVM"
    );

    // And it round-trips through the full body validator with SF9 live — a real, valid block.
    let conds = execute_block_generator_result(&BlockGeneratorInput {
        transactions_generator: tx.program.clone(),
        generator_refs: Vec::new(),
        constants: MAINNET,
        height: BUILD_HEIGHT,
        flags: BlockGeneratorFlags::for_height(&MAINNET, BUILD_HEIGHT),
    })
    .expect("compressed generator re-runs clean");
    assert_eq!(conds.spends.len(), 4, "validator recovers all four spends");
    assert_eq!(
        tx.cost, conds.cost,
        "declared cost == independent re-run cost"
    );
}

#[tokio::test]
async fn aggregate_signature_of_included_bundles_verifies() {
    use blst::min_pk::SecretKey;
    let store = store().await;
    let coin1 = seed_coin(&store, 0x91, 400_000).await;
    let coin2 = seed_coin(&store, 0x92, 300_000).await;

    let signed_bundle = |coin: &Coin, seed: u8, msg: &[u8]| {
        let sk = SecretKey::key_gen_v3(&[seed; 32], &[]).expect("sk");
        let pk = Bytes48::parse(&sk.sk_to_pk().to_bytes()).expect("pk bytes");
        let sig = sign(&sk, msg);
        let mut bundle = bundle_with_conditions(
            std::slice::from_ref(coin),
            vec![
                SExp::from(&ConditionWithArgs::AggSigUnsafe(
                    pk,
                    Message::new(msg.to_vec()).expect("message"),
                ))
                .to_owned(),
            ],
        );
        bundle.aggregated_signature = Bytes96::parse(&sig.to_bytes()).expect("sig bytes");
        bundle
    };

    let mut mp = mempool_at_peak();
    admit_real(
        &mut mp,
        &store,
        signed_bundle(&coin1, 7, b"t080 first message"),
    )
    .await;
    admit_real(
        &mut mp,
        &store,
        signed_bundle(&coin2, 11, b"t080 second message"),
    )
    .await;

    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("assembles");
    let conds = execute_block_generator_result(&BlockGeneratorInput {
        transactions_generator: tx.program.clone(),
        generator_refs: Vec::new(),
        constants: MAINNET,
        height: BUILD_HEIGHT,
        flags: BlockGeneratorFlags::for_height(&MAINNET, BUILD_HEIGHT),
    })
    .expect("re-runs");
    validate_block_aggregate_signature(&conds, &tx.aggregated_signature, &MAINNET)
        .expect("the aggregated signature of both included bundles verifies against the block's AGG_SIG pairs");
}

// ── Test 6: the post-SF9 emitted-generator form ──────────────────────────────────────────────────
// SF9 SIMPLE_GENERATOR: the generator must be a quoted list (`ff01` prefix), the ref list must
// be empty (TooManyGeneratorRefs), and body rule 2 requires canonical CLVM serialization. The
// byte format is pinned byte-for-byte by core's chia_rs_solution_generator_byte_parity.
#[tokio::test]
async fn post_sf9_generator_form() {
    let store = store().await;
    let coin = seed_coin(&store, 0x93, 250_000).await;
    let mut mp = mempool_at_peak();
    admit_real(&mut mp, &store, fee_bundle(&coin, 10_000)).await;
    let tx = mp
        .create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
        .expect("assembles");
    assert!(
        tx.program.as_ref().starts_with(&[0xff, 0x01]),
        "SF9 SIMPLE_GENERATOR: the generator is a quoted spend list (ff01 prefix)"
    );
    assert!(
        is_canonical_serialization(tx.program.as_ref()),
        "SF9: the emitted generator uses canonical CLVM serialization"
    );
    assert!(tx.block_refs.is_empty(), "SF9: ref_list must be empty");
}

// ── Test 7: empty mempool ⇒ the empty-block path, untouched ──────────────────────────────────────
// No removals ⇒ no generator. The candidate assembles with transactions=None: no generator,
// zero fees/cost, the empty-TransactionsInfo defaults — the path producer_differential pins
// byte-for-byte against 7,953 real mainnet blocks.
#[tokio::test]
async fn empty_mempool_yields_the_unchanged_empty_block() {
    let mp = mempool_at_peak();
    assert!(
        mp.create_block_generator(&MAINNET, BUILD_HEIGHT, TIMEOUT)
            .is_none(),
        "an empty mempool builds no generator"
    );

    let candidate = assemble(None);
    let ti = candidate.transactions_info.expect("tx block ti");
    assert!(candidate.transactions_generator.is_none());
    assert!(candidate.transactions_generator_ref_list.is_empty());
    assert_eq!(ti.fees, 0);
    assert_eq!(ti.cost, 0);
    assert_eq!(
        ti.generator_root,
        Bytes32::default(),
        "no generator => zeros root"
    );
    assert_eq!(
        ti.generator_refs_root,
        Bytes32::from([1u8; 32]),
        "no refs => bytes32([1]*32) (the empty-list sentinel)"
    );
}

#[tokio::test]
async fn no_peak_builds_nothing() {
    let mp = Mempool::new(&MAINNET);
    assert!(mp.peak().is_none());
    assert!(
        mp.create_block_generator(&MAINNET, 1, TIMEOUT).is_none(),
        "no reference frame, no block generator"
    );
}
