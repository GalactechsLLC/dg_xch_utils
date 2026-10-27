use super::*;
use crate::discriminant::create_discriminant_int;
use crate::form::{B_BYTES, get_b, get_block};
use num_traits::One;

#[test]
fn memory_bounded_prover_preserves_wesolowski_proofs() {
    let challenge = [37; 32];
    let mut input = [0; 100];
    input[0] = 8;
    for iterations in [1, 73, 4097] {
        let bounded = prove_result_bounded(&challenge, &input, 32, iterations, 128 * 1024).unwrap();
        let ordinary = prove_result(&challenge, &input, 32, iterations).unwrap();
        assert_eq!(bounded, ordinary);
        assert!(verify_vdf_serial(
            &challenge, &input, &bounded, 32, iterations, 0
        ));
    }
    let first = prove_result_bounded(&challenge, &input, 1024, 8, 128 * 1024).unwrap();
    let second = prove_result_bounded(&challenge, &first[..100], 1024, 9, 128 * 1024).unwrap();
    let whole = prove_result(&challenge, &input, 1024, 17).unwrap();
    assert_eq!(second[..100], whole[..100]);
    assert!(verify_vdf_serial(
        &challenge,
        &first[..100],
        &second,
        1024,
        9,
        0
    ));
}

#[test]
fn bounded_parameters_cover_consensus_iterations_without_compact_ceiling() {
    for iterations in [1u64 << 26, 1u64 << 27, 1u64 << 40, u64::MAX - 4095] {
        let budget = 128 * 1024;
        let (spacing, window) = bounded_parameters(iterations, budget).unwrap();
        let checkpoints = iterations.div_ceil(spacing.checked_mul(window).unwrap());
        assert!(64 * 1024 + 512 * ((1u64 << window) + checkpoints) <= budget);
        assert!((1..=20).contains(&window));
        assert!(spacing > 0);
    }
    assert_eq!(
        bounded_parameters(100, 64 * 1024),
        Err(Error::ProverMemoryLimit)
    );
}

#[test]
fn memoized_verify_vdf_is_identical_to_uncached() {
    let challenge = hex::decode("9f1fbdf9b1a0b6912cd5e2a4b40a2ffb1810b513994b1dd6c4e6df9c30de5f6e")
        .expect("challenge hex is valid");
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let mut x_s = [0u8; 100];
    x_s[0] = 0x08;
    let x = Form::deserialize(&discriminant, &x_s).unwrap();
    let proof = prove_with_discriminant(&discriminant, &x, 100).unwrap();

    // Miss, then hit — both must equal the uncached result.
    assert!(verify_vdf(&challenge, &x_s, &proof, 1024, 100, 0));
    assert!(verify_vdf(&challenge, &x_s, &proof, 1024, 100, 0));
    assert!(verify_vdf_uncached(
        &challenge, &x_s, &proof, 1024, 100, 0, true
    ));

    // An invalid proof of the correct length: a distinct key, recomputed, false — on the
    // miss and the hit path (i.e. `false` results are cached too, and a bad proof can never
    // inherit the valid proof's cached `true`).
    let bad = vec![0xFFu8; proof.len()];
    assert!(!verify_vdf(&challenge, &x_s, &bad, 1024, 100, 0));
    assert!(!verify_vdf(&challenge, &x_s, &bad, 1024, 100, 0));
    assert!(!verify_vdf_uncached(
        &challenge, &x_s, &bad, 1024, 100, 0, true
    ));

    // A changed iteration count is a distinct key (length-prefixed fields, fixed-width
    // trailer — no concatenation ambiguity), so the cached `true` above cannot leak here.
    assert!(!verify_vdf(&challenge, &x_s, &proof, 1024, 101, 0));
}

/// The serial (no inner threads) verification path must be result-identical to the parallel
/// path, below the memo — valid proof, valid recursive proof, and an invalid proof alike.
#[test]
fn serial_verification_is_result_identical_to_parallel() {
    let challenge = hex::decode("1f0c94d5d1f5ea25be3b04e04d17806bcc9a0dbcdcc16346eb388937b5981c37")
        .expect("challenge hex is valid");
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let mut x_s = [0u8; 100];
    x_s[0] = 0x08;
    let x = Form::deserialize(&discriminant, &x_s).unwrap();
    let proof = prove_with_discriminant(&discriminant, &x, 73).unwrap();

    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 73, 0, true).is_ok());
    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 73, 0, false).is_ok());
    // Wrong iteration count: both paths must reject.
    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 74, 0, true).is_err());
    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 74, 0, false).is_err());
    // The public serial entry point (through the memo) agrees as well.
    assert!(verify_vdf_serial(&challenge, &x_s, &proof, 1024, 73, 0));
}

