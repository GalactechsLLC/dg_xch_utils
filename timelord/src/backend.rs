use crate::worker::{ProofRequest, ProofResult, run_regular_isolated};
use std::future::Future;
use std::io::Error;
use std::pin::Pin;
use std::time::Duration;

pub type ProofFuture<'backend> =
    Pin<Box<dyn Future<Output = Result<ProofResult, Error>> + Send + 'backend>>;

pub trait VdfBackend: Send + Sync {
    fn prove(&self, request: ProofRequest) -> ProofFuture<'_>;
}

pub struct CpuBackend {
    pub timeout: Duration,
    pub memory_bytes: u64,
}

impl VdfBackend for CpuBackend {
    fn prove(&self, request: ProofRequest) -> ProofFuture<'_> {
        Box::pin(run_regular_isolated(
            request,
            self.timeout,
            self.memory_bytes,
        ))
    }
}
