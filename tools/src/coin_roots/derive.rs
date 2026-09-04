//! Read-only root derivation against a synced store — one streaming pass in canonical
//! `(confirmed_height, coin_id)` order, emitting a [`RootV1`] at each requested boundary.
//!
//! The store traits deliberately expose no coin-set enumeration (point-gets and per-block
//! deltas only), so this layer reads each backend's documented representation directly and
//! STRICTLY read-only:
//!
//! - **Postgres**: one `REPEATABLE READ, READ ONLY` transaction over the `coin_record` /
//!   `block_record` schema (`stores/migrations/postgres/`); the whole derivation — header
//!   hashes and the coin stream — sees a single snapshot, so a live syncing leg is never
//!   disturbed and never observed mid-write.
//! - **SQLite**: a `mode=ro` connection over the identical schema inside one deferred read
//!   transaction (WAL readers snapshot for free).
//! - **mmap**: a standalone parser for the libbitcoin-style file layout documented in
//!   `stores/src/mmap/mod.rs` (`coins.tbl` chained hash table, `coins.dat` frame log,
//!   `heights.dat` dense height index), opened read-only. Entries are resolved by walking
//!   bucket chains exactly as `ChainedTable::find` does (head-first, first match per key),
//!   so crash-orphaned unlinked records are excluded just as the store excludes them.
//!   Reading a LIVE mmap directory is not supported — copy the files first (`coins.tbl`
//!   before `coins.dat`, so every table-referenced frame offset exists in the log copy;
//!   in-place spend updates only ever move a boundary-visible value away from "spent below
//!   the copy-time peak", which the as-of-`H` predicate already excludes for `H` at or
//!   below the copy-time peak).
//!
//! The SQL `ORDER BY confirmed_index ASC, coin_name ASC` is bytewise on both engines
//! (SQLite BLOB = memcmp; Postgres BYTEA = unsigned bytewise), i.e. exactly the canonical
//! order; the accumulator still fail-closes on any misordered row ([`RootsError::OutOfOrder`]).

use super::{CoinSetAccumulator, RootV1, RootsError};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use futures_util::TryStreamExt;
use sqlx::{Connection, Row};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

impl From<sqlx::Error> for RootsError {
    fn from(e: sqlx::Error) -> Self {
        RootsError::Store(e.to_string())
    }
}

/// A parsed store locator: `postgres://…`, `sqlite:///path/to/db`, or `mmap:///dir`.
pub enum StoreUrl {
    Postgres(String),
    Sqlite(PathBuf),
    Mmap(PathBuf),
}

impl StoreUrl {
    /// # Errors
    /// [`RootsError::Store`] on an unrecognized scheme.
    pub fn parse(url: &str) -> Result<Self, RootsError> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Ok(Self::Postgres(url.to_string()))
        } else if let Some(path) = url.strip_prefix("sqlite://") {
            Ok(Self::Sqlite(PathBuf::from(path)))
        } else if let Some(dir) = url.strip_prefix("mmap://") {
            Ok(Self::Mmap(PathBuf::from(dir)))
        } else {
            Err(RootsError::Store(format!(
                "unrecognized store url (want postgres://, sqlite://, mmap://): {url}"
            )))
        }
    }

    #[must_use]
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Postgres(_) => "postgres",
            Self::Sqlite(_) => "sqlite",
            Self::Mmap(_) => "mmap",
        }
    }
}

// The single-pass boundary driver: boundaries ascend; every coin with
// `confirmed_height <= boundaries[k]` is appended before boundary k emits.
struct BoundaryDriver {
    boundaries: Vec<(u32, Bytes32)>,
    next: usize,
    acc: CoinSetAccumulator,
    out: Vec<RootV1>,
}

impl BoundaryDriver {
    fn new(boundaries: Vec<(u32, Bytes32)>) -> Self {
        Self {
            out: Vec::with_capacity(boundaries.len()),
            boundaries,
            next: 0,
            acc: CoinSetAccumulator::new(),
        }
    }