/// The memo stays bounded past capacity.
#[test]
fn verify_memo_stays_bounded() {
    let challenge = hex::decode("8be26af52b34a1a7c47a35c7f0c1add793d5b6e2b0e56e6e970cbd6bd4e17e2a")
        .expect("challenge hex is valid");
    let x_s = [0u8; 100];
    for i in 0..(VERIFY_MEMO_CAPACITY as u64 + 100) {
        let _ = verify_vdf(&challenge, &x_s, &[0u8; 8], 1024, i, 0);
    }
    assert!(verify_memo_len() <= VERIFY_MEMO_CAPACITY);
}

#[test]
fn recursive_proof_verifies_segment_before_final_witness() {
    let challenge = hex::decode("ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb")
        .expect("challenge hex is valid");
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let mut x_s = [0u8; 100];
    x_s[0] = 0x08;
    let x = Form::deserialize(&discriminant, &x_s).unwrap();

    let segment_iterations = 24;
    let final_iterations = 31;
    let segment_proof = prove_with_discriminant(&discriminant, &x, segment_iterations).unwrap();
    let segment_y = Form::deserialize(&discriminant, &segment_proof[..FORM_SIZE]).unwrap();
    let segment_witness = &segment_proof[FORM_SIZE..FORM_SIZE * 2];
    let segment_b = get_b(&discriminant, &x, &segment_y).unwrap();

    let mut recursive_proof =
        prove_with_discriminant(&discriminant, &segment_y, final_iterations).unwrap();
    recursive_proof.extend_from_slice(&segment_iterations.to_be_bytes());
    recursive_proof.extend_from_slice(&fixed_be_bytes(&segment_b, B_BYTES));
    recursive_proof.extend_from_slice(segment_witness);

    check_n_wesolowski(
        &discriminant,
        &x_s,
        &recursive_proof,
        segment_iterations + final_iterations,
        1,
    )
    .expect("depth-1 proof should verify");
}

fn fixed_be_bytes(value: &BigInt, size: usize) -> Vec<u8> {
    let (_, bytes) = value.to_bytes_be();
    assert!(bytes.len() <= size);
    let mut out = vec![0u8; size - bytes.len()];
    out.extend_from_slice(&bytes);
    out
}

fn reference_prover(
    discriminant: &BigInt,
    input: &Form,
    iterations: u64,
    spacing: u64,
    window: u64,
) -> Vec<u8> {
    let bound = nucomp_bound(discriminant);
    let mut output = input.clone();
    let mut checkpoints = Vec::new();
    for i in 0..iterations {
        if i.is_multiple_of(spacing * window) {
            checkpoints.push(output.clone());
        }
        output = output.square_with(discriminant, &bound).unwrap();
    }
    let witness = generate_wesolowski_reference(
        discriminant,
        &output,
        input,
        &checkpoints,
        iterations,
        window,
        spacing,
    )
    .unwrap();
    let bits = bit_len(discriminant);
    let mut bytes = output.serialize(bits).unwrap().to_vec();
    bytes.extend_from_slice(&witness.serialize(bits).unwrap());
    bytes
}

#[test]
fn continuous_prover_preserves_full_proof_bytes_for_generator_and_nonidentity_inputs() {
    for bits in [16, 32, 256, 1024] {
        let discriminant = create_discriminant_int(&[64; 32], bits).unwrap();
        let bound = nucomp_bound(&discriminant);
        let mut input = Form::generator(&discriminant).unwrap();
        for iterations in [1, 73, 4097] {
            let expected = reference_prover(&discriminant, &input, iterations, 3, 5);
            let actual = prove_with_parameters(&discriminant, &input, iterations, 3, 5).unwrap();
            assert_eq!(actual, expected, "bits={bits} iterations={iterations}");
            verify_wesolowski(
                &discriminant,
                &nucomp_bound(&discriminant),
                &input,
                &Form::deserialize(&discriminant, &actual[..FORM_SIZE]).unwrap(),
                &Form::deserialize(&discriminant, &actual[FORM_SIZE..]).unwrap(),
                iterations,
                false,
            )
            .unwrap();
            // Exercise continuation from a reduced nonidentity form as well as the generator.
            input = input.square_with(&discriminant, &bound).unwrap();
        }
    }
}

