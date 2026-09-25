// The restart-resume record-floor decision: can the next possible epoch retarget's deepest record
// read (`epoch_backfill_low`) be served from the store, or does the resume repair need to fetch a
// weight proof and backfill headers first?
//
// The floor MUST be measured by prev-hash walk from the peak, for two reasons:
//   1. `get_block_record_by_height` is main-chain-only on every backend (sqlite/postgres
//      `in_main_chain = 1`; mmap `heights.dat` is populated only by `set_peak`). Epoch-depth
//      backfill writes CANDIDATE records, which a by-height read never sees, so a by-height floor
//      sits at the confirmed span base forever and every restart re-runs the weight-proof fetch,
//      validation and header backfill it had already done.
//   2. A by-height binary search assumes hole-free monotone presence. A mid-span record hole (a
//      lost candidate link batch on mmap; a `synchronous_commit=off` suffix drop on Postgres)
//      above the by-height floor reads as "nothing to repair" while the stage walk keeps dying on
//      the hole, which livelocks the resume.
// The prev-hash walk reads records BY HASH (candidate-visible on every backend) and detects a hole
// naturally: the walk breaks exactly at the record whose parent is missing.

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_stores::BlockStore;
use dg_xch_stores::error::StoreError;

// Step bound for the floor walk: the needed span plus slack. Heights decrease by exactly one per
// prev-hash link on a well-formed chain, so this is never the limiting factor there — it only
// guarantees termination against a corrupt store (a prev-hash cycle), reported as Broken.
const FLOOR_WALK_SLACK: u32 = 16;

/// The outcome of the record-floor walk from the peak toward `needed_low`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordFloor {
    /// The prev-hash chain from the peak reaches `needed_low` (or genesis) unbroken: the next
    /// epoch retarget's deepest read is servable. `floor` is the height at which the walk crossed
    /// `needed_low` — the deepen anchor when a re-armed repair must reach further down.
    Reached { floor: u32 },
    /// The chain breaks above `needed_low`: `stop` is the lowest record still reachable from the
    /// peak; the record below it is missing (an anchored span's un-backfilled base, or a mid-span
    /// hole). Backfilling headers anchored at `stop` covers the missing span.
    Broken { stop: u32 },
}

/// Measure the record floor for a resume over peak `(peak, local)`.
///
/// # Errors
/// Returns [`StoreError`] on a store read failure.
pub(crate) async fn measure_record_floor<S: BlockStore + Send + Sync + ?Sized>(
    store: &S,
    peak: Bytes32,
    local: u32,
    needed_low: u32,
) -> Result<RecordFloor, StoreError> {
    // Bounded: heights decrease by exactly one per link on a well-formed chain, so the walk takes
    // at most `local - needed_low + 1` steps; the slack only bounds a corrupt store (see above).
    let max_steps = u64::from(local.saturating_sub(needed_low)) + u64::from(FLOOR_WALK_SLACK);
    let mut hash = peak;
    let mut lowest_reached = local.saturating_add(1);
    let mut steps = 0u64;
    loop {
        if steps > max_steps {
            // A prev-hash cycle or a non-decreasing height chain: report the break at the lowest
            // height legitimately reached so the repair re-fetches below it.
            return Ok(RecordFloor::Broken {
                stop: lowest_reached,
            });
        }
        let Some(rec) = store.get_block_record(&hash).await? else {
            return Ok(RecordFloor::Broken {
                stop: lowest_reached,
            });
        };
        if rec.height <= needed_low {
            return Ok(RecordFloor::Reached { floor: rec.height });
        }
        lowest_reached = lowest_reached.min(rec.height);
        hash = rec.prev_hash;
        steps += 1;
    }
}

#[cfg(test)]
#[path = "../tests/unit/resume_floor.rs"]
mod tests;
