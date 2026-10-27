use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes100};
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::traits::SizedBytes;
use dg_xch_vdf::proof::{StreamingProver, prove_result, prove_result_bounded, verify_vdf_serial};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAX_ITERATIONS: u64 = 1 << 26;
pub const WORKER_MESSAGE_LIMIT: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofRequest {
    pub generation: u64,
    pub challenge: Bytes32,
    pub input: ClassgroupElement,
    pub iterations: u64,
    pub discriminant_bits: usize,
}

impl ProofRequest {
    pub fn validate(&self) -> Result<(), Error> {
        self.validate_regular()?;
        if self.iterations > MAX_ITERATIONS {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "compact VDF request exceeds iteration limit",
            ));
        }
        Ok(())
    }

    pub fn validate_regular(&self) -> Result<(), Error> {
        if self.iterations == 0
            || !(16..=1024).contains(&self.discriminant_bits)
            || !self.discriminant_bits.is_multiple_of(8)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "VDF request exceeds worker resource limits",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofResult {
    pub generation: u64,
    pub info: VdfInfo,
    pub proof: VdfProof,
}

pub fn prove(request: &ProofRequest) -> Result<ProofResult, Error> {
    request.validate()?;
    prove_inner(request, None)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegularProofRequest {
    pub request: ProofRequest,
    pub memory_bytes: u64,
}

impl RegularProofRequest {
    pub fn validate(&self) -> Result<(), Error> {
        self.request.validate_regular()?;
        if !(128 * 1024..=8 * 1024 * 1024 * 1024).contains(&self.memory_bytes) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "regular VDF memory budget must be 128 KiB through 8 GiB",
            ));
        }
        Ok(())
    }
}

pub fn prove_regular(request: &RegularProofRequest) -> Result<ProofResult, Error> {
    request.validate()?;
    let mut result = prove_inner(&request.request, Some(request.memory_bytes))?;
    result.proof.normalized_to_identity = false;
    Ok(result)
}

fn prove_inner(request: &ProofRequest, memory_bytes: Option<u64>) -> Result<ProofResult, Error> {
    let bytes = match memory_bytes {
        Some(memory_bytes) => prove_result_bounded(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            request.discriminant_bits,
            request.iterations,
            memory_bytes,
        ),
        None => prove_result(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            request.discriminant_bits,
            request.iterations,
        ),
    }
    .map_err(Error::other)?;
    result_from_bytes(request, &bytes)
}