    fn on_coin(
        &mut self,
        coin_id: Bytes32,
        confirmed: u32,
        timestamp: u64,
        spent_index: u32,
    ) -> Result<(), RootsError> {
        while self.next < self.boundaries.len() && confirmed > self.boundaries[self.next].0 {
            let (h, hh) = self.boundaries[self.next];
            self.out.push(self.acc.root_at(h, hh)?);
            self.next += 1;
        }
        self.acc.append(coin_id, confirmed, timestamp, spent_index)
    }

    fn finish(mut self) -> Result<Vec<RootV1>, RootsError> {
        while self.next < self.boundaries.len() {
            let (h, hh) = self.boundaries[self.next];
            self.out.push(self.acc.root_at(h, hh)?);
            self.next += 1;
        }
        Ok(self.out)
    }
}

fn sorted_boundaries(heights: &[u32]) -> Result<Vec<u32>, RootsError> {
    let mut hs: Vec<u32> = heights.to_vec();
    hs.sort_unstable();
    hs.dedup();
    if hs.is_empty() {
        return Err(RootsError::Store("no boundary heights given".into()));
    }
    Ok(hs)
}

/// Derive the v1 root at each of `heights` from the store at `url`. One streaming pass;
/// returns roots in ascending height order.
///
/// # Errors
/// [`RootsError`] on store access failure, a missing main-chain header at a boundary, or a
/// canonical-order violation in the stream.
pub async fn derive(url: &StoreUrl, heights: &[u32]) -> Result<Vec<RootV1>, RootsError> {
    let heights = sorted_boundaries(heights)?;
    match url {
        StoreUrl::Postgres(u) => derive_postgres(u, &heights).await,
        StoreUrl::Sqlite(path) => derive_sqlite(path, &heights).await,
        StoreUrl::Mmap(dir) => derive_mmap(dir, &heights),
    }
}

const COIN_STREAM_SQL_PG: &str = "SELECT coin_name, confirmed_index, spent_index, timestamp \
     FROM coin_record WHERE confirmed_index <= $1 \
     ORDER BY confirmed_index ASC, coin_name ASC";
const HEADER_SQL_PG: &str =
    "SELECT header_hash FROM block_record WHERE height = $1 AND in_main_chain = 1";

