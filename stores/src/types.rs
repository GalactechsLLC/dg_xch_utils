use dg_xch_core::blockchain::sized_bytes::Bytes32;
use sqlx::SqliteConnection;
use tokio::sync::OwnedMutexGuard;

pub struct CoinChanges<'a> {
    pub height: u32,
    pub timestamp: u64,
    pub additions: &'a [dg_xch_core::blockchain::coin_record::CoinRecord],
    pub removals: &'a [Bytes32],
    pub hints: &'a [(Bytes32, Bytes32)],
}

#[derive(Clone)]
pub struct OwnedCoinChanges {
    pub height: u32,
    pub timestamp: u64,
    pub additions: Vec<dg_xch_core::blockchain::coin_record::CoinRecord>,
    pub removals: Vec<Bytes32>,
    pub hints: Vec<(Bytes32, Bytes32)>,
}

impl OwnedCoinChanges {
    pub fn borrowed(&self) -> CoinChanges<'_> {
        CoinChanges {
            height: self.height,
            timestamp: self.timestamp,
            additions: &self.additions,
            removals: &self.removals,
            hints: &self.hints,
        }
    }
}

pub enum PreparedCoinWindow {
    Native(Vec<OwnedCoinChanges>),
    Sqlite {
        additions: Vec<(Bytes32, dg_xch_core::blockchain::coin_record::CoinRecord)>,
        removals: Vec<(Bytes32, u32)>,
        hints: Vec<(Bytes32, Bytes32)>,
    },
}

pub enum PreparedArchive {
    Native {
        records: Vec<(
            dg_xch_core::blockchain::block_record::BlockRecord,
            BlockStatus,
        )>,
        blocks: Vec<dg_xch_core::blockchain::full_block::FullBlock>,
    },
    Sqlite {
        records: Vec<EncodedRecord>,
        bodies: Vec<(Bytes32, Vec<u8>)>,
    },
}

pub struct EncodedRecord {
    pub hash: Bytes32,
    pub parent: Bytes32,
    pub height: i64,
    pub weight: Vec<u8>,
    pub iterations: Vec<u8>,
    pub transaction: i64,
    pub summary: Option<Vec<u8>>,
    pub record: Vec<u8>,
    pub status: i64,
}

/// Durable per-block validation state, stored as a u8 in `block_record.status`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockStatus {
    Unvalidated,
    Validated,
    Bypass,
}
impl BlockStatus {
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            BlockStatus::Unvalidated => 0,
            BlockStatus::Validated => 1,
            BlockStatus::Bypass => 2,
        }
    }
    #[must_use]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => BlockStatus::Validated,
            2 => BlockStatus::Bypass,
            _ => BlockStatus::Unvalidated,
        }
    }
}

/// An open write batch: one `commit` = one fsync. Backend-tagged: the SQLite arm owns the single
/// writer connection for the batch's lifetime (sqlite is single-writer, WAL readers proceed
/// lock-free); the Postgres arm is an ordinary transaction from the pool (Postgres is multi-writer).
/// A handle only commits against the backend that opened it.
pub struct BatchHandle {
    pub(crate) inner: BatchInner,
    pub(crate) _timing: Option<crate::telemetry::OperationTimer>,
}

impl BatchHandle {
    // Borrow the batch's held SQLite writer connection; fail-closed on a cross-backend handle.
    pub(crate) fn sqlite_conn(
        &mut self,
    ) -> Result<&mut SqliteConnection, crate::error::StoreError> {
        #[allow(irrefutable_let_patterns)]
        let BatchInner::Sqlite(conn) = &mut self.inner else {
            return Err(crate::error::StoreError::Corrupt(
                "batch was opened by a different backend".to_string(),
            ));
        };
        Ok(&mut **conn)
    }

    #[cfg(feature = "postgres")]
    // Borrow the batch's open Postgres transaction connection; fail-closed on a cross-backend handle.
    pub(crate) fn pg_conn(&mut self) -> Result<&mut sqlx::PgConnection, crate::error::StoreError> {
        let BatchInner::Postgres(tx) = &mut self.inner else {
            return Err(crate::error::StoreError::Corrupt(
                "batch was opened by a different backend".to_string(),
            ));
        };
        Ok(&mut **tx)
    }

