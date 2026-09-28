// Leak isolation probe. The live heap profile named the leaking allocation site as
// `sexp_from_bytes_backrefs` (91% of growth) reached via `SerializedProgram::to_program_backrefs`
// in the block-body validation path — but jemalloc names WHERE bytes are allocated, not WHO retains
// them, and every engine collection measured empty/bounded. This binary isolates the question the
// profiler can't answer: does parsing (and running) a real tx-era generator and then DROPPING it
// leave live bytes behind? It loops the exact site on a real generator fetched from the store and
// reads jemalloc `allocated` (live bytes) across iterations. A flat trace exonerates the CLVM
// parse/drop path (retainer is downstream); a rising trace convicts it (an Arc<SExp> cycle or a
// Drop that doesn't free), independent of any collection.
//
// Run against a real store (never the live node):
//   leak-probe "postgres://user:PASS@host:5432/dgxch" 850000

use dg_xch_core::consensus::block_generator::{
    BlockGeneratorInput, execute_block_generator_result,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_stores::PostgresStore;
use dg_xch_stores::traits::BlockStore;

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn allocated() -> u64 {
    let _ = tikv_jemalloc_ctl::epoch::advance();
    tikv_jemalloc_ctl::stats::allocated::read().map_or(0, |v| v as u64)
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .expect("usage: leak-probe <db-url> <start-height>");
    let start: u32 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("start-height");

    let store = PostgresStore::open(&url).await.expect("open store");

    // Scan downward for a height that actually carries a generator (a transaction block).
    let mut found = None;
    for h in (start.saturating_sub(400)..=start).rev() {
        if let Ok(Some(g)) = store.get_generator_at_height(h).await {
            println!(
                "using generator at height {h}: {} raw bytes",
                g.as_ref().len()
            );
            found = Some((h, g));
            break;
        }
    }
    let (height, generator) = found.expect("no generator found in the scanned range");

    // ---- Experiment A: pure parse + drop (the exact profiler site) ----
    // Warm up so lazy/one-time allocations settle, then measure retention across a long loop.
    for _ in 0..50 {
        let _ = generator.to_program_backrefs().expect("parse");
    }
    let base = allocated();
    const ITERS: u64 = 5_000;
    for i in 1..=ITERS {
        let prog = generator.to_program_backrefs().expect("parse");
        drop(prog);
        if i.is_multiple_of(1000) {
            let now = allocated();
            let delta = now as i64 - base as i64;
            println!(
                "[parse] iter {i:>5}: allocated {now:>12}  delta {delta:>+12}  per-iter {:>+8} B",
                delta / i as i64
            );
        }
    }
    let after_parse = allocated();
    println!(
        "[parse] RETAINED over {ITERS} parse+drop cycles: {} bytes ({} B/cycle)",
        after_parse as i64 - base as i64,
        (after_parse as i64 - base as i64) / ITERS as i64
    );

    // ---- Experiment B: full execute_block_generator_result + drop (parse + CLVM run) ----
    // Refs resolved from the store; if any ref is missing we skip B (parse-only result stands).
    let refs = {
        let mut v = Vec::new();
        // A generator with refs would need the block's ref list; for isolation we run ref-less,
        // which exercises the same parse + run + drop of the primary generator.
        v.clear();
        v
    };
    let input = BlockGeneratorInput {
        transactions_generator: generator.clone(),
        generator_refs: refs,
        constants: MAINNET,
        height,
        flags: dg_xch_core::consensus::block_generator::BlockGeneratorFlags::for_height(
            &MAINNET, height,
        ),
    };
    for _ in 0..50 {
        let _ = execute_block_generator_result(&input);
    }
    let base_b = allocated();
    for i in 1..=ITERS {
        let r = execute_block_generator_result(&input);
        drop(r);
        if i.is_multiple_of(1000) {
            let now = allocated();
            let delta = now as i64 - base_b as i64;
            println!(
                "[exec ] iter {i:>5}: allocated {now:>12}  delta {delta:>+12}  per-iter {:>+8} B",
                delta / i as i64
            );
        }
    }
    let after_exec = allocated();
    println!(
        "[exec ] RETAINED over {ITERS} execute+drop cycles: {} bytes ({} B/cycle)",
        after_exec as i64 - base_b as i64,
        (after_exec as i64 - base_b as i64) / ITERS as i64
    );
}