async fn derive_postgres(url: &str, heights: &[u32]) -> Result<Vec<RootV1>, RootsError> {
    let mut conn = sqlx::PgConnection::connect(url).await?;
    // One snapshot for headers + coin stream; read-only so a live leg is never disturbed.
    sqlx::query("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut conn)
        .await?;
    let mut boundaries = Vec::with_capacity(heights.len());
    for &h in heights {
        let row = sqlx::query(HEADER_SQL_PG)
            .bind(i64::from(h))
            .fetch_optional(&mut conn)
            .await?
            .ok_or_else(|| RootsError::Store(format!("no main-chain block at height {h}")))?;
        boundaries.push((h, row.try_get::<Bytes32, _>("header_hash")?));
    }
    let max = *heights.last().expect("nonempty");
    let mut driver = BoundaryDriver::new(boundaries);
    {
        let mut rows = sqlx::query(COIN_STREAM_SQL_PG)
            .bind(i64::from(max))
            .fetch(&mut conn);
        let mut seen = 0u64;
        while let Some(row) = rows.try_next().await? {
            let coin_id: Bytes32 = row.try_get("coin_name")?;
            let confirmed: i64 = row.try_get("confirmed_index")?;
            let spent: i64 = row.try_get("spent_index")?;
            let timestamp: i64 = row.try_get("timestamp")?;
            driver.on_coin(
                coin_id,
                u32::try_from(confirmed)
                    .map_err(|_| RootsError::Store(format!("confirmed_index {confirmed}")))?,
                timestamp as u64,
                u32::try_from(spent)
                    .map_err(|_| RootsError::Store(format!("spent_index {spent}")))?,
            )?;
            seen += 1;
            if seen.is_multiple_of(1_000_000) {
                eprintln!("  … {seen} coins (height {confirmed})");
            }
        }
    }
    sqlx::query("COMMIT").execute(&mut conn).await?;
    driver.finish()
}

const COIN_STREAM_SQL_LITE: &str = "SELECT coin_name, confirmed_index, spent_index, timestamp \
     FROM coin_record WHERE confirmed_index <= ? \
     ORDER BY confirmed_index ASC, coin_name ASC";
const HEADER_SQL_LITE: &str =
    "SELECT header_hash FROM block_record WHERE height = ? AND in_main_chain = 1";

async fn derive_sqlite(path: &Path, heights: &[u32]) -> Result<Vec<RootV1>, RootsError> {
    use sqlx::sqlite::SqliteConnectOptions;
    let opts = SqliteConnectOptions::new().filename(path).read_only(true);
    let mut conn = sqlx::SqliteConnection::connect_with(&opts).await?;
    // One read transaction = one WAL snapshot across headers + stream.
    sqlx::query("BEGIN").execute(&mut conn).await?;
    let mut boundaries = Vec::with_capacity(heights.len());
    for &h in heights {
        let row = sqlx::query(HEADER_SQL_LITE)
            .bind(i64::from(h))
            .fetch_optional(&mut conn)
            .await?
            .ok_or_else(|| RootsError::Store(format!("no main-chain block at height {h}")))?;
        boundaries.push((h, row.try_get::<Bytes32, _>("header_hash")?));
    }
    let max = *heights.last().expect("nonempty");
    let mut driver = BoundaryDriver::new(boundaries);
    {
        let mut rows = sqlx::query(COIN_STREAM_SQL_LITE)
            .bind(i64::from(max))
            .fetch(&mut conn);
        while let Some(row) = rows.try_next().await? {
            let coin_id: Bytes32 = row.try_get("coin_name")?;
            let confirmed: i64 = row.try_get("confirmed_index")?;
            let spent: i64 = row.try_get("spent_index")?;
            let timestamp: i64 = row.try_get("timestamp")?;
            driver.on_coin(
                coin_id,
                u32::try_from(confirmed)
                    .map_err(|_| RootsError::Store(format!("confirmed_index {confirmed}")))?,
                timestamp as u64,
                u32::try_from(spent)
                    .map_err(|_| RootsError::Store(format!("spent_index {spent}")))?,
            )?;
        }
    }
    sqlx::query("COMMIT").execute(&mut conn).await?;
    driver.finish()
}

/// Find main-chain sub-epoch-summary boundary heights: for each multiple of `every` up to
/// `max`, the greatest SES-carrying main-chain height at or below it (SQL backends only —
/// the mmap block table does not index SES presence).
///
/// # Errors
/// [`RootsError`] on store access failure or when no SES block exists at or below a target.
pub async fn find_ses_boundaries(
    url: &StoreUrl,
    every: u32,
    max: u32,
) -> Result<Vec<u32>, RootsError> {
    let sql_pg = "SELECT height FROM block_record WHERE in_main_chain = 1 \
         AND sub_epoch_summary IS NOT NULL AND height <= $1 ORDER BY height DESC LIMIT 1";
    let sql_lite = "SELECT height FROM block_record WHERE in_main_chain = 1 \
         AND sub_epoch_summary IS NOT NULL AND height <= ? ORDER BY height DESC LIMIT 1";
    let mut out = Vec::new();
    let mut target = every;
    match url {
        StoreUrl::Postgres(u) => {
            let mut conn = sqlx::PgConnection::connect(u).await?;
            while target <= max {
                let row = sqlx::query(sql_pg)
                    .bind(i64::from(target))
                    .fetch_optional(&mut conn)
                    .await?
                    .ok_or_else(|| {
                        RootsError::Store(format!("no SES block at or below {target}"))
                    })?;
                out.push(u32::try_from(row.try_get::<i64, _>("height")?).expect("height"));
                target += every;
            }
        }
        StoreUrl::Sqlite(path) => {
            use sqlx::sqlite::SqliteConnectOptions;
            let opts = SqliteConnectOptions::new().filename(path).read_only(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts).await?;
            while target <= max {
                let row = sqlx::query(sql_lite)
                    .bind(i64::from(target))
                    .fetch_optional(&mut conn)
                    .await?
                    .ok_or_else(|| {
                        RootsError::Store(format!("no SES block at or below {target}"))
                    })?;
                out.push(u32::try_from(row.try_get::<i64, _>("height")?).expect("height"));
                target += every;
            }
        }
        StoreUrl::Mmap(_) => {
            return Err(RootsError::Store(
                "find-boundaries needs a SQL backend (mmap has no SES index)".into(),
            ));
        }
    }
    out.dedup();
    Ok(out)
}

// ---------------------------------------------------------------------------
// mmap backend: standalone read-only parser of the layout in stores/src/mmap.
// ---------------------------------------------------------------------------

const TBL_HEAD_BYTES: usize = 16;
const TBL_KEY: usize = 32;
const TBL_NEXT: usize = 8;
const COIN_PAYLOAD: usize = 16;
const TBL_RECORD: usize = TBL_KEY + TBL_NEXT + COIN_PAYLOAD;

struct MmapCoin {
    key: [u8; 32],
    confirmed: u32,
    spent: u32,
    off: u64,
}

fn store_io(what: &str, e: impl std::fmt::Display) -> RootsError {
    RootsError::Store(format!("{what}: {e}"))
}

// Chain-walk the copied `coins.tbl` exactly as ChainedTable::find resolves entries:
// bucket head first, first match per key wins; unlinked (crash-orphaned) records and
// reorg-sentinel entries (offset field 0) are excluded.
fn read_coins_tbl(dir: &Path, max_height: u32) -> Result<Vec<MmapCoin>, RootsError> {
    let tbl = std::fs::read(dir.join("coins.tbl")).map_err(|e| store_io("coins.tbl", e))?;
    if tbl.len() < TBL_HEAD_BYTES {
        return Err(RootsError::Store("coins.tbl: short header".into()));
    }
    let count = u64::from_le_bytes(tbl[0..8].try_into().expect("header"));
    let buckets = u64::from_le_bytes(tbl[8..16].try_into().expect("header"));
    if buckets == 0 || !buckets.is_power_of_two() {
        return Err(RootsError::Store(format!(
            "coins.tbl: bucket count {buckets}"
        )));
    }
    let records_base = TBL_HEAD_BYTES + (buckets as usize) * 8;
    if tbl.len() < records_base {
        return Err(RootsError::Store(
            "coins.tbl: truncated bucket heads".into(),
        ));
    }
    let record_at = |index: u64| -> Result<&[u8], RootsError> {
        let start = records_base + (index as usize) * TBL_RECORD;
        tbl.get(start..start + TBL_RECORD)
            .ok_or_else(|| RootsError::Store(format!("coins.tbl: record {index} out of range")))
    };
    let mut out = Vec::new();
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    for b in 0..buckets as usize {
        let head_off = TBL_HEAD_BYTES + b * 8;
        let mut link = u64::from_le_bytes(tbl[head_off..head_off + 8].try_into().expect("head"));
        while link != 0 {
            let index = link - 1;
            if index >= count {
                return Err(RootsError::Store(format!(
                    "coins.tbl: chain link {index} beyond record count {count}"
                )));
            }
            let rec = record_at(index)?;
            let key: [u8; 32] = rec[..TBL_KEY].try_into().expect("key");
            link = u64::from_le_bytes(rec[TBL_KEY..TBL_KEY + TBL_NEXT].try_into().expect("next"));
            if !seen.insert(key) {
                continue; // shadowed duplicate: find() returns the first match only
            }
            let payload = &rec[TBL_KEY + TBL_NEXT..];
            let off_plus_1 = u64::from_le_bytes(payload[0..8].try_into().expect("payload"));
            let Some(off) = off_plus_1.checked_sub(1) else {
                continue; // reorg-deleted sentinel
            };
            let spent = u32::from_le_bytes(payload[8..12].try_into().expect("payload"));
            let confirmed = u32::from_le_bytes(payload[12..16].try_into().expect("payload"));
            if confirmed <= max_height {
                out.push(MmapCoin {
                    key,
                    confirmed,
                    spent,
                    off,
                });
            }
        }
    }
    Ok(out)
}

// `coins.dat` frame at `off`: [u32 LE len][Coin ChiaSerialize][coinbase:1][confirmed:4 LE]
// [timestamp:8 LE] — the timestamp is the last 8 bytes, and the frame's confirmed field must
// agree with the table payload (torn-copy guard).
fn frame_timestamp(dat: &[u8], coin: &MmapCoin) -> Result<u64, RootsError> {
    let start = usize::try_from(coin.off).map_err(|_| RootsError::Store("offset".into()))?;
    let hdr = dat
        .get(start..start + 4)
        .ok_or_else(|| RootsError::Store(format!("coins.dat: frame offset {start} torn")))?;
    let len = u32::from_le_bytes(hdr.try_into().expect("len")) as usize;
    let frame = dat
        .get(start + 4..start + 4 + len)
        .ok_or_else(|| RootsError::Store(format!("coins.dat: torn frame at {start}")))?;
    if len < 13 {
        return Err(RootsError::Store(format!(
            "coins.dat: short frame at {start}"
        )));
    }
    let confirmed = u32::from_le_bytes(frame[len - 12..len - 8].try_into().expect("frame"));
    if confirmed != coin.confirmed {
        return Err(RootsError::Store(format!(
            "coins.dat: frame confirmed {confirmed} != table {} at {start} (torn copy?)",
            coin.confirmed
        )));
    }
    Ok(u64::from_le_bytes(
        frame[len - 8..].try_into().expect("frame"),
    ))
}

fn derive_mmap(dir: &Path, heights: &[u32]) -> Result<Vec<RootV1>, RootsError> {
    let max = *heights.last().expect("nonempty");
    // heights.dat: dense height -> main-chain header hash, 32 bytes per height, zero = vacant.
    let heights_dat =
        std::fs::read(dir.join("heights.dat")).map_err(|e| store_io("heights.dat", e))?;
    let mut boundaries = Vec::with_capacity(heights.len());
    for &h in heights {
        let start = (h as usize) * 32;
        let slot = heights_dat
            .get(start..start + 32)
            .ok_or_else(|| RootsError::Store(format!("heights.dat: no slot for height {h}")))?;
        if slot == [0u8; 32] {
            return Err(RootsError::Store(format!(
                "no main-chain block at height {h}"
            )));
        }
        let arr: [u8; 32] = slot.try_into().expect("32-byte slot");
        boundaries.push((h, Bytes32::from(arr)));
    }
    let mut coins = read_coins_tbl(dir, max)?;
    let dat = std::fs::read(dir.join("coins.dat")).map_err(|e| store_io("coins.dat", e))?;
    coins.sort_unstable_by_key(|c| (c.confirmed, c.key));
    let mut driver = BoundaryDriver::new(boundaries);
    for coin in &coins {
        let timestamp = frame_timestamp(&dat, coin)?;
        driver.on_coin(
            Bytes32::from(coin.key),
            coin.confirmed,
            timestamp,
            coin.spent,
        )?;
    }
    driver.finish()
}
