use super::*;
use crate::{BlockStore, SqliteStore};
use sqlx::{ConnectOptions, Connection};

fn outcome(log: i64, checkpointed: i64) -> Option<CheckpointOutcome> {
    Some(CheckpointOutcome {
        busy: 0,
        log,
        checkpointed,
    })
}

#[test]
fn write_activity_retriggers_after_reuse_and_coalesces_notifications() {
    let now = Instant::now();
    let mut schedule = Schedule::new(now);
    assert_eq!(schedule.reason(now, 1000, 4120, 100, false), None);
    assert_eq!(
        schedule.reason(now + MIN_PASS_INTERVAL, 1000, 4120, 100, false),
        Some("write_budget")
    );
    schedule.writes_at_pass = 1000;
    schedule.last_pass = now + MIN_PASS_INTERVAL;
    schedule.observe(schedule.last_pass, outcome(1000, 1000));
    assert_eq!(
        schedule.reason(now + Duration::from_secs(1), 1000, 4120, 100, false),
        None
    );
    assert_eq!(
        schedule.reason(now + Duration::from_secs(1), 1001, 4120, 100, false),
        Some("write_budget")
    );
}

#[test]
fn periodic_probe_covers_startup_and_unsampled_writes() {
    let now = Instant::now();
    let schedule = Schedule::new(now);
    assert_eq!(
        schedule.reason(now + Duration::from_secs(1), 0, 4120, 100, false),
        None
    );
    assert_eq!(
        schedule.reason(now + BULK_INTERVAL, 0, 4120, 100, false),
        Some("interval")
    );
    assert_eq!(
        schedule.reason(now + Duration::from_secs(1), 0, 4120, 100, true),
        Some("interval")
    );
}

#[test]
fn stalled_backlog_requires_time_size_and_spaced_escalation() {
    let now = Instant::now();
    let mut schedule = Schedule::new(now);
    schedule.observe(now, outcome(100, 10));
    assert!(!schedule.may_escalate(now, 4120, 100));
    assert_eq!(
        schedule.reason(now + RETRY_INTERVAL, 0, 4120, 100, false),
        Some("backlog")
    );
    let later = now + STALL_INTERVAL;
    assert!(schedule.may_escalate(later, 4120, 100));
    assert!(!schedule.may_escalate(later, 4120, 1_000_000));
    schedule.last_escalation = Some(later);
    assert!(!schedule.may_escalate(later + STALL_INTERVAL, 4120, 100));
    assert!(schedule.may_escalate(later + ESCALATION_INTERVAL, 4120, 100));
}

#[test]
fn progress_and_reset_restart_stall_age_and_completion_clears_it() {
    let now = Instant::now();
    let mut schedule = Schedule::new(now);
    schedule.observe(now, outcome(100, 10));
    let later = now + STALL_INTERVAL;
    schedule.observe(later, outcome(110, 11));
    assert!(!schedule.may_escalate(later, 4120, 100));
    assert_eq!(schedule.progress_age(later), 0);
    schedule.observe(later + STALL_INTERVAL, outcome(10, 1));
    assert_eq!(schedule.progress_age(later + STALL_INTERVAL), 0);
    schedule.observe(later + STALL_INTERVAL, outcome(10, 10));
    assert!(!schedule.retry);
    assert!(schedule.stalled_since.is_none());
}

#[test]
fn error_retries_without_inventing_a_pinned_reader() {
    let now = Instant::now();
    let mut schedule = Schedule::new(now);
    schedule.observe(now, None);
    assert!(schedule.retry);
    assert!(!schedule.may_escalate(now + STALL_INTERVAL, 4120, 100));
    assert_eq!(
        schedule.reason(now + RETRY_INTERVAL, 0, 4120, 100, false),
        Some("backlog")
    );
}

