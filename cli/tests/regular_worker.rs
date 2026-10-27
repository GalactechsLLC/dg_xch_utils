#![cfg(feature = "timelord")]

use dg_xch_vdf::proof::verify_vdf_serial;

use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_timelord::worker::{ProofRequest, ProofResult, RegularProofRequest};

use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[test]
fn regular_worker_subprocess_returns_verified_full_discriminant_proof() {
    let request = RegularProofRequest {
        request: ProofRequest {
            generation: 42,
            challenge: Bytes32::from([41; 32]),
            input: ClassgroupElement::get_default_element(),
            iterations: 9,
            discriminant_bits: 1024,
        },
        memory_bytes: 128 * 1024,
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_dgx"))
        .args(["timelord", "regular-worker"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let result: ProofResult = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.generation, request.request.generation);
    assert_eq!(result.info.challenge, request.request.challenge);
    assert_eq!(result.info.number_of_iterations, request.request.iterations);
    assert!(!result.proof.normalized_to_identity);
    let mut proof = result.info.output.data.to_vec();
    proof.extend_from_slice(result.proof.witness.as_slice());
    assert!(verify_vdf_serial(
        request.request.challenge.as_ref(),
        request.request.input.data.as_ref(),
        &proof,
        request.request.discriminant_bits,
        request.request.iterations,
        0,
    ));
}

#[test]
fn streaming_worker_responds_without_eof_reuses_history_and_handles_late_targets() {
    use dg_xch_timelord::worker::StreamingProofResult;
    let mut child = Command::new(env!("CARGO_BIN_EXE_dgx"))
        .args(["timelord", "regular-stream-worker"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut maximum = 0;
    for iterations in [73, 4097, 8193, 4000, 10000] {
        let request = RegularProofRequest {
            request: ProofRequest {
                generation: 42,
                challenge: [41; 32].into(),
                input: ClassgroupElement::get_default_element(),
                iterations,
                discriminant_bits: 1024,
            },
            memory_bytes: 128 * 1024,
        };
        let encoded = serde_json::to_vec(&dg_xch_timelord::worker::StreamingRegularProofRequest {
            work: request.clone(),
            witness_threads: 4,
        })
        .unwrap();
        input
            .write_all(&(encoded.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(&encoded).unwrap();
        input.flush().unwrap();
        let mut header = [0; 4];
        output.read_exact(&mut header).unwrap();
        let length = u32::from_be_bytes(header) as usize;
        assert!(length <= dg_xch_timelord::worker::WORKER_MESSAGE_LIMIT as usize);
        let mut bytes = vec![0; length];
        output.read_exact(&mut bytes).unwrap();
        let response: StreamingProofResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.result.generation, 42);
        assert_eq!(response.result.info.number_of_iterations, iterations);
        assert!(!response.result.proof.normalized_to_identity);
        if iterations >= maximum {
            assert_eq!(response.new_squarings, iterations - maximum);
        }
        maximum = maximum.max(iterations);
        assert!(response.checkpoint_budget_bytes <= request.memory_bytes);
        let mut proof = response.result.info.output.data.to_vec();
        proof.extend_from_slice(response.result.proof.witness.as_slice());
        assert!(verify_vdf_serial(
            request.request.challenge.as_ref(),
            request.request.input.data.as_ref(),
            &proof,
            1024,
            iterations,
            0
        ));
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}

#[test]
fn streaming_worker_rejects_oversized_frames_before_allocating() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dgx"))
        .args(["timelord", "regular-stream-worker"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&u32::MAX.to_be_bytes())
        .unwrap();
    assert!(!child.wait().unwrap().success());
}

#[tokio::test]
async fn backend_cancellation_kills_busy_worker_and_next_request_has_no_stale_response() {
    use dg_xch_timelord::backend::{CpuBackend, VdfBackend};
    let backend = std::sync::Arc::new(CpuBackend::with_executable(
        std::time::Duration::from_secs(10),
        4 * 1024 * 1024,
        env!("CARGO_BIN_EXE_dgx").into(),
    ));
    let mut request = ProofRequest {
        generation: 42,
        challenge: [41; 32].into(),
        input: ClassgroupElement::get_default_element(),
        iterations: 1 << 26,
        discriminant_bits: 1024,
    };
    let busy = {
        let backend = backend.clone();
        let request = request.clone();
        tokio::spawn(async move { backend.prove_with_threads(request, 4).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    busy.abort();
    assert!(busy.await.unwrap_err().is_cancelled());
    for iterations in [73, 4097, 8193] {
        request.iterations = iterations;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            backend.prove_with_threads(request.clone(), 4),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.info.number_of_iterations, iterations);
        let mut proof = result.info.output.data.to_vec();
        proof.extend_from_slice(result.proof.witness.as_slice());
        assert!(verify_vdf_serial(
            request.challenge.as_ref(),
            request.input.data.as_ref(),
            &proof,
            1024,
            iterations,
            0
        ));
    }
}

#[test]
#[ignore = "release-mode streaming worker timing and resident memory measurement"]
fn streaming_worker_performance_and_memory() {
    use dg_xch_timelord::worker::{StreamingProofResult, StreamingRegularProofRequest};
    let mut child = Command::new(env!("CARGO_BIN_EXE_dgx"))
        .args(["timelord", "regular-stream-worker"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let started = std::time::Instant::now();
    let mut squares = 0;
    for index in 1..=20 {
        let request = StreamingRegularProofRequest {
            work: RegularProofRequest {
                request: ProofRequest {
                    generation: 42,
                    challenge: [64; 32].into(),
                    input: ClassgroupElement::get_default_element(),
                    iterations: index * 32768,
                    discriminant_bits: 1024,
                },
                memory_bytes: 64 * 1024 * 1024,
            },
            witness_threads: 4,
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        input
            .write_all(&(encoded.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(&encoded).unwrap();
        input.flush().unwrap();
        let mut header = [0; 4];
        output.read_exact(&mut header).unwrap();
        let length = u32::from_be_bytes(header) as usize;
        assert!(length as u64 <= dg_xch_timelord::worker::WORKER_MESSAGE_LIMIT);
        let mut bytes = vec![0; length];
        output.read_exact(&mut bytes).unwrap();
        let response: StreamingProofResult = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response.result.info.number_of_iterations, index * 32768);
        assert_eq!(response.new_squarings, 32768);
        squares += response.new_squarings;
    }
    eprintln!(
        "isolated streaming worker: points=20 elapsed_ms={:.1} new_squarings={squares}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
        for line in status
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
        {
            eprintln!("worker {line}");
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
