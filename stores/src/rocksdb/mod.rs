//! Embedded full-node storage. V1 keeps all indexes live and stages one atomic write batch.
mod block;
mod coin;

use crate::{BatchHandle, StoreError};
use ::rocksdb::{DB, Direction, IteratorMode, Options, Snapshot, WriteBatch, WriteOptions};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{OwnedMutexGuard, Semaphore};

const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;
type Pending = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

// One column family with distinct key prefixes avoids per-family memory budgets in v1.
const COIN: u8 = b'c';
const RECORD: u8 = b'r';
const BODY: u8 = b'b';
const STATUS: u8 = b's';
const MAIN: u8 = b'm';
const MISSING: u8 = b'n';
const CONFIRMED: u8 = b'a';
const SPENT: u8 = b'd';
#[cfg(feature = "coin-index")]
const PUZZLE: u8 = b'p';
#[cfg(feature = "coin-index")]
const PARENT: u8 = b'q';
#[cfg(feature = "hint")]
const HINT: u8 = b'h';
const SEGMENTS: u8 = b'e';
const PEAK: &[u8] = b"!peak";
const SCHEMA: &[u8] = b"!schema";
const FEATURES: &[u8] = b"!features";

pub struct RocksDbStore {
    db: Arc<DB>,
    options: Arc<Options>,
    writer: Arc<tokio::sync::Mutex<()>>,
    workers: Arc<Semaphore>,
    near_tip: AtomicBool,
}

pub(crate) struct RocksBatch {
    db: Arc<DB>,
    state: Mutex<BatchState>,
    options: Arc<Options>,
    in_flight: AtomicUsize,
    // Workers retain this Arc, so cancellation never releases the writer during database work.
    _writer: OwnedMutexGuard<()>,
    started: Instant,
}

#[derive(Default)]
struct BatchState {
    pending: Pending,
    failed: bool,
    finished: bool,
}

// Retained by each worker, including after its async caller has been cancelled.
struct StageLease(Arc<RocksBatch>);
impl Drop for StageLease {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for RocksBatch {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut()
            && !state.finished
            && !state.pending.is_empty()
        {
            log::info!(
                "rocksdb abandoned_batch pending_keys={} failed={} writer_hold_us={}",
                state.pending.len(),
                state.failed,
                self.started.elapsed().as_micros()
            );
        }
    }
}

impl From<::rocksdb::Error> for StoreError {
    fn from(error: ::rocksdb::Error) -> Self {
        Self::Rocksdb(error)
    }
}

fn key(prefix: u8, suffix: impl AsRef<[u8]>) -> Vec<u8> {
    let mut key = vec![prefix];
    key.extend_from_slice(suffix.as_ref());
    key
}

fn index(prefix: u8, group: impl AsRef<[u8]>, id: &Bytes32) -> Vec<u8> {
    let mut key = key(prefix, group);
    key.extend_from_slice(id.as_ref());
    key
}

fn hash(bytes: &[u8]) -> Result<Bytes32, StoreError> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| StoreError::Corrupt("invalid rocksdb hash length".into()))?;
    Ok(Bytes32::from(bytes))
}

fn height(bytes: &[u8]) -> Result<u32, StoreError> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| StoreError::Corrupt("invalid rocksdb height length".into()))?;
    Ok(u32::from_be_bytes(bytes))
}

fn decode<T: ChiaSerialize>(bytes: &[u8]) -> Result<T, StoreError> {
    let mut cursor = Cursor::new(bytes);
    let value = T::from_bytes(&mut cursor, VERSION)?;
    if cursor.position() != bytes.len() as u64 {
        return Err(StoreError::Corrupt("trailing rocksdb value bytes".into()));
    }
    Ok(value)
}

struct View<'a> {
    snapshot: Snapshot<'a>,
    pending: &'a mut Pending,
}

