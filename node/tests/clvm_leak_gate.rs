use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, GeneratorReference, execute_block_generator_result,
};
use dg_xch_core::consensus::constants::MAINNET;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Exact live-byte accounting. A counting allocator over the system allocator has no arena, cache
/// or purge behaviour to threshold around, so "flat" means flat to the byte.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        if new_size > l.size() {
            LIVE.fetch_add(new_size - l.size(), Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(l.size() - new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, l, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Bytes a single run may retain. The allocator is exact, and a correct VM releases everything on
/// runtime drop, so the true value is zero; 64 B absorbs incidental one-time growth in a lazy
/// static without admitting a real per-run leak. The historical bugs were 156,320 B/run and
/// ~90 MB/block — thousands of times over this. Do NOT raise it to make a change pass: at mainnet
/// scale even 1 KB/run is gigabytes, which is exactly how the node OOM-cycled before.
const MAX_RETAINED_BYTES_PER_RUN: usize = 64;

const PLAIN: &str = include_str!("../../core/tests/fixtures/chia_generator_tests/block-834752.txt");
const COMPRESSED: &str =
    include_str!("../../core/tests/fixtures/chia_generator_tests/block-834752-compressed.txt");
const WITH_REF: &str =
    include_str!("../../core/tests/fixtures/chia_generator_tests/block-4671894.txt");
const WITH_REF_ENV: &str =
    include_str!("../../core/tests/fixtures/chia_generator_tests/block-4671894.env");
const HEAVY_49_SPENDS: &str =
    include_str!("../../core/tests/fixtures/heavy_generators/block-9189472.txt");
const HEAVY_20_SPENDS: &str =
    include_str!("../../core/tests/fixtures/heavy_generators/block-9189481.txt");

/// A malicious generator whose loop counter drives it past a VM limit — the error path. A run that
/// aborts must release exactly as much as one that completes; an allocator freed only on the
/// success path leaks on every rejected block, which is attacker-triggerable.
const MALICIOUS: &str = "ff02ffff01ff02ffff01ff04ffff04ffff04ffff01a00101010101010101010101010101010101010101010101010101010101010101ffff04ffff04ffff0101ffff02ff02ffff04ff02ffff04ff05ffff04ff0bffff04ff17ff80808080808080ffff01ff7bffff80ffff018080808080ff8080ff8080ffff04ffff01ff02ffff03ff17ffff01ff04ff05ffff04ff0bffff02ff02ffff04ff02ffff04ff05ffff04ff0bffff04ffff11ff17ffff010180ff8080808080808080ff8080ff0180ff018080ffff04ffff01ff42ff24ff8568656c6c6fffa0010101010101010101010101010101010101010101010101010101010101010180ffff04ffff01ff43ff24ff8568656c6c6fffa0010101010101010101010101010101010101010101010101010101010101010180ffff04ffff01830f4240ff0180808080";

fn first_line(fixture: &str) -> &str {
    fixture.lines().next().expect("fixture has content").trim()
}

fn input_for(hex: &str, height: u32, refs: Vec<GeneratorReference>) -> BlockGeneratorInput {
    BlockGeneratorInput {
        transactions_generator: SerializedProgram::from_hex(first_line(hex))
            .expect("generator fixture is valid hex"),
        generator_refs: refs,
        constants: MAINNET,
        height,
        flags: BlockGeneratorFlags::for_height(&MAINNET, height),
    }
}

/// Run `input` `iters` times and return bytes retained per run. Warms first so one-time lazy
/// allocations are not charged to the measured window. The allocator is exact, so a real leak
/// shows in a handful of iterations — counts are sized to each generator's runtime, not to
/// statistical need.
// The counter is process-global and the default harness runs tests on several threads, so an
// unserialized window counts the other tests' traffic — measured as MB/run of phantom
// retention on a 10-core host. Every measurement takes this lock.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn retained_per_run(input: &BlockGeneratorInput, iters: usize) -> usize {
    for _ in 0..3 {
        let _ = execute_block_generator_result(input);
    }
    let base = LIVE.load(Ordering::Relaxed);
    for _ in 0..iters {
        let _ = execute_block_generator_result(input);
    }
    LIVE.load(Ordering::Relaxed).saturating_sub(base) / iters
}

fn assert_flat(label: &str, retained: usize) {
    eprintln!("  {label}: {retained} B/run retained");
    assert!(
        retained <= MAX_RETAINED_BYTES_PER_RUN,
        "{label} retained {retained} B/run, over the {MAX_RETAINED_BYTES_PER_RUN} B budget — a \
         memory leak. Prior leaks in this path were 156,320 B/run (owned cons pairs) and ~90 MB \
         per block (owned atoms); at mainnet scale even 1 KB/run is gigabytes of RSS. Do NOT relax \
         this threshold to make a change pass."
    );
}

#[test]
fn a_plain_generator_retains_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_flat(
        "plain (834752)",
        retained_per_run(&input_for(PLAIN, 834_752, vec![]), 40),
    );
}

#[test]
fn a_backref_compressed_generator_retains_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The ROM bootstrap / CLVM-side decompression path — the heaviest allocator in the VM.
    assert_flat(
        "compressed (834752)",
        retained_per_run(&input_for(COMPRESSED, 834_752, vec![]), 40),
    );
}