#[test]
#[ignore = "release-mode prover timing comparison"]
fn continuous_prover_performance_comparison() {
    let discriminant = create_discriminant_int(&[64; 32], 1024).unwrap();
    let input = Form::generator(&discriminant).unwrap();
    for iterations in [32_768, 262_144] {
        let (spacing, window) = bounded_parameters(iterations, 128 * 1024 * 1024).unwrap();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let reference = reference_prover(&discriminant, &input, iterations, spacing, window);
            let before = started.elapsed();
            let started = std::time::Instant::now();
            let optimized =
                prove_with_parameters(&discriminant, &input, iterations, spacing, window).unwrap();
            let after = started.elapsed();
            assert_eq!(optimized, reference);
            eprintln!(
                "prover comparison: iterations={iterations} old_ms={:.2} new_ms={:.2} speedup={:.3}",
                before.as_secs_f64() * 1000.0,
                after.as_secs_f64() * 1000.0,
                before.as_secs_f64() / after.as_secs_f64()
            );
        }
    }
}

fn generate_wesolowski_reference(
    discriminant: &BigInt,
    y: &Form,
    x_init: &Form,
    intermediates: &[Form],
    num_iterations: u64,
    k: u64,
    l: u64,
) -> Result<Form> {
    let b = get_b(discriminant, x_init, y)?;
    let k1 = k / 2;
    let k0 = k - k1;
    let bucket_count = 1usize
        .checked_shl(k.try_into().map_err(|_| Error::InvalidProofParameters)?)
        .ok_or(Error::InvalidProofParameters)?;
    let bucket0_count = 1usize
        .checked_shl(k0.try_into().map_err(|_| Error::InvalidProofParameters)?)
        .ok_or(Error::InvalidProofParameters)?;
    let bucket1_count = 1usize
        .checked_shl(k1.try_into().map_err(|_| Error::InvalidProofParameters)?)
        .ok_or(Error::InvalidProofParameters)?;

    // NUCOMP bound computed once for every loop composition below.
    let nl = nucomp_bound(discriminant);
    let mut x = Form::identity(discriminant)?;
    for j in (0..l).rev() {
        x = fast_pow_form_with(&x, discriminant, &nl, &(BigInt::one() << k as usize))?;

        let mut ys = Vec::new();
        ys.try_reserve_exact(bucket_count)
            .map_err(|_| Error::ProverMemoryLimit)?;
        ys.resize(bucket_count, Form::identity(discriminant)?);
        let chunks =
            num_iterations.div_ceil(k.checked_mul(l).ok_or(Error::InvalidProofParameters)?);
        for i in 0..chunks {
            let block_number = i
                .checked_mul(l)
                .and_then(|base| base.checked_add(j))
                .ok_or(Error::InvalidProofParameters)?;
            if block_number
                .checked_add(1)
                .and_then(|next| k.checked_mul(next))
                .is_some_and(|end| num_iterations >= end)
            {
                let block = get_block(block_number, k, num_iterations, &b)?;
                let block_index =
                    usize::try_from(block).map_err(|_| Error::InvalidProofParameters)?;
                if block_index >= ys.len() {
                    return Err(Error::InvalidProofParameters);
                }
                ys[block_index] = ys[block_index].multiply_with(
                    intermediates
                        .get(i as usize)
                        .ok_or(Error::InvalidProofParameters)?,
                    discriminant,
                    &nl,
                )?;
            }
        }

        for b1 in 0..bucket1_count {
            let mut z = Form::identity(discriminant)?;
            for b0 in 0..bucket0_count {
                z = z.multiply_with(&ys[b1 * bucket0_count + b0], discriminant, &nl)?;
            }
            z = fast_pow_form_with(
                &z,
                discriminant,
                &nl,
                &BigInt::from((b1 as u64) * (1u64 << k0)),
            )?;
            x = x.multiply_with(&z, discriminant, &nl)?;
        }

        for b0 in 0..bucket0_count {
            let mut z = Form::identity(discriminant)?;
            for b1 in 0..bucket1_count {
                z = z.multiply_with(&ys[b1 * bucket0_count + b0], discriminant, &nl)?;
            }
            z = fast_pow_form_with(&z, discriminant, &nl, &BigInt::from(b0 as u64))?;
            x = x.multiply_with(&z, discriminant, &nl)?;
        }
    }

    x.reduce();
    Ok(x)
}