impl View<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        match self.pending.get(key) {
            Some(value) => Ok(value.clone()),
            None => Ok(self.snapshot.get(key)?),
        }
    }

    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.pending.insert(key, Some(value));
    }

    fn delete(&mut self, key: Vec<u8>) {
        self.pending.insert(key, None);
    }

    // Stream the sorted merge of committed keys and the batch overlay. Callers stop in-scan.
    fn scan(
        &self,
        prefix: &[u8],
        visit: impl FnMut(&[u8], &[u8]) -> Result<bool, StoreError>,
    ) -> Result<(), StoreError> {
        self.scan_from(prefix, prefix, visit)
    }

    fn scan_from(
        &self,
        prefix: &[u8],
        start: &[u8],
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<bool, StoreError>,
    ) -> Result<(), StoreError> {
        let mut disk = self
            .snapshot
            .iterator(IteratorMode::From(start, Direction::Forward))
            .peekable();
        let mut staged = self
            .pending
            .range(start.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .peekable();
        loop {
            let disk_key = match disk.peek() {
                Some(Ok((k, _))) if k.starts_with(prefix) => Some(k.as_ref()),
                Some(Err(_)) => {
                    let Err(error) = disk.next().unwrap() else {
                        unreachable!()
                    };
                    return Err(error.into());
                }
                _ => None,
            };
            let staged_key = staged.peek().map(|(k, _)| k.as_slice());
            match (disk_key, staged_key) {
                (None, None) => break,
                (Some(a), Some(b)) if a >= b => {
                    let equal = a == b;
                    let (k, value) = staged.next().unwrap();
                    if equal {
                        disk.next();
                    }
                    if let Some(value) = value
                        && !visit(k, value)?
                    {
                        break;
                    }
                }
                (None, Some(_)) => {
                    let (k, value) = staged.next().unwrap();
                    if let Some(value) = value
                        && !visit(k, value)?
                    {
                        break;
                    }
                }
                (Some(_), _) => {
                    let (k, value) = disk.next().unwrap()?;
                    if !visit(&k, &value)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

impl RocksDbStore {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        let path = path.to_owned();
        let (db, options) = tokio::task::spawn_blocking(move || -> Result<(DB, Options), StoreError> {
            let started = Instant::now();
            std::fs::create_dir_all(&path)?;
            let mut options = Options::default();
            options.create_if_missing(true);
            options.enable_statistics();
            options.set_compression_type(::rocksdb::DBCompressionType::None);
            let db = DB::open(&options, &path)?;
            // Service indexes are maintained from the first write. Refuse feature upgrades that
            // would silently serve incomplete indexes; a fresh DB is the v1 upgrade path.
            let features = [u8::from(cfg!(feature = "coin-index")), u8::from(cfg!(feature = "hint"))];
            match db.get(SCHEMA)? {
                Some(version) if version != b"1" => return Err(StoreError::Corrupt("unsupported rocksdb schema version".into())),
                Some(_) => {
                    let stored = db.get(FEATURES)?.ok_or_else(|| StoreError::Corrupt("missing rocksdb feature metadata".into()))?;
                    if stored.len() != features.len() || stored.iter().zip(features).any(|(old, new)| *old < new) {
                        return Err(StoreError::Batch("rocksdb service tier upgrade requires a fresh database in v1".into()));
                    }
                    // Downgrades are also refused: otherwise subsequent writes leave stale indexes.
                    if stored != features { return Err(StoreError::Batch("rocksdb feature tier differs from the database; use the original tier or a fresh directory".into())); }
                }
                None => {
                    if db.iterator(IteratorMode::Start).next().transpose()?.is_some() {
                        return Err(StoreError::Corrupt("rocksdb schema marker missing from nonempty database".into()));
                    }
                    let mut batch = WriteBatch::default();
                    batch.put(SCHEMA, b"1");
                    batch.put(FEATURES, features);
                    let mut write = WriteOptions::default();
                    write.set_sync(true);
                    db.write_opt(batch, &write)?;
                }
            }
            log::info!("rocksdb open path={} schema=1 coin_index={} hint={} wal=true sync=true elapsed_ms={}", path.display(), features[0], features[1], started.elapsed().as_millis());
            Ok((db, options))
        }).await.map_err(|e| StoreError::Batch(format!("rocksdb open worker: {e}")))??;
        Ok(Self {
            db: Arc::new(db),
            options: Arc::new(options),
            writer: Arc::new(tokio::sync::Mutex::new(())),
            workers: Arc::new(Semaphore::new(8)),
            near_tip: AtomicBool::new(false),
        })
    }

    async fn read<T: Send + 'static>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&View<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| StoreError::Batch(e.to_string()))?;
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let started = Instant::now();
            let mut pending = Pending::new();
            let view = View {
                snapshot: db.snapshot(),
                pending: &mut pending,
            };
            let result = f(&view);
            log::info!(
                "rocksdb read operation={operation} elapsed_us={} success={}",
                started.elapsed().as_micros(),
                result.is_ok()
            );
            result
        })
        .await
        .map_err(|e| StoreError::Batch(format!("rocksdb read worker: {e}")))?
    }

    fn batch(&self, batch: &BatchHandle) -> Result<Arc<RocksBatch>, StoreError> {
        let crate::types::BatchInner::Rocksdb(batch) = &batch.inner else {
            return Err(StoreError::Corrupt(
                "batch was opened by a different backend".into(),
            ));
        };
        if !Arc::ptr_eq(&self.db, &batch.db) {
            return Err(StoreError::Corrupt(
                "batch belongs to a different rocksdb instance".into(),
            ));
        }
        Ok(batch.clone())
    }

    async fn mutate<T: Send + 'static>(
        &self,
        batch: &BatchHandle,
        operation: &'static str,
        f: impl FnOnce(&mut View<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let batch = self.batch(batch)?;
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| StoreError::Batch(e.to_string()))?;
        batch.in_flight.fetch_add(1, Ordering::SeqCst);
        let lease = StageLease(batch.clone());
        tokio::task::spawn_blocking(move || {
            let _lease = lease;
            let _permit = permit;
            let started = Instant::now();
            let mut state = batch
                .state
                .lock()
                .map_err(|_| StoreError::Batch("rocksdb batch mutex poisoned".into()))?;
            if state.failed || state.finished {
                return Err(StoreError::Batch(
                    "rocksdb batch previously failed; drop it".into(),
                ));
            }
            let mut view = View {
                snapshot: batch.db.snapshot(),
                pending: &mut state.pending,
            };
            let result = f(&mut view);
            drop(view);
            if result.is_err() {
                state.failed = true;
            }
            log::info!(
                "rocksdb stage operation={operation} elapsed_us={} pending_keys={} success={}",
                started.elapsed().as_micros(),
                state.pending.len(),
                result.is_ok()
            );
            result
        })
        .await
        .map_err(|e| StoreError::Batch(format!("rocksdb stage worker: {e}")))?
    }

    fn log_properties(db: &DB, options: &Options) {
        use ::rocksdb::statistics::Ticker;
        log::info!(
            "rocksdb counters stall_us_total={} bytes_written_total={} bytes_read_total={} wal_bytes_written_total={} wal_syncs_total={} compaction_read_bytes_total={} compaction_write_bytes_total={} flush_write_bytes_total={} cache_hits_total={} cache_misses_total={}",
            options.get_ticker_count(Ticker::StallMicros),
            options.get_ticker_count(Ticker::BytesWritten),
            options.get_ticker_count(Ticker::BytesRead),
            options.get_ticker_count(Ticker::WalFileBytes),
            options.get_ticker_count(Ticker::WalFileSynced),
            options.get_ticker_count(Ticker::CompactReadBytes),
            options.get_ticker_count(Ticker::CompactWriteBytes),
            options.get_ticker_count(Ticker::FlushWriteBytes),
            options.get_ticker_count(Ticker::BlockCacheHit),
            options.get_ticker_count(Ticker::BlockCacheMiss)
        );
        let get = |name| db.property_int_value(name).ok().flatten().unwrap_or(0);
        log::info!(
            "rocksdb stats memtable_bytes={} block_cache_bytes={} pending_compaction_bytes={} live_sst_bytes={} running_compactions={} running_flushes={} write_stopped={} background_errors={}",
            get("rocksdb.cur-size-all-mem-tables"),
            get("rocksdb.block-cache-usage"),
            get("rocksdb.estimate-pending-compaction-bytes"),
            get("rocksdb.live-sst-files-size"),
            get("rocksdb.num-running-compactions"),
            get("rocksdb.num-running-flushes"),
            get("rocksdb.is-write-stopped"),
            get("rocksdb.background-errors")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockStore;

    #[tokio::test]
    async fn cancelled_stage_retains_writer_until_the_worker_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksDbStore::open(&dir.path().join("rocks")).await.unwrap();
        let batch = store.begin().await.unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut mutation = Box::pin(store.mutate(&batch, "cancel_probe", move |view| {
            entered_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            view.put(b"!cancel_probe".to_vec(), vec![1]);
            Ok(())
        }));
        tokio::select! {
            _ = entered_rx => {},
            result = &mut mutation => panic!("stage finished early: {result:?}"),
        }
        drop(mutation);
        drop(batch);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), store.begin())
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        let batch = tokio::time::timeout(std::time::Duration::from_secs(5), store.begin())
            .await
            .unwrap()
            .unwrap();
        drop(batch);
        assert!(store.db.get(b"!cancel_probe").unwrap().is_none());
    }
}
