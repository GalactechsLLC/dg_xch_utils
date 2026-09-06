use crate::telemetry::StoreTelemetry;
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

const MIN_PASS_INTERVAL: Duration = Duration::from_millis(250);
const BULK_INTERVAL: Duration = Duration::from_secs(2);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const STALL_INTERVAL: Duration = Duration::from_secs(30);
const ESCALATION_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
struct CheckpointOutcome {
    busy: i64,
    log: i64,
    checkpointed: i64,
}

impl CheckpointOutcome {
    fn fully_checkpointed(self) -> bool {
        self.busy == 0 && self.log >= 0 && self.checkpointed >= self.log
    }

    fn outstanding(self) -> u64 {
        if self.log < 0 || self.checkpointed < 0 {
            return 0;
        }
        self.log.saturating_sub(self.checkpointed).max(0) as u64
    }
}

struct Schedule {
    last_pass: Instant,
    writes_at_pass: u64,
    previous: Option<CheckpointOutcome>,
    stalled_since: Option<Instant>,
    last_escalation: Option<Instant>,
    retry: bool,
}

impl Schedule {
    fn new(now: Instant) -> Self {
        Self {
            last_pass: now,
            writes_at_pass: 0,
            previous: None,
            stalled_since: None,
            last_escalation: None,
            retry: false,
        }
    }

    fn reason(
        &self,
        now: Instant,
        writes: u64,
        frame_bytes: u64,
        budget: u64,
        near_tip: bool,
    ) -> Option<&'static str> {
        let elapsed = now.duration_since(self.last_pass);
        if elapsed < MIN_PASS_INTERVAL {
            return None;
        }
        if writes
            .saturating_sub(self.writes_at_pass)
            .saturating_mul(frame_bytes)
            >= budget
        {
            return Some("write_budget");
        }
        if self.retry && elapsed >= RETRY_INTERVAL {
            return Some("backlog");
        }
        let interval = if near_tip {
            RETRY_INTERVAL
        } else {
            BULK_INTERVAL
        };
        (elapsed >= interval).then_some("interval")
    }

    fn observe(&mut self, now: Instant, outcome: Option<CheckpointOutcome>) {
        self.retry = outcome.is_none_or(|result| !result.fully_checkpointed());
        let Some(result) = outcome else {
            return;
        };
        if result.fully_checkpointed() {
            self.stalled_since = None;
        } else if result.outstanding() > 0 {
            let progressed = self.previous.is_none_or(|previous| {
                result.checkpointed > previous.checkpointed || result.log < previous.log
            });
            if progressed || self.stalled_since.is_none() {
                self.stalled_since = Some(now);
            }
        }
        self.previous = Some(result);
    }

    fn may_escalate(&self, now: Instant, frame_bytes: u64, budget: u64) -> bool {
        self.previous
            .is_some_and(|result| result.outstanding().saturating_mul(frame_bytes) >= budget)
            && self
                .stalled_since
                .is_some_and(|since| now.duration_since(since) >= STALL_INTERVAL)
            && self
                .last_escalation
                .is_none_or(|last| now.duration_since(last) >= ESCALATION_INTERVAL)
    }

    fn progress_age(&self, now: Instant) -> u64 {
        self.stalled_since
            .map_or(0, |since| now.duration_since(since).as_secs())
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_checkpointer(
    mut conn: SqliteConnection,
    near_tip: Arc<AtomicBool>,
    telemetry: Arc<StoreTelemetry>,
    read: SqlitePool,
    writer: Arc<Mutex<SqliteConnection>>,
    notify: Arc<Notify>,
    budget: u64,
    page_size: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(MIN_PASS_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut schedule = Schedule::new(Instant::now());
        let mut prev_checkpointed = 0;
        let frame_bytes = page_size.saturating_add(24);
        loop {
            tokio::select! {
                _ = tick.tick() => {},
                _ = notify.notified() => {},
            }
            let now = Instant::now();
            telemetry
                .read_pool_idle
                .store(read.num_idle() as u64, Ordering::Relaxed);
            telemetry
                .read_pool_size
                .store(u64::from(read.size()), Ordering::Relaxed);
            telemetry
                .checkpoint_no_progress_seconds
                .store(schedule.progress_age(now), Ordering::Relaxed);
            let writes = telemetry.cache_writes.load(Ordering::Relaxed);
            let Some(reason) = schedule.reason(
                now,
                writes,
                frame_bytes,
                budget,
                near_tip.load(Ordering::Relaxed),
            ) else {
                continue;
            };
            match reason {
                "write_budget" => &telemetry.checkpoint_write_budget,
                "backlog" => &telemetry.checkpoint_backlog,
                _ => &telemetry.checkpoint_interval,
            }
            .fetch_add(1, Ordering::Relaxed);
            let passive =
                checkpoint_pass(&mut conn, &telemetry, &mut prev_checkpointed, "PASSIVE").await;
            let now = Instant::now();
            schedule.last_pass = now;
            schedule.writes_at_pass = writes;
            schedule.observe(now, passive);
            if passive.is_some() && schedule.may_escalate(now, frame_bytes, budget) {
                if let Ok(_writer_guard) = writer.try_lock() {
                    schedule.last_escalation = Some(now);
                    let outcome =
                        checkpoint_pass(&mut conn, &telemetry, &mut prev_checkpointed, "TRUNCATE")
                            .await;
                    schedule.observe(Instant::now(), outcome);
                } else {
                    telemetry
                        .checkpoint_escalation_deferred
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            telemetry
                .checkpoint_no_progress_seconds
                .store(schedule.progress_age(Instant::now()), Ordering::Relaxed);
        }
    })
}

async fn checkpoint_pass(
    conn: &mut SqliteConnection,
    telemetry: &StoreTelemetry,
    prev_checkpointed: &mut i64,
    mode: &str,
) -> Option<CheckpointOutcome> {
    let started = Instant::now();
    let _timer = if mode == "PASSIVE" {
        telemetry.checkpoint_passive.start()
    } else {
        telemetry.checkpoint_truncate.start()
    };
    let result = sqlx::query(&format!("PRAGMA wal_checkpoint({mode})"))
        .fetch_one(&mut *conn)
        .await;
    match result {
        Ok(row) => {
            telemetry.checkpoint.record(started.elapsed().as_secs_f64());
            let outcome = CheckpointOutcome {
                busy: row.try_get(0).unwrap_or(0),
                log: row.try_get(1).unwrap_or(-1),
                checkpointed: row.try_get(2).unwrap_or(-1),
            };
            if outcome.busy != 0 {
                telemetry
                    .checkpoint_busy_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            if !outcome.fully_checkpointed() {
                telemetry
                    .checkpoint_incomplete
                    .fetch_add(1, Ordering::Relaxed);
            }
            if outcome.log >= 0 && outcome.checkpointed >= 0 {
                telemetry
                    .wal_frames
                    .store(outcome.log as u64, Ordering::Relaxed);
                telemetry
                    .wal_outstanding_frames
                    .store(outcome.outstanding(), Ordering::Relaxed);
                let advance = if outcome.checkpointed >= *prev_checkpointed {
                    outcome.checkpointed - *prev_checkpointed
                } else {
                    outcome.checkpointed
                };
                *prev_checkpointed = outcome.checkpointed;
                telemetry
                    .wal_frames_checkpointed_total
                    .fetch_add(advance as u64, Ordering::Relaxed);
            }
            Some(outcome)
        }
        Err(_) => {
            telemetry
                .checkpoint_errors_total
                .fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sqlite/mod/checkpoint_tests.rs"]
mod tests;