#[test]
fn streaming_prover_reuses_squarings_and_preserves_proofs_through_coarsening_and_late_targets() {
    let challenge = [64; 32];
    let mut input = [0; 100];
    input[0] = 8;
    for threads in [1, 2, 3, 4] {
        for bits in [32, 1024] {
            let mut prover =
                StreamingProver::new_with_threads(&challenge, &input, bits, 128 * 1024, threads)
                    .unwrap();
            let mut previous = 0;
            for target in [1, 73, 4097, 8193, 17001] {
                let actual = prover.prove(target).unwrap();
                let reference =
                    prove_result_bounded(&challenge, &input, bits, target, 128 * 1024).unwrap();
                assert_eq!(actual.bytes, reference);
                assert_eq!(actual.new_squarings, target - previous);
                assert_eq!(actual.reused_squarings, previous);
                assert!(actual.checkpoint_budget_bytes <= 128 * 1024);
                assert!(verify_vdf_serial(
                    &challenge,
                    &input,
                    &actual.bytes,
                    bits,
                    target,
                    0
                ));
                previous = target;
            }
            assert!(prover.spacing > 1);
            for target in [1, 73, 4097, 16000, 17001] {
                let actual = prover.prove(target).unwrap();
                assert_eq!(
                    actual.bytes,
                    prove_result_bounded(&challenge, &input, bits, target, 128 * 1024).unwrap()
                );
                assert!(actual.new_squarings < prover.spacing * prover.window);
            }
            let next = prover.prove(18000).unwrap();
            assert_eq!(next.new_squarings, 999);
            assert_eq!(
                next.bytes,
                prove_result_bounded(&challenge, &input, bits, 18000, 128 * 1024).unwrap()
            );
        }
    }
}

#[test]
#[ignore = "release-mode multi-signage prover benchmark"]
fn streaming_signage_performance_comparison() {
    let challenge = [64; 32];
    let mut input_bytes = [0; 100];
    input_bytes[0] = 8;
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let input = Form::deserialize(&discriminant, &input_bytes).unwrap();
    let targets: Vec<u64> = (1..=20).map(|index| index * 32768).collect();
    let started = std::time::Instant::now();
    let mut reference = Vec::new();
    for &target in &targets {
        let (spacing, window) = bounded_parameters(target, 64 * 1024 * 1024).unwrap();
        reference.push(reference_prover(
            &discriminant,
            &input,
            target,
            spacing,
            window,
        ));
    }
    let before = started.elapsed();
    for (memory, threads) in [
        (64 * 1024 * 1024, 1),
        (64 * 1024 * 1024, 4),
        (4 * 1024 * 1024, 4),
    ] {
        let started = std::time::Instant::now();
        let mut prover =
            StreamingProver::new_with_threads(&challenge, &input_bytes, 1024, memory, threads)
                .unwrap();
        let mut squares = 0;
        for (&target, expected) in targets.iter().zip(&reference) {
            let actual = prover.prove(target).unwrap();
            assert_eq!(&actual.bytes, expected);
            squares += actual.new_squarings;
        }
        let after = started.elapsed();
        assert_eq!(squares, *targets.last().unwrap());
        eprintln!(
            "multi-signage comparison: points={} threads={threads} memory_bytes={memory} old_ms={:.1} new_ms={:.1} speedup={:.2} old_squarings={} new_squarings={squares}",
            targets.len(),
            before.as_secs_f64() * 1000.0,
            after.as_secs_f64() * 1000.0,
            before.as_secs_f64() / after.as_secs_f64(),
            targets.iter().sum::<u64>()
        );
    }
}