    #[cfg(feature = "mmap")]
    // Assert the batch belongs to the mmap backend.
    pub(crate) fn require_mmap(&self) -> Result<(), crate::error::StoreError> {
        let BatchInner::Mmap(_) = &self.inner else {
            return Err(crate::error::StoreError::Corrupt(
                "batch was opened by a different backend".to_string(),
            ));
        };
        Ok(())
    }

    #[cfg(feature = "mmap")]
    // Borrow the batch's staged mmap coin links; fail-closed on a cross-backend handle.
    pub(crate) fn mmap_batch(&mut self) -> Result<&mut MmapBatch, crate::error::StoreError> {
        let BatchInner::Mmap(b) = &mut self.inner else {
            return Err(crate::error::StoreError::Corrupt(
                "batch was opened by a different backend".to_string(),
            ));
        };
        Ok(b)
    }
}

#[cfg(feature = "mmap")]
/// Coin-table mutations staged under an open mmap batch. `apply_block_in` appends coin log frames
/// immediately but defers EVERY table write here — new-coin links, in-place spends of pre-existing
/// coins, and replays alike stage an absolute payload image; `rollback_to_in` stages the reorg's
/// fork sweep. The batch's durability point (`set_peak_in` before the peak walk, or `commit` for
/// peak-less batches) publishes everything with ONE log fsync + ONE ordering msync + ONE
/// table msync for the whole batch — the libbitcoin sync-before-link rule applied at the batch
/// boundary instead of per coin. Bounded by the confirm window's coin count (the engine confirms
/// at most one window per batch). A dropped batch therefore loses ONLY unreferenced log frames
/// (append-only leak, invisible to every read path): the logical store is untouched — the
/// transactions-free analog of a rolled-back reorg transaction (T0-4). A staged sweep additionally
/// arms the on-disk reorg journal at publish time (see `mmap/mod.rs`), so a crash INSIDE the
/// publish converges at the next open instead of leaving reverted coins under an unmoved peak.
#[derive(Default)]
pub struct MmapBatch {
    // (coin id, packed CoinEntry payload), in insertion order; `index` maps a coin id to its
    // slot so a same-batch spend (dust-era ephemeral coins) updates the staged payload in place.
    // Images are ABSOLUTE (last-writer-wins at publish), so they land correctly over the staged
    // sweep's in-place reverts.
    pub(crate) pending_coins: Vec<(Bytes32, [u8; 16])>,
    pub(crate) pending_index: std::collections::HashMap<Bytes32, usize>,
    // The reorg's fork revert, staged by `rollback_to_in`: computed against the pre-branch table
    // (nothing staged is published yet, so scanning at stage time and at publish time see the
    // same state), applied at the batch's durability point BEFORE the staged images.
    pub(crate) sweep: Option<StagedSweep>,
}

#[cfg(feature = "mmap")]
// The precomputed fork revert of one staged reorg (see [`MmapBatch::sweep`]).
pub(crate) struct StagedSweep {
    pub(crate) fork_height: u32,
    // The main-chain hash at the fork at stage time — the convergence peak a crashed publish
    // rolls back to (None: no main-chain block at the fork; converge to an empty peak).
    pub(crate) fork_hash: Option<Bytes32>,
    // Coins created above the fork (sentinel-delete) and coins spent above it (un-spend). The
    // deletion set is consulted by `apply_block_in`'s staged-spend path: a branch removal that
    // references a coin the sweep deletes must stay a no-op (the SQL backends get this free —
    // their in-transaction DELETE lands before the branch UPDATE matches nothing), never a
    // resurrection of the swept entry.
    pub(crate) deletions: std::collections::HashSet<Bytes32>,
    pub(crate) unspends: Vec<Bytes32>,
}

pub(crate) enum BatchInner {
    Sqlite(OwnedMutexGuard<SqliteConnection>),
    #[cfg(feature = "postgres")]
    Postgres(sqlx::Transaction<'static, sqlx::Postgres>),
    // The mmap backend's per-batch resource is the staged coin-link set; appends serialize on
    // the log files and commit/set_peak_in is the durability point.
    #[cfg(feature = "mmap")]
    Mmap(MmapBatch),
}

/// A reorg boundary: the confirmed peak captured at `savepoint` time. `rollback` restores it by flipping
/// confirmation pointers back (RAM O(1) — the changed set is never materialized).
#[derive(Clone, Copy, Debug)]
pub struct Savepoint {
    pub(crate) peak: Option<(Bytes32, u32)>,
}