async fn open_probe(budget: u64) -> (tempfile::TempDir, SqliteStore) {
    let directory = tempfile::tempdir().unwrap();
    let store =
        SqliteStore::open_with_wal_drain_trigger(&directory.path().join("chain.db"), budget)
            .await
            .unwrap();
    sqlx::query("CREATE TABLE checkpoint_probe (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(&mut *store.writer.lock().await)
        .await
        .unwrap();
    (directory, store)
}

async fn write_probe(store: &SqliteStore, bytes: i64) {
    let mut batch = store.begin().await.unwrap();
    sqlx::query("INSERT OR REPLACE INTO checkpoint_probe VALUES (1, zeroblob(?))")
        .bind(bytes)
        .execute(batch.sqlite_conn().unwrap())
        .await
        .unwrap();
    store.commit(batch).await.unwrap();
}

async fn wait_for_pass(store: &SqliteStore, previous: u64) {
    tokio::time::timeout(Duration::from_secs(6), async {
        while store
            .telemetry
            .checkpoint_passive
            .calls
            .load(Ordering::Relaxed)
            <= previous
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reused_wal_is_drained_again_without_truncation() {
    let (_directory, store) = open_probe(16 * 1024).await;
    write_probe(&store, 256 * 1024).await;
    wait_for_pass(&store, 0).await;
    let allocation = store.wal_file_bytes();
    let requests = store
        .telemetry
        .checkpoint_write_budget
        .load(Ordering::Relaxed);
    let passes = store
        .telemetry
        .checkpoint_passive
        .calls
        .load(Ordering::Relaxed);
    write_probe(&store, 32 * 1024).await;
    wait_for_pass(&store, passes).await;
    assert!(
        store
            .telemetry
            .checkpoint_write_budget
            .load(Ordering::Relaxed)
            > requests
    );
    assert_eq!(store.wal_file_bytes(), allocation);
    assert_eq!(
        store
            .telemetry
            .checkpoint_truncate
            .calls
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        store
            .telemetry
            .wal_outstanding_frames
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn pinned_reader_does_not_trigger_immediate_truncate_or_stall_writer() {
    let (directory, store) = open_probe(16 * 1024).await;
    write_probe(&store, 128 * 1024).await;
    wait_for_pass(&store, 0).await;
    let mut reader = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(directory.path().join("chain.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap();
    let mut transaction = reader.begin().await.unwrap();
    sqlx::query("SELECT * FROM checkpoint_probe")
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
    let passes = store
        .telemetry
        .checkpoint_passive
        .calls
        .load(Ordering::Relaxed);
    write_probe(&store, 256 * 1024).await;
    wait_for_pass(&store, passes).await;
    assert!(
        store
            .telemetry
            .wal_outstanding_frames
            .load(Ordering::Relaxed)
            > 0
    );
    assert!(
        store
            .telemetry
            .checkpoint_incomplete
            .load(Ordering::Relaxed)
            > 0
    );
    assert_eq!(
        store
            .telemetry
            .checkpoint_truncate
            .calls
            .load(Ordering::Relaxed),
        0
    );
    tokio::time::timeout(Duration::from_secs(1), write_probe(&store, 64 * 1024))
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        while store
            .telemetry
            .wal_outstanding_frames
            .load(Ordering::Relaxed)
            > 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn continuous_commits_do_not_starve_background_progress() {
    let (_directory, store) = open_probe(16 * 1024).await;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        write_probe(&store, 64 * 1024).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        store
            .telemetry
            .checkpoint_passive
            .calls
            .load(Ordering::Relaxed)
            >= 2
    );
    assert!(
        store
            .telemetry
            .wal_frames_checkpointed_total
            .load(Ordering::Relaxed)
            > 0
    );
    assert_eq!(
        store
            .telemetry
            .checkpoint_truncate
            .calls
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn reopen_drains_preexisting_wal_without_commit_notification() {
    let (directory, store) = open_probe(u64::MAX).await;
    store.checkpointer.abort();
    write_probe(&store, 128 * 1024).await;
    let mut reader = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(directory.path().join("chain.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap();
    sqlx::query("SELECT * FROM checkpoint_probe")
        .fetch_all(&mut reader)
        .await
        .unwrap();
    drop(store);
    let reopened = SqliteStore::open(&directory.path().join("chain.db"))
        .await
        .unwrap();
    wait_for_pass(&reopened, 0).await;
    let bytes: i64 = sqlx::query_scalar("SELECT length(data) FROM checkpoint_probe WHERE id = 1")
        .fetch_one(&reopened.read)
        .await
        .unwrap();
    assert_eq!(bytes, 128 * 1024);
    assert_eq!(
        reopened
            .telemetry
            .wal_outstanding_frames
            .load(Ordering::Relaxed),
        0
    );
    assert!(
        reopened
            .telemetry
            .checkpoint_interval
            .load(Ordering::Relaxed)
            > 0
    );
}

#[tokio::test]
async fn escalation_waits_for_writer_and_does_not_wait_for_pinned_reader() {
    let (directory, store) = open_probe(16 * 1024).await;
    write_probe(&store, 128 * 1024).await;
    wait_for_pass(&store, 0).await;
    let mut reader = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(directory.path().join("chain.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap();
    let mut transaction = reader.begin().await.unwrap();
    sqlx::query("SELECT * FROM checkpoint_probe")
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
    write_probe(&store, 256 * 1024).await;
    let writer_guard = store.writer.lock().await;
    tokio::time::timeout(Duration::from_secs(40), async {
        while store
            .telemetry
            .checkpoint_escalation_deferred
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        store
            .telemetry
            .checkpoint_truncate
            .calls
            .load(Ordering::Relaxed),
        0
    );
    drop(writer_guard);
    tokio::time::timeout(Duration::from_secs(3), async {
        while store
            .telemetry
            .checkpoint_truncate
            .calls
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        store
            .telemetry
            .checkpoint_busy_total
            .load(Ordering::Relaxed)
            > 0
    );
    tokio::time::timeout(Duration::from_secs(1), write_probe(&store, 64 * 1024))
        .await
        .unwrap();
    let old_bytes: i64 =
        sqlx::query_scalar("SELECT length(data) FROM checkpoint_probe WHERE id = 1")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(old_bytes, 128 * 1024);
    transaction.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(6), async {
        while store
            .telemetry
            .wal_outstanding_frames
            .load(Ordering::Relaxed)
            > 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        store
            .telemetry
            .checkpoint_truncate
            .calls
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        store
            .telemetry
            .checkpoint_no_progress_seconds
            .load(Ordering::Relaxed),
        0
    );
}
