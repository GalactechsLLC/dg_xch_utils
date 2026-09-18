mod common;

use dg_xch_core::clvm::parser::sexp_to_bytes;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::clvm::sexp::{AtomBuf, SExp};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{Engine, NativePrimitives};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

// Build a sizeable, unique CLVM generator program for `seed`, driven through the exact wire
// decode the live path uses: SExp tree -> `sexp_to_bytes` -> `SerializedProgram::from_bytes`
// (which runs `sexp_from_bytes_backrefs` to bound the program, then keeps the raw bytes). A
// trailing 0xfe back-reference cell exercises the back-reference decoder specifically — the
// production generators are back-reference compressed. ~17 KiB each, so retaining N of them is
// visibly unbounded heap.
fn synthetic_generator(seed: u32) -> SerializedProgram {
    let mut items: Vec<SExp<'static>> = Vec::with_capacity(256);
    for i in 0..256u32 {
        let mut atom = vec![0u8; 64];
        atom[0..4].copy_from_slice(&seed.to_be_bytes());
        atom[4..8].copy_from_slice(&i.to_be_bytes());
        items.push(SExp::Atom(AtomBuf::new(atom)));
    }
    // A proper list of the atoms, then round-trip through the back-reference-aware wire decode so
    // the retained bytes are produced exactly as a peer-fetched ref generator is.
    let bytes = sexp_to_bytes(&SExp::from(items)).expect("serialize synthetic generator");
    let mut cursor = Cursor::new(bytes.as_ref());
    let decoded = <SerializedProgram as ChiaSerialize>::from_bytes(
        &mut cursor,
        ChiaProtocolVersion::default(),
    )
    .expect("decode via sexp_from_bytes_backrefs");
    decoded
        .to_program_backrefs()
        .expect("back-reference decode of synthetic generator");
    decoded
}

// The `--sync-from` server fetches every out-of-span generator back-ref from a peer and seeds it
// into the engine via `seed_generator`; those heights live BELOW the anchor span, so they are never
// in this node's store and never confirmed. Two invariants must hold, and a prior fix got the
// second right while breaking the first — walling every anchored node.
//
// INVARIANT 1 (correctness — the wall): a seeded out-of-span ref MUST resolve through the exact
// validation path a real block uses, `resolve_generator_refs`. The regression resolved refs via the
// raw `staged_generators` map (in-window only) then the store — never consulting the seed cache —
// so an anchored block referencing a generator below the anchor hit `GeneratorRefHasNoGenerator`
// forever (live: a node walled at 4,761,236, whose ref points to height 4,413,681, 347k blocks back).
//
// INVARIANT 2 (retention): the seed cache must not grow with sync length. It holds only the current
// window's refs and is wiped per window (`clear_seed_generators`), so N distinct seeds across the
// whole sync never accumulate. (A prior FIFO cap bounded retention but could evict a ref the current
// window still needed — trading the leak for the wall; the per-window clear does neither.)
//
// The offline era-replay harness never exercises this: it seeds ref blocks into the STORE, so
// `missing_ref_heights` resolves them and `seed_generator` is never called. Hence flat offline,
// walled/leaking live — this test closes that gap.
#[tokio::test]
async fn seeded_out_of_span_ref_resolves_through_the_validation_path() {
    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);

    // A ref far below any anchor — never in the store, only ever available via the peer-fetched
    // seed cache, exactly like the live wall's 4,413,681.
    const DEEP_REF: u32 = 4_413_681;
    let generator = synthetic_generator(DEEP_REF);
    engine.seed_generator(DEEP_REF, generator);

    // The wall: `resolve_generator_refs` is the path block-body validation actually uses. Before the
    // fix it consulted only `staged_generators` + store and returned GeneratorRefHasNoGenerator here.
    let resolved = engine
        .resolve_generator_refs(&[DEEP_REF])
        .await
        .expect("a seeded out-of-span ref must resolve through resolve_generator_refs (the wall)");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].height, DEEP_REF);
}

#[tokio::test]
async fn seed_overlay_is_bounded_per_window_by_clear() {
    let store = common::new_store().await;
    let mut engine = Engine::new(store, NativePrimitives, MAINNET);

    // Seed many distinct out-of-span refs (one window's worth in the extreme). Within a window the
    // overlay legitimately holds them all — the bound is the per-window CLEAR, not an internal cap.
    const SEEDS: u32 = 6_000;
    for h in 0..SEEDS {
        engine.seed_generator(h, synthetic_generator(h));
    }
    let (_c, _p, before) = engine.collection_sizes();
    assert_eq!(before, SEEDS as usize, "in-window seeds are all held");

    // The server calls this at the start of every window: retention drops to zero regardless of how
    // many distinct historical refs the whole sync has streamed — no accumulation with sync length.
    engine.clear_seed_generators();
    let (_c, _p, after) = engine.collection_sizes();
    assert_eq!(
        after, 0,
        "the per-window clear must empty the seed overlay (bounded independent of sync length)"
    );
}
