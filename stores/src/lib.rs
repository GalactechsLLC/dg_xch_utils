mod arc;
pub mod error;
#[cfg(feature = "mmap")]
pub mod mmap;
#[cfg(feature = "postgres")]
pub mod postgres;
mod record_compat;
pub mod sqlite;
pub mod telemetry;
pub mod traits;
pub mod types;

pub use error::StoreError;
#[cfg(feature = "mmap")]
pub use mmap::MmapStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresStore;
pub use sqlite::SqliteStore;
pub use telemetry::{DURATION_BUCKETS_SECS, HistogramSnapshot, StoreTelemetry};
pub use traits::{BlockStore, CoinStore};
pub use types::{BatchHandle, BlockStatus, Savepoint};

// Sort one block's coin-delta batches by coin_name BEFORE the apply chunks them into
// statements. The names are hashes, so block order is random with respect to the coin_record
// primary key: every row becomes an independent random descent into the multi-GB pkey btree — a
// distinct, uncached leaf page, i.e. a distinct random read, which dominates the confirm on
// high-latency storage. Sorting ONCE over the whole batch and then chunking makes each chunk
// key-contiguous, so its descents share btree path prefixes and touch few distinct leaf pages,
// and gives storage read-ahead something to work with. No semantic effect: the upsert
// (ON CONFLICT / INSERT OR REPLACE over unique names) and the spent-update are
// order-independent within and across chunks.
pub(crate) fn sort_additions_by_name(
    additions: &[dg_xch_core::blockchain::coin_record::CoinRecord],
) -> Vec<(
    dg_xch_core::blockchain::sized_bytes::Bytes32,
    &dg_xch_core::blockchain::coin_record::CoinRecord,
)> {
    use dg_xch_core::traits::SizedBytes;
    // Pair each record with its name so the hash is computed once (it is also the INSERT's key
    // bind), then sort the pairs.
    let mut named: Vec<_> = additions.iter().map(|cr| (cr.coin.name(), cr)).collect();
    named.sort_unstable_by_key(|(name, _)| name.bytes());
    named
}

pub(crate) fn sorted_removal_names(
    removals: &[dg_xch_core::blockchain::sized_bytes::Bytes32],
) -> Vec<dg_xch_core::blockchain::sized_bytes::Bytes32> {
    use dg_xch_core::traits::SizedBytes;
    let mut names = removals.to_vec();
    names.sort_unstable_by_key(|n| n.bytes());
    names
}

// Drop `--` comment lines from a migration file so a ';' inside a comment can never cut a
// statement in half when the deferred index build splits the file into single statements.
pub(crate) fn strip_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    for line in sql.lines() {
        if !line.trim_start().starts_with("--") {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/lib/batch_sort_tests.rs"]
mod batch_sort_tests;
