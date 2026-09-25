use super::SqliteStore;
use crate::{error::StoreError, telemetry::StoreTelemetry};
use sqlx::{ConnectOptions, Connection};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct ActiveSchema(Arc<StoreTelemetry>);

impl Drop for ActiveSchema {
    fn drop(&mut self) {
        self.0.schema_active.store(0, Ordering::Relaxed);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl SqliteStore {
    pub(crate) async fn run_schema(&self, sql: String) -> Result<(), StoreError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(cancelled.clone());
        let options = self.maintenance_options.clone();
        let schema_gate = self.schema_gate.clone();
        let writer = self.writer.clone();
        let telemetry = self.telemetry.clone();
        let checkpoint = self.checkpoint_notify.clone();
        tokio::spawn(async move {
            let _schema_guard = schema_gate.lock_owned().await;
            if cancelled.load(Ordering::Relaxed) {
                return Err(StoreError::Batch("schema maintenance cancelled".into()));
            }
            let mut connection = options.connect().await?;
            let memory_only: i64 = sqlx::query_scalar("SELECT sqlite_compileoption_used('TEMP_STORE=3')")
                .fetch_one(&mut connection).await?;
            if memory_only != 0 {
                return Err(StoreError::Batch("SQLite TEMP_STORE=3 prevents disk-backed index sorting".into()));
            }
            let progress = telemetry.clone();
            let stopped = cancelled.clone();
            connection.lock_handle().await?.set_progress_handler(10_000, move || {
                progress.schema_vm_steps.fetch_add(10_000, Ordering::Relaxed);
                progress.schema_progress_unix.store(now(), Ordering::Relaxed);
                !stopped.load(Ordering::Relaxed)
            });
            telemetry.schema_active.store(1, Ordering::Relaxed);
            telemetry.schema_started_unix.store(now(), Ordering::Relaxed);
            telemetry.schema_progress_unix.store(now(), Ordering::Relaxed);
            let _active = ActiveSchema(telemetry.clone());
            let mut outcome = Ok(());
            for statement in sql.split(';').map(str::trim).filter(|statement| !statement.is_empty()) {
                let _writer_guard = writer.clone().lock_owned().await;
                if cancelled.load(Ordering::Relaxed) {
                    outcome = Err(StoreError::Batch("schema maintenance cancelled".into()));
                    break;
                }
                let started = Instant::now();
                log::info!("SQLite index statement started temp_store=FILE cache_mib=16 sort_threads=2 sql={statement}");
                let result = sqlx::query(statement).execute(&mut connection).await;
                telemetry.schema_seconds.record(started.elapsed().as_secs_f64());
                telemetry.schema_statements.fetch_add(1, Ordering::Relaxed);
                if let Err(error) = result {
                    telemetry.schema_errors.fetch_add(1, Ordering::Relaxed);
                    log::warn!("SQLite index statement failed elapsed_ms={} error={error}", started.elapsed().as_millis());
                    outcome = Err(StoreError::Backend(error));
                    break;
                }
                log::info!("SQLite index statement completed elapsed_ms={} sql={statement}", started.elapsed().as_millis());
                drop(_writer_guard);
                checkpoint.notify_one();
                tokio::task::yield_now().await;
            }
            connection.lock_handle().await?.remove_progress_handler();
            let closed = connection.close().await;
            checkpoint.notify_one();
            outcome.and(closed.map_err(StoreError::Backend))
        }).await.map_err(|error| StoreError::Io(std::io::Error::other(error)))?
    }

    #[must_use]
    pub fn memory_usage() -> (u64, u64) {
        let mut current = 0i64;
        let mut peak = 0i64;
        let status = unsafe {
            libsqlite3_sys::sqlite3_status64(
                libsqlite3_sys::SQLITE_STATUS_MEMORY_USED,
                &mut current,
                &mut peak,
                0,
            )
        };
        if status == libsqlite3_sys::SQLITE_OK {
            (current.max(0) as u64, peak.max(0) as u64)
        } else {
            (0, 0)
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sqlite/mod/maintenance_tests.rs"]
mod tests;
