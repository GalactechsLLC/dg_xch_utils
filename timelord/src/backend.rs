use crate::worker::{PersistentRegularWorker, ProofRequest, ProofResult};
use std::collections::VecDeque;
use std::future::Future;
use std::io::Error;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::Mutex;

pub type ProofFuture<'backend> =
    Pin<Box<dyn Future<Output = Result<ProofResult, Error>> + Send + 'backend>>;

pub trait VdfBackend: Send + Sync {
    fn prove(&self, request: ProofRequest) -> ProofFuture<'_>;
    fn prove_with_threads(&self, request: ProofRequest, _threads: usize) -> ProofFuture<'_> {
        self.prove(request)
    }
}

pub struct CpuBackend {
    pub timeout: Duration,
    pub memory_bytes: u64,
    executable: Option<PathBuf>,
    idle: Mutex<VecDeque<(ProofRequest, PersistentRegularWorker)>>,
}

impl CpuBackend {
    pub fn new(timeout: Duration, memory_bytes: u64) -> Self {
        Self {
            timeout,
            memory_bytes,
            executable: None,
            idle: Mutex::new(VecDeque::new()),
        }
    }
    /// Use a dedicated dgx worker executable when embedding the backend in another process.
    pub fn with_executable(timeout: Duration, memory_bytes: u64, executable: PathBuf) -> Self {
        Self {
            executable: Some(executable),
            ..Self::new(timeout, memory_bytes)
        }
    }
}

impl VdfBackend for CpuBackend {
    fn prove(&self, request: ProofRequest) -> ProofFuture<'_> {
        self.prove_with_threads(request, 1)
    }
    fn prove_with_threads(&self, request: ProofRequest, threads: usize) -> ProofFuture<'_> {
        Box::pin(async move {
            let mut key = request.clone();
            key.iterations = 0;
            let cached = {
                let mut idle = self.idle.lock().await;
                idle.retain(|(old, _)| old.generation == key.generation);
                let index = idle.iter().position(|(old, _)| *old == key);
                index
                    .and_then(|index| idle.remove(index))
                    .map(|(_, worker)| worker)
            };
            // Ownership stays inside this future while work runs. Cancelling it drops and kills
            // the child, so a later request cannot consume a stale response or continue stale work.
            let mut worker = match cached {
                Some(worker) => worker,
                None => match &self.executable {
                    Some(path) => PersistentRegularWorker::spawn_with_executable(path)?,
                    None => PersistentRegularWorker::spawn()?,
                },
            };
            let started = std::time::Instant::now();
            let output = worker
                .prove(request, self.memory_bytes, self.timeout, threads)
                .await?;
            eprintln!(
                "timelord event=worker_proof_completed challenge={} iterations={} new_squarings={} reused_squarings={} checkpoint_budget_bytes={} witness_threads={} elapsed_ms={} new_squarings_per_second={:.0}",
                output.result.info.challenge,
                output.result.info.number_of_iterations,
                output.new_squarings,
                output.reused_squarings,
                output.checkpoint_budget_bytes,
                output.witness_threads,
                started.elapsed().as_millis(),
                output.new_squarings as f64 / started.elapsed().as_secs_f64().max(f64::EPSILON)
            );
            let mut idle = self.idle.lock().await;
            if idle.len() == 3 {
                idle.pop_front();
            }
            idle.push_back((key, worker));
            Ok(output.result)
        })
    }
}
