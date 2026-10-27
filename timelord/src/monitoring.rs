use dg_xch_servers::observability::{Snapshot, SnapshotFn, gauges};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
#[derive(Default)]
pub struct State {
    pub connected: AtomicBool,
    pub attempts: AtomicU64,
    pub connections: AtomicU64,
    pub disconnects: AtomicU64,
    pub jobs: AtomicU64,
    pub completed: AtomicU64,
    pub failed: AtomicU64,
    pub outputs: AtomicU64,
    pub active_jobs: AtomicU64,
}
impl State {
    pub fn snapshot(self: &Arc<Self>, mode: &'static str) -> SnapshotFn {
        let state = self.clone();
        let start = Instant::now();
        Arc::new(move || {
            let state = state.clone();
            Box::pin(async move {
                let value = |counter: &AtomicU64| counter.load(Ordering::Relaxed) as f64;
                let connected = state.connected.load(Ordering::Acquire);
                Ok(Snapshot {
                    healthy: connected,
                    message: if connected {
                        "full node connected"
                    } else {
                        "waiting for full node"
                    }
                    .into(),
                    metrics: gauges(
                        &format!("chia_timelord_{mode}"),
                        &[
                            (
                                "connected",
                                "Authenticated full node connection active",
                                f64::from(connected),
                            ),
                            (
                                "uptime_seconds",
                                "Timelord runtime uptime",
                                start.elapsed().as_secs_f64(),
                            ),
                            (
                                "connection_attempts",
                                "Connection attempts since startup",
                                value(&state.attempts),
                            ),
                            (
                                "connections",
                                "Authenticated connections since startup",
                                value(&state.connections),
                            ),
                            (
                                "disconnects",
                                "Ended sessions since startup",
                                value(&state.disconnects),
                            ),
                            (
                                "jobs_started",
                                "VDF jobs started since startup",
                                value(&state.jobs),
                            ),
                            (
                                "jobs_completed",
                                "VDF jobs completed since startup",
                                value(&state.completed),
                            ),
                            (
                                "jobs_failed",
                                "VDF job failures since startup",
                                value(&state.failed),
                            ),
                            (
                                "active_jobs",
                                "VDF jobs currently computing",
                                value(&state.active_jobs),
                            ),
                            (
                                "outputs_sent",
                                "Protocol proof outputs successfully sent",
                                value(&state.outputs),
                            ),
                        ],
                    )?,
                })
            })
        })
    }
    pub fn connect(self: &Arc<Self>) -> Connection {
        self.connected.store(true, Ordering::Release);
        self.connections.fetch_add(1, Ordering::Relaxed);
        Connection(self.clone())
    }
    pub fn job(self: &Arc<Self>) -> Job {
        self.jobs.fetch_add(1, Ordering::Relaxed);
        self.active_jobs.fetch_add(1, Ordering::Relaxed);
        Job(self.clone())
    }
}
pub struct Connection(Arc<State>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.connected.store(false, Ordering::Release);
        self.0.disconnects.fetch_add(1, Ordering::Relaxed);
    }
}
pub struct Job(Arc<State>);
impl Drop for Job {
    fn drop(&mut self) {
        self.0.active_jobs.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn health_and_active_job_gauges_follow_connections_and_cancellation() {
        let state = Arc::new(State::default());
        let snapshot = state.snapshot("regular");
        assert!(!(snapshot)().await.unwrap().healthy);
        let connection = state.connect();
        let job = state.job();
        let observed = (snapshot)().await.unwrap();
        assert!(observed.healthy);
        assert!(
            observed
                .metrics
                .contains("chia_timelord_regular_active_jobs 1\n")
        );
        drop(job);
        drop(connection);
        let observed = (snapshot)().await.unwrap();
        assert!(!observed.healthy);
        assert!(
            observed
                .metrics
                .contains("chia_timelord_regular_active_jobs 0\n")
        );
        assert!(
            observed
                .metrics
                .contains("chia_timelord_regular_disconnects 1\n")
        );
    }
}
