use super::*;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use rayon::prelude::*;

struct Maintenance(Arc<crate::telemetry::StoreTelemetry>, Instant);

impl Maintenance {
    fn start(telemetry: Arc<crate::telemetry::StoreTelemetry>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        telemetry.schema_active.fetch_add(1, Ordering::Relaxed);
        telemetry.schema_started_unix.store(now, Ordering::Relaxed);
        Self(telemetry, Instant::now())
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.0.schema_seconds.record(self.1.elapsed().as_secs_f64());
        self.0.schema_active.fetch_sub(1, Ordering::Relaxed);
    }
}

fn indexes_ready(db: &DB) -> Result<bool, StoreError> {
    match db.get(SERVICE_INDEXES)?.as_deref() {
        None | Some(b"1") => Ok(true),
        Some(b"0") => Ok(false),
        _ => Err(StoreError::Corrupt(
            "invalid rocksdb service index state".into(),
        )),
    }
}

impl RocksDbStore {
    async fn index_writer(
        &self,
        operation: impl FnOnce(
            &DB,
            &rayon::ThreadPool,
            &crate::telemetry::StoreTelemetry,
        ) -> Result<(), StoreError>
        + Send
        + 'static,
    ) -> Result<(), StoreError> {
        let maintenance = Maintenance::start(self.telemetry.clone());
        let writer = self.writer.clone().lock_owned().await;
        let db = self.db.clone();
        let workers = self.coin_workers.clone();
        tokio::task::spawn_blocking(move || {
            // Cancellation of the async caller cannot release the writer during backfill.
            let _writer = writer;
            let result = operation(&db, &workers, &maintenance.0);
            if result.is_err() {
                maintenance.0.schema_errors.fetch_add(1, Ordering::Relaxed);
            }
            result
        })
        .await
        .map_err(|e| StoreError::Batch(format!("rocksdb index worker: {e}")))?
    }

    pub(super) async fn defer_service_indexes(&self) -> Result<(), StoreError> {
        self.index_writer(|db, _, _| {
            if !indexes_ready(db)? {
                return Ok(());
            }
            let mut batch = WriteBatch::default();
            // Puzzle and parent prefixes are adjacent; preserve primary/height/hint keys.
            batch.delete_range([PUZZLE], [PARENT + 1]);
            batch.put(SERVICE_INDEXES, b"0");
            let mut options = WriteOptions::default();
            options.set_sync(true);
            db.write_opt(batch, &options)?;
            log::info!("rocksdb wallet indexes deferred for catch-up");
            Ok(())
        })
        .await
    }

    pub(super) async fn build_service_indexes(&self) -> Result<(), StoreError> {
        self.index_writer(|db, workers, telemetry| {
            if indexes_ready(db)? {
                return Ok(());
            }
            let started = Instant::now();
            let mut clear = WriteBatch::default();
            clear.delete_range([PUZZLE], [PARENT + 1]);
            db.write(clear)?;
            let snapshot = db.snapshot();
            let mut records = Vec::with_capacity(8_192);
            let mut count = 0usize;
            let mut flush = |records: &mut Vec<(Bytes32, CoinRecord)>| -> Result<(), StoreError> {
                let keys: Vec<_> = workers.install(|| {
                    records
                        .par_iter()
                        .map(|(id, r)| {
                            (
                                index(PUZZLE, r.coin.puzzle_hash, id),
                                index(PARENT, r.coin.parent_coin_info, id),
                            )
                        })
                        .collect()
                });
                let mut batch = WriteBatch::default();
                for (puzzle, parent) in keys {
                    batch.put(puzzle, []);
                    batch.put(parent, []);
                }
                db.write(batch)?;
                count += records.len();
                records.clear();
                telemetry.schema_statements.fetch_add(1, Ordering::Relaxed);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                telemetry.schema_progress_unix.store(now, Ordering::Relaxed);
                Ok(())
            };
            for entry in snapshot.iterator(IteratorMode::From(&[COIN], Direction::Forward)) {
                let (k, bytes) = entry?;
                if !k.starts_with(&[COIN]) {
                    break;
                }
                records.push((hash(&k[1..])?, decode::<CoinRecord>(&bytes)?));
                if records.len() == 8_192 {
                    flush(&mut records)?;
                }
            }
            if !records.is_empty() {
                flush(&mut records)?;
            }
            // Publish readiness only after all prior WAL writes are made durable. A failed or
            // interrupted build stays deferred and can be safely rebuilt from primary coins.
            let mut ready = WriteBatch::default();
            ready.put(SERVICE_INDEXES, b"1");
            let mut options = WriteOptions::default();
            options.set_sync(true);
            db.write_opt(ready, &options)?;
            log::info!(
                "rocksdb wallet indexes built coins={count} elapsed_ms={}",
                started.elapsed().as_millis()
            );
            Ok(())
        })
        .await
    }
}