#[test]
fn a_generator_resolving_a_reference_retains_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let refs = vec![GeneratorReference {
        height: 4_671_893,
        index: 0,
        generator: SerializedProgram::from_hex(first_line(WITH_REF_ENV)).expect("ref generator"),
    }];
    assert_flat(
        "with-ref (4671894)",
        retained_per_run(&input_for(WITH_REF, 4_671_894, refs), 40),
    );
}

#[test]
fn cost_maxed_generators_emitting_many_spends_retain_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The site of the 156 KB/run bug: `c` builds the output condition list from owned pairs, so a
    // generator emitting many spends is where owned-pair retention shows up first.
    assert_flat(
        "cost-maxed 49 spends (9189472)",
        retained_per_run(&input_for(HEAVY_49_SPENDS, 9_189_472, vec![]), 6),
    );
    assert_flat(
        "cost-maxed 20 spends (9189481)",
        retained_per_run(&input_for(HEAVY_20_SPENDS, 9_189_481, vec![]), 6),
    );
}

#[test]
fn a_generator_that_fails_mid_run_retains_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Rejected blocks are attacker-triggerable and unbounded in number, so the error path must
    // release exactly like the success path. Two distinct rejection paths:
    //
    // Soak the COST-ROOF rejection under a clamped ceiling — each rejection is cheap, so enough
    // iterations run to expose a per-run leak.
    let mut clamped = MAINNET;
    clamped.max_block_cost_clvm = 20_000_000;
    let input = BlockGeneratorInput {
        transactions_generator: SerializedProgram::from_hex(MALICIOUS).expect("hex"),
        generator_refs: vec![],
        constants: clamped,
        height: 5_000_000,
        flags: BlockGeneratorFlags::for_height(&clamped, 5_000_000),
    };
    assert!(
        execute_block_generator_result(&input).is_err(),
        "the malicious generator must be rejected under a 20M ceiling"
    );
    assert_flat("malicious (cost roof)", retained_per_run(&input, 100));

    // Then take the PAIR-POOL-LIMIT rejection once at full mainnet constants: the runaway loop
    // fills the consensus pair pool (~62.5M pairs) before erroring, so the arena is at maximum
    // size when the error unwinds — the worst case for a release-on-error bug, and far too
    // expensive to soak.
    let full = input_for(MALICIOUS, 5_000_000, vec![]);
    let _ = execute_block_generator_result(&full);
    let base = LIVE.load(Ordering::Relaxed);
    let result = execute_block_generator_result(&full);
    assert!(
        result.is_err(),
        "the pair-pool limit must reject the runaway loop"
    );
    drop(result);
    let retained = LIVE.load(Ordering::Relaxed).saturating_sub(base);
    assert_flat("malicious (pair-pool limit, 1 run)", retained);
}

#[test]
fn the_mempool_admission_path_retains_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // `conditions_from_spend_bundle` is the second VM entry point — every transaction a peer
    // relays goes through it, unboundedly and for free, so a leak here is remotely drainable
    // without ever landing a block. The `1` puzzle echoes its solution as the condition list,
    // covering serialize→run→parse per spend.
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::blockchain::coin_spend::CoinSpend;
    use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
    use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
    use dg_xch_core::blockchain::spend_bundle::SpendBundle;
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::SExp;
    use dg_xch_core::consensus::block_generator::conditions_from_spend_bundle;
    use dg_xch_core::traits::SizedBytes;

    let puzzle = Program::to(1_u8);
    let conditions = [ConditionWithArgs::CreateCoin(
        Bytes32::new([9; 32]),
        900,
        vec![],
    )];
    let solution = Program::to(
        conditions
            .iter()
            .map(|c| SExp::from(c).to_owned())
            .collect::<Vec<_>>(),
    );
    let bundle = SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![CoinSpend {
            coin: Coin {
                parent_coin_info: Bytes32::new([1; 32]),
                puzzle_hash: puzzle.tree_hash(),
                amount: 1000,
            },
            puzzle_reveal: puzzle.serialized().expect("puzzle serializes"),
            solution: solution.serialized().expect("solution serializes"),
        }],
        aggregated_signature: Bytes96::default(),
    };

    conditions_from_spend_bundle(&bundle, 5_000_000, &MAINNET).expect("bundle runs");
    let base = LIVE.load(Ordering::Relaxed);
    const ITERS: usize = 300;
    for _ in 0..ITERS {
        let _ = conditions_from_spend_bundle(&bundle, 5_000_000, &MAINNET);
    }
    let retained = LIVE.load(Ordering::Relaxed).saturating_sub(base) / ITERS;
    assert_flat("mempool admission", retained);
}

#[test]
fn concurrent_validation_retains_nothing() {
    let _serial = MEASURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Window validation runs generators on several threads. Retention here would compound per
    // worker; it also catches anything shared and per-run that outlives a thread.
    let inputs = [
        input_for(PLAIN, 834_752, vec![]),
        input_for(COMPRESSED, 834_752, vec![]),
        input_for(HEAVY_20_SPENDS, 9_189_481, vec![]),
    ];
    for input in &inputs {
        for _ in 0..2 {
            let _ = execute_block_generator_result(input);
        }
    }
    let base = LIVE.load(Ordering::Relaxed);

    const ROUNDS: usize = 8;
    for _ in 0..ROUNDS {
        std::thread::scope(|s| {
            for input in &inputs {
                s.spawn(move || {
                    let _ = execute_block_generator_result(input);
                });
            }
        });
    }
    let retained = LIVE.load(Ordering::Relaxed).saturating_sub(base) / (ROUNDS * inputs.len());
    assert_flat("concurrent (3 shapes)", retained);
}