fn result_from_bytes(request: &ProofRequest, bytes: &[u8]) -> Result<ProofResult, Error> {
    if bytes.len() != 200
        || !verify_vdf_serial(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            bytes,
            request.discriminant_bits,
            request.iterations,
            0,
        )
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "native prover produced an invalid VDF",
        ));
    }
    let (output, witness) = bytes.split_at(100);
    Ok(ProofResult {
        generation: request.generation,
        info: VdfInfo {
            challenge: request.challenge,
            number_of_iterations: request.iterations,
            output: ClassgroupElement {
                data: Bytes100::parse(output)?,
            },
        },
        proof: VdfProof {
            witness_type: 0,
            witness: UnsizedBytes::new(witness.to_vec()),
            normalized_to_identity: request.input == ClassgroupElement::get_default_element(),
        },
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingProofResult {
    pub witness_threads: usize,
    pub result: ProofResult,
    pub new_squarings: u64,
    pub reused_squarings: u64,
    pub checkpoint_budget_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingRegularProofRequest {
    pub work: RegularProofRequest,
    pub witness_threads: usize,
}

#[derive(Default)]
pub struct StreamingRegularWorker {
    cached: Option<(RegularProofRequest, usize, StreamingProver)>,
}

impl StreamingRegularWorker {
    pub fn prove(&mut self, request: &RegularProofRequest) -> Result<StreamingProofResult, Error> {
        self.prove_with_threads(request, 1)
    }

    pub fn prove_with_threads(
        &mut self,
        request: &RegularProofRequest,
        witness_threads: usize,
    ) -> Result<StreamingProofResult, Error> {
        request.validate()?;
        if !(1..=4).contains(&witness_threads) {
            return Err(Error::other("witness threads must be 1..4"));
        }
        if self.cached.as_ref().is_none_or(|(old, old_threads, _)| {
            *old_threads != witness_threads
                || old.memory_bytes != request.memory_bytes
                || old.request.generation != request.request.generation
                || old.request.challenge != request.request.challenge
                || old.request.input != request.request.input
                || old.request.discriminant_bits != request.request.discriminant_bits
        }) {
            // Drop the previous history before allocating the new one.
            self.cached = None;
            let prover = StreamingProver::new_with_threads(
                request.request.challenge.as_ref(),
                request.request.input.data.as_ref(),
                request.request.discriminant_bits,
                request.memory_bytes,
                witness_threads,
            )
            .map_err(Error::other)?;
            self.cached = Some((request.clone(), witness_threads, prover));
        }
        let (_, _, prover) = self
            .cached
            .as_mut()
            .ok_or_else(|| Error::other("streaming prover unavailable"))?;
        let proof = prover
            .prove(request.request.iterations)
            .map_err(Error::other)?;
        let mut result = result_from_bytes(&request.request, &proof.bytes)?;
        result.proof.normalized_to_identity = false;
        Ok(StreamingProofResult {
            witness_threads,
            result,
            new_squarings: proof.new_squarings,
            reused_squarings: proof.reused_squarings,
            checkpoint_budget_bytes: proof.checkpoint_budget_bytes,
        })
    }
}

/// A reusable isolated worker. Dropping it on cancellation kills unfinished native work.
pub struct PersistentRegularWorker {
    _child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl PersistentRegularWorker {
    pub fn spawn() -> Result<Self, Error> {
        Self::spawn_with_executable(std::env::current_exe()?)
    }

    pub fn spawn_with_executable(executable: impl AsRef<std::ffi::OsStr>) -> Result<Self, Error> {
        let mut child = tokio::process::Command::new(executable)
            .args(["timelord", "regular-stream-worker"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::other("worker stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::other("worker stdout unavailable"))?;
        Ok(Self {
            _child: child,
            stdin,
            stdout,
        })
    }

    pub async fn prove(
        &mut self,
        request: ProofRequest,
        memory_bytes: u64,
        timeout: Duration,
        witness_threads: usize,
    ) -> Result<StreamingProofResult, Error> {
        let work = RegularProofRequest {
            request: request.clone(),
            memory_bytes,
        };
        work.validate()?;
        if timeout.is_zero() || timeout > Duration::from_secs(86_400) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "worker timeout must be 1..86400 seconds",
            ));
        }
        tokio::time::timeout(timeout, async {
            let bytes = serde_json::to_vec(&StreamingRegularProofRequest {
                work,
                witness_threads,
            })
            .map_err(Error::other)?;
            if bytes.len() as u64 > WORKER_MESSAGE_LIMIT {
                return Err(Error::other("worker input exceeds limit"));
            }
            self.stdin
                .write_all(&(bytes.len() as u32).to_be_bytes())
                .await?;
            self.stdin.write_all(&bytes).await?;
            let mut header = [0; 4];
            self.stdout.read_exact(&mut header).await?;
            let length = u32::from_be_bytes(header) as usize;
            if length == 0 || length as u64 > WORKER_MESSAGE_LIMIT {
                return Err(Error::other("worker output exceeds limit"));
            }
            let mut bytes = vec![0; length];
            self.stdout.read_exact(&mut bytes).await?;
            let output: StreamingProofResult =
                serde_json::from_slice(&bytes).map_err(Error::other)?;
            if output.result.generation != request.generation
                || output.result.info.challenge != request.challenge
                || output.result.info.number_of_iterations != request.iterations
                || output.result.proof.witness_type != 0
                || output.result.proof.witness.as_slice().len() != 100
                || output.result.proof.normalized_to_identity
                || output.checkpoint_budget_bytes > memory_bytes
                || output.witness_threads != witness_threads
            {
                return Err(Error::other("VDF worker returned mismatched work"));
            }
            Ok(output)
        })
        .await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "VDF worker deadline exceeded"))?
    }
}

pub async fn run_isolated(request: ProofRequest, timeout: Duration) -> Result<ProofResult, Error> {
    request.validate()?;
    run_isolated_inner(request, timeout, None).await
}

pub async fn run_regular_isolated(
    request: ProofRequest,
    timeout: Duration,
    memory_bytes: u64,
) -> Result<ProofResult, Error> {
    RegularProofRequest {
        request: request.clone(),
        memory_bytes,
    }
    .validate()?;
    run_isolated_inner(request, timeout, Some(memory_bytes)).await
}

async fn run_isolated_inner(
    request: ProofRequest,
    timeout: Duration,
    memory_bytes: Option<u64>,
) -> Result<ProofResult, Error> {
    if timeout.is_zero() || timeout > Duration::from_secs(86_400) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "worker timeout must be 1..86400 seconds",
        ));
    }
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .arg("timelord")
        .arg(if memory_bytes.is_some() {
            "regular-worker"
        } else {
            "worker"
        })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::other("worker stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("worker stdout unavailable"))?;
    let result = tokio::time::timeout(timeout, async {
        let encoded = match memory_bytes {
            Some(memory_bytes) => serde_json::to_vec(&RegularProofRequest {
                request: request.clone(),
                memory_bytes,
            }),
            None => serde_json::to_vec(&request),
        }
        .map_err(Error::other)?;
        stdin.write_all(&encoded).await?;
        drop(stdin);
        let mut bytes = Vec::new();
        stdout
            .take(WORKER_MESSAGE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > WORKER_MESSAGE_LIMIT {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "worker output exceeds limit",
            ));
        }
        let status = child.wait().await?;
        if !status.success() {
            return Err(Error::other(format!(
                "native VDF worker exited with {status}"
            )));
        }
        let result: ProofResult = serde_json::from_slice(&bytes).map_err(Error::other)?;
        if result.generation != request.generation
            || result.info.challenge != request.challenge
            || result.info.number_of_iterations != request.iterations
            || result.proof.witness_type != 0
            || result.proof.witness.as_slice().len() != 100
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "VDF worker returned mismatched work",
            ));
        }
        Ok(result)
    })
    .await
    .map_err(|_| Error::new(ErrorKind::TimedOut, "VDF worker deadline exceeded"))?;
    if result.is_err() {
        let _ = child.kill().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_worker_returns_real_verified_proof_and_generation() {
        let request = ProofRequest {
            generation: 7,
            challenge: Bytes32::from([17; 32]),
            input: ClassgroupElement::get_default_element(),
            iterations: 8,
            discriminant_bits: 32,
        };
        let result = prove(&request).unwrap();
        assert_eq!(result.generation, 7);
        assert!(result.proof.normalized_to_identity);
        let mut proof = result.info.output.data.to_vec();
        proof.extend_from_slice(result.proof.witness.as_slice());
        assert!(verify_vdf_serial(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            &proof,
            32,
            8,
            0
        ));
        assert!(!verify_vdf_serial(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            &proof,
            32,
            9,
            0
        ));
    }

    #[test]
    fn resource_limits_fail_closed() {
        let mut request = ProofRequest {
            generation: 0,
            challenge: Bytes32::default(),
            input: ClassgroupElement::get_default_element(),
            iterations: MAX_ITERATIONS + 1,
            discriminant_bits: 1024,
        };
        assert!(request.validate().is_err());
        request.iterations = 1;
        request.discriminant_bits = 2048;
        assert!(request.validate().is_err());
    }

    #[test]
    fn regular_worker_has_full_iteration_range_and_partial_proof_semantics() {
        let mut request = RegularProofRequest {
            request: ProofRequest {
                generation: 9,
                challenge: Bytes32::from([29; 32]),
                input: ClassgroupElement::get_default_element(),
                iterations: MAX_ITERATIONS + 1,
                discriminant_bits: 1024,
            },
            memory_bytes: 128 * 1024,
        };
        assert!(request.validate().is_ok());
        assert!(request.request.validate().is_err());
        request.request.iterations = 7;
        let result = prove_regular(&request).unwrap();
        assert!(!result.proof.normalized_to_identity);
        assert_eq!(result.info.number_of_iterations, 7);
        request.memory_bytes = 0;
        assert!(request.validate().is_err());
        request.memory_bytes = 9 * 1024 * 1024 * 1024;
        assert!(request.validate().is_err());
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    #[test]
    fn stream_reuses_only_matching_chain_origins_and_validates_every_proof() {
        let mut stream = StreamingRegularWorker::default();
        let mut request = RegularProofRequest {
            request: ProofRequest {
                generation: 42,
                challenge: [41; 32].into(),
                input: ClassgroupElement::get_default_element(),
                iterations: 0,
                discriminant_bits: 1024,
            },
            memory_bytes: 128 * 1024,
        };
        let mut maximum = 0;
        for iterations in [8, 16, 1024, 512, 1040] {
            request.request.iterations = iterations;
            let actual = stream.prove(&request).unwrap();
            let expected = prove_regular(&request).unwrap();
            assert_eq!(actual.result.info, expected.info);
            assert_eq!(actual.result.proof, expected.proof);
            if iterations >= maximum {
                assert_eq!(actual.new_squarings, iterations - maximum);
            }
            maximum = maximum.max(iterations);
        }
        for change in 0..4 {
            match change {
                0 => request.request.generation += 1,
                1 => request.request.challenge = [43; 32].into(),
                2 => request.memory_bytes *= 2,
                _ => request.request.input = prove_regular(&request).unwrap().info.output,
            }
            let output = stream.prove(&request).unwrap();
            assert_eq!(output.new_squarings, request.request.iterations);
            assert_eq!(output.reused_squarings, 0);
            assert_eq!(output.result.proof, prove_regular(&request).unwrap().proof);
        }
        request.memory_bytes = 0;
        assert!(stream.prove(&request).is_err());
    }
}
