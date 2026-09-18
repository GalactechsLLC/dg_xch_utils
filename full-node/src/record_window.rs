// The server's record window: the in-process ancestor cache behind every consensus-walk record
// map (`difficulty_records_map`).
//
// A per-call store walk re-fetches the walk window from the backend every time: 513..=896
// sequential point reads mid-epoch, and 5,121..=5,503 for every anchor whose next height sits in
// an epoch's FIRST sub-epoch (`difficulty_record_depth`'s epoch-turn regime). On the Postgres
// backends each point read is a network round trip.
//
// This module serves the same walk from a bounded in-memory window and touches the store only for
// records the window has not yet seen: the whole window once at cold start, then just the new
// head delta per peak (and a below-floor tail in the rare trailing-anchor case). Records are
// immutable per header hash, so the by-hash cache can never serve a stale value; a reorg simply
// makes the walk fetch the fork branch's new hashes from the store (cache misses), and
// `BlockRecordCache::insert` evicts the superseded sibling at each overwritten height.

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::difficulty_adjustment::difficulty_record_depth;
use dg_xch_node::{BlockRecordCache, SyncMetrics};
use dg_xch_stores::BlockStore;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;

// Window capacity: the shared constants-derived walk window
// (`core::consensus::difficulty_adjustment::consensus_walk_window`) — one source of truth with
// the node engine's walk cache. Mainnet: 4608 + 384 + 6*128 = 5,760 records ≈ 6 MiB.
// (`capacity_covers_every_depth_with_trailing_slack` pins the inequality against the real depth
// function rather than trusting the arithmetic.)
#[must_use]
pub(crate) fn record_window_capacity(constants: &ConsensusConstants) -> usize {
    dg_xch_core::consensus::difficulty_adjustment::consensus_walk_window(constants)
}

// The ancestor record map for the consensus walks, `depth`-sized by `difficulty_record_depth`,
// served from the shared window with store fallback per miss. The map is identical to a pure store
// walk (same chain, same break-on-miss and stop-at-genesis semantics): the window only ever holds
// records fetched from this same store, keyed by header hash, and a record is immutable per hash.
// Fetched misses are folded back into the window (ascending
// height, so `BlockRecordCache`'s lowest-first eviction keeps the window contiguous below the
// newest head).
pub(crate) async fn windowed_records_map<S: BlockStore + Send + Sync>(
    window: &Mutex<BlockRecordCache>,
    store: &S,
    metrics: &SyncMetrics,
    constants: &ConsensusConstants,
    anchor: &BlockRecord,
) -> HashMap<Bytes32, BlockRecord> {
    let depth = difficulty_record_depth(constants, anchor.height);
    let mut map: HashMap<Bytes32, BlockRecord> = HashMap::with_capacity(depth as usize);
    let mut fetched: Vec<BlockRecord> = Vec::new();
    let mut hash = anchor.header_hash;
    let mut hits: u64 = 0;
    let mut reads: u64 = 0;
    for _ in 0..depth {
        // Short per-step lock; the store await below runs with the window unlocked.
        let cached = { window.lock().await.get(&hash).cloned() };
        let rec = match cached {
            Some(r) => {
                hits += 1;
                r
            }
            None => {
                let Ok(Some(r)) = store.get_block_record(&hash).await else {
                    break;
                };
                reads += 1;
                fetched.push(r.clone());
                r
            }
        };
        let prev = rec.prev_hash;
        let height = rec.height;
        map.insert(hash, rec);
        if height == 0 {
            break;
        }
        hash = prev;
    }
    if !fetched.is_empty() {
        let mut guard = window.lock().await;
        for rec in fetched.into_iter().rev() {
            guard.insert(rec);
        }
    }
    metrics
        .difficulty_window_cache_hits
        .fetch_add(hits, Ordering::Relaxed);
    metrics
        .difficulty_window_store_reads
        .fetch_add(reads, Ordering::Relaxed);
    map
}

#[cfg(test)]
#[path = "../tests/unit/record_window.rs"]
mod tests;
