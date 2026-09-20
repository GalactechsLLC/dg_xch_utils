use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes100};
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::traits::SizedBytes;
use dg_xch_vdf::proof::{prove_result, verify_vdf_serial};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAX_ITERATIONS: u64 = 1 << 26;
pub const WORKER_MESSAGE_LIMIT: u64 = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        if self.iterations == 0
            || self.iterations > MAX_ITERATIONS
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
    let bytes = prove_result(
        request.challenge.as_ref(),
        request.input.data.as_ref(),
        request.discriminant_bits,
        request.iterations,
    )
    .map_err(Error::other)?;
    if bytes.len() != 200
        || !verify_vdf_serial(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            &bytes,
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

pub async fn run_isolated(request: ProofRequest, timeout: Duration) -> Result<ProofResult, Error> {
    request.validate()?;
    if timeout.is_zero() || timeout > Duration::from_secs(3600) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "worker timeout must be 1..3600 seconds",
        ));
    }
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .arg("worker")
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
        stdin
            .write_all(&serde_json::to_vec(&request).map_err(Error::other)?)
            .await?;
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
}
