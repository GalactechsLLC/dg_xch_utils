use crate::discriminant::{bigint_from_be, bit_len, create_discriminant_int};
use crate::error::{Error, Result};
use crate::form::{
    B_BYTES, FORM_SIZE, Form, fast_pow_form_pair_with, fast_pow_form_with, fast_pow_u64_mod, get_b,
    nucomp_bound,
};
use num_bigint::BigInt;
use rayon::prelude::*;

const SEGMENT_LEN: usize = 8 + B_BYTES + FORM_SIZE;

pub fn verify_n_wesolowski(
    discriminant: &[u8],
    x_s: &[u8],
    proof: &[u8],
    num_iterations: u64,
    recursion: u64,
) -> bool {
    verify_n_wesolowski_result(discriminant, x_s, proof, num_iterations, recursion).is_ok()
}

const VERIFY_MEMO_CAPACITY: usize = 1000;

pub(crate) fn verify_memo() -> &'static crate::memo::Memo<Vec<u8>, bool> {
    static MEMO: std::sync::OnceLock<crate::memo::Memo<Vec<u8>, bool>> = std::sync::OnceLock::new();
    MEMO.get_or_init(|| crate::memo::Memo::new(VERIFY_MEMO_CAPACITY))
}

fn verify_memo_key(
    challenge: &[u8],
    x_s: &[u8],
    proof: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
    recursion: u64,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(challenge.len() + x_s.len() + proof.len() + 48);
    for field in [challenge, x_s, proof] {
        key.extend_from_slice(&(field.len() as u64).to_be_bytes());
        key.extend_from_slice(field);
    }
    key.extend_from_slice(&(discriminant_size_bits as u64).to_be_bytes());
    key.extend_from_slice(&num_iterations.to_be_bytes());
    key.extend_from_slice(&recursion.to_be_bytes());
    key
}

pub fn verify_vdf(
    challenge: &[u8],
    x_s: &[u8],
    proof: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
    recursion: u64,
) -> bool {
    verify_vdf_impl(
        challenge,
        x_s,
        proof,
        discriminant_size_bits,
        num_iterations,
        recursion,
        true,
    )
}

/// [`verify_vdf`] without the two-thread split inside each segment exponentiation. Same result,
/// same memo — group arithmetic is scheduling-independent. For SATURATED batch drains: when a
/// window drain already runs one proof per core, the inner spawn adds oversubscription and a
/// thread spawn/join per segment while buying no wall. The parallel variant remains right for
/// latency-bound single proofs (the live tip).
pub fn verify_vdf_serial(
    challenge: &[u8],
    x_s: &[u8],
    proof: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
    recursion: u64,
) -> bool {
    verify_vdf_impl(
        challenge,
        x_s,
        proof,
        discriminant_size_bits,
        num_iterations,
        recursion,
        false,
    )
}

fn verify_vdf_impl(
    challenge: &[u8],
    x_s: &[u8],
    proof: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
    recursion: u64,
    parallel: bool,
) -> bool {
    let key = verify_memo_key(
        challenge,
        x_s,
        proof,
        discriminant_size_bits,
        num_iterations,
        recursion,
    );
    let result = verify_memo().get_or_init(key, || {
        verify_vdf_uncached(
            challenge,
            x_s,
            proof,
            discriminant_size_bits,
            num_iterations,
            recursion,
            parallel,
        )
    });
    *result.get().expect("verification initialized")
}

fn verify_vdf_uncached(
    challenge: &[u8],
    x_s: &[u8],
    proof: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
    recursion: u64,
    parallel: bool,
) -> bool {
    let Ok(discriminant) = create_discriminant_int(challenge, discriminant_size_bits) else {
        return false;
    };
    check_n_wesolowski_impl(
        &discriminant,
        x_s,
        proof,
        num_iterations,
        recursion,
        parallel,
    )
    .is_ok()
}

pub fn prove(
    challenge: &[u8],
    x_s: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
) -> Option<Vec<u8>> {
    prove_result(challenge, x_s, discriminant_size_bits, num_iterations).ok()
}

pub fn verify_n_wesolowski_result(
    discriminant: &[u8],
    x_s: &[u8],
    proof: &[u8],
    num_iterations: u64,
    recursion: u64,
) -> Result<()> {
    let discriminant = -bigint_from_be(discriminant);
    check_n_wesolowski(&discriminant, x_s, proof, num_iterations, recursion)
}

pub fn prove_result(
    challenge: &[u8],
    x_s: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
) -> Result<Vec<u8>> {
    let discriminant = create_discriminant_int(challenge, discriminant_size_bits)?;
    let x = Form::deserialize(&discriminant, x_s)?;
    prove_with_discriminant(&discriminant, &x, num_iterations)
}

pub fn prove_result_bounded(
    challenge: &[u8],
    input: &[u8],
    discriminant_bits: usize,
    iterations: u64,
    memory_bytes: u64,
) -> Result<Vec<u8>> {
    let (spacing, window) = bounded_parameters(iterations, memory_bytes)?;
    let discriminant = create_discriminant_int(challenge, discriminant_bits)?;
    let input = Form::deserialize(&discriminant, input)?;
    prove_with_parameters(&discriminant, &input, iterations, spacing, window)
}

/// A memory-bounded squaring history for multiple proofs from one chain origin.
/// Checkpoints are coarsened in place as the chain grows, without repeating squarings.
pub struct StreamingProver {
    discriminant: BigInt,
    input: Form,
    squaring: crate::form::SquaringState,
    iterations: u64,
    window: u64,
    spacing: u64,
    checkpoints: Vec<Form>,
    checkpoint_limit: usize,
    witness_threads: usize,
    witness_pool: Option<rayon::ThreadPool>,
}

pub struct StreamingProof {
    pub bytes: Vec<u8>,
    pub new_squarings: u64,
    pub reused_squarings: u64,
    pub checkpoint_budget_bytes: u64,
}

impl StreamingProver {
    pub fn new(
        challenge: &[u8],
        input: &[u8],
        discriminant_bits: usize,
        memory_bytes: u64,
    ) -> Result<Self> {
        Self::new_with_threads(challenge, input, discriminant_bits, memory_bytes, 1)
    }

    pub fn new_with_threads(
        challenge: &[u8],
        input: &[u8],
        discriminant_bits: usize,
        memory_bytes: u64,
        witness_threads: usize,
    ) -> Result<Self> {
        if !(1..=4).contains(&witness_threads) {
            return Err(Error::InvalidProofParameters);
        }
        let witness_pool = if witness_threads > 1 {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(witness_threads)
                    .build()
                    .map_err(|_| Error::ProverMemoryLimit)?,
            )
        } else {
            None
        };
        let bucket_budget = 1024 * witness_threads as u64;
        let available = memory_bytes
            .checked_sub(64 * 1024)
            .ok_or(Error::ProverMemoryLimit)?;
        let mut window = 12;
        while window > 1 && (1u64 << window) * bucket_budget > available / 4 {
            window -= 1;
        }
        let checkpoint_limit = usize::try_from(
            available
                .checked_sub((1u64 << window) * bucket_budget)
                .ok_or(Error::ProverMemoryLimit)?
                / 512,
        )
        .map_err(|_| Error::ProverMemoryLimit)?;
        if checkpoint_limit == 0 {
            return Err(Error::ProverMemoryLimit);
        }
        let discriminant = create_discriminant_int(challenge, discriminant_bits)?;
        let input = Form::deserialize(&discriminant, input)?;
        let squaring =
            crate::form::SquaringState::new(&input, &discriminant, &nucomp_bound(&discriminant));
        let mut checkpoints = Vec::new();
        checkpoints
            .try_reserve_exact(checkpoint_limit)
            .map_err(|_| Error::ProverMemoryLimit)?;
        Ok(Self {
            discriminant,
            input,
            squaring,
            iterations: 0,
            window,
            spacing: 1,
            checkpoints,
            checkpoint_limit,
            witness_threads,
            witness_pool,
        })
    }

    pub fn prove(&mut self, iterations: u64) -> Result<StreamingProof> {
        if iterations == 0 {
            return Err(Error::InvalidProofParameters);
        }
        let before = self.iterations;
        while self.iterations < iterations {
            let mut stride = self
                .window
                .checked_mul(self.spacing)
                .ok_or(Error::InvalidProofParameters)?;
            if self.iterations.is_multiple_of(stride) {
                if self.checkpoints.len() == self.checkpoint_limit {
                    let mut index = 0;
                    self.checkpoints.retain(|_| {
                        let keep = index % 2 == 0;
                        index += 1;
                        keep
                    });
                    self.spacing = self
                        .spacing
                        .checked_mul(2)
                        .ok_or(Error::InvalidProofParameters)?;
                    stride = self
                        .window
                        .checked_mul(self.spacing)
                        .ok_or(Error::InvalidProofParameters)?;
                }
                if self.iterations.is_multiple_of(stride) {
                    self.checkpoints.push(self.squaring.snapshot());
                }
            }
            self.squaring.square();
            self.iterations += 1;
        }
        let stride = self
            .window
            .checked_mul(self.spacing)
            .ok_or(Error::InvalidProofParameters)?;
        let (output, new_squarings) = if iterations == self.iterations {
            (self.squaring.snapshot(), self.iterations - before)
        } else {
            // A late farmer can supply a candidate whose infusion precedes our latest SP.
            // Recover its output from the nearest retained checkpoint without discarding history.
            let index =
                usize::try_from(iterations / stride).map_err(|_| Error::ProverMemoryLimit)?;
            let checkpoint = self
                .checkpoints
                .get(index)
                .ok_or(Error::InvalidProofParameters)?;
            let mut tail = crate::form::SquaringState::new(
                checkpoint,
                &self.discriminant,
                &nucomp_bound(&self.discriminant),
            );
            let steps = iterations % stride;
            for _ in 0..steps {
                tail.square();
            }
            (tail.snapshot(), steps)
        };
        let needed =
            usize::try_from(iterations.div_ceil(stride)).map_err(|_| Error::ProverMemoryLimit)?;
        let witness = generate_wesolowski_parallel(
            &self.discriminant,
            &output,
            &self.input,
            self.checkpoints
                .get(..needed)
                .ok_or(Error::InvalidProofParameters)?,
            iterations,
            self.window,
            self.spacing,
            self.witness_threads,
            self.witness_pool.as_ref(),
        )?;
        let bits = bit_len(&self.discriminant);
        let mut bytes = output.serialize(bits)?.to_vec();
        bytes.extend_from_slice(&witness.serialize(bits)?);
        Ok(StreamingProof {
            bytes,
            new_squarings,
            reused_squarings: iterations.saturating_sub(new_squarings),
            checkpoint_budget_bytes: 64 * 1024
                + self.checkpoint_limit as u64 * 512
                + (1u64 << self.window) * 1024 * self.witness_threads as u64,
        })
    }
}

fn bounded_parameters(iterations: u64, memory_bytes: u64) -> Result<(u64, u64)> {
    const FORM_BUDGET: u64 = 512;
    const BUCKET_BUDGET: u64 = 1024;
    const FIXED_BUDGET: u64 = 64 * 1024;
    let available = memory_bytes
        .checked_sub(FIXED_BUDGET)
        .ok_or(Error::ProverMemoryLimit)?;
    let (initial_spacing, mut window) = approximate_parameters(iterations).unwrap_or((1, 20));
    while window > 1 && (1u64 << window) > available / (4 * BUCKET_BUDGET) {
        window -= 1;
    }
    let bucket_bytes = (1u64 << window)
        .checked_mul(BUCKET_BUDGET)
        .ok_or(Error::ProverMemoryLimit)?;
    let checkpoints = available
        .checked_sub(bucket_bytes)
        .ok_or(Error::ProverMemoryLimit)?
        / FORM_BUDGET;
    if checkpoints == 0 {
        return Err(Error::ProverMemoryLimit);
    }
    let span = window
        .checked_mul(checkpoints)
        .ok_or(Error::ProverMemoryLimit)?;
    let spacing = initial_spacing.max(iterations.div_ceil(span));
    Ok((spacing, window))
}

pub fn check_n_wesolowski(
    discriminant: &BigInt,
    x_s: &[u8],
    proof: &[u8],
    iterations: u64,
    depth: u64,
) -> Result<()> {
    check_n_wesolowski_impl(discriminant, x_s, proof, iterations, depth, true)
}

fn check_n_wesolowski_impl(
    discriminant: &BigInt,
    x_s: &[u8],
    proof: &[u8],
    mut iterations: u64,
    depth: u64,
    parallel: bool,
) -> Result<()> {
    if bit_len(discriminant) == 0 || bit_len(discriminant) > 1024 {
        return Err(Error::InvalidDiscriminant);
    }

    let expected_len = FORM_SIZE
        .checked_mul(2)
        .and_then(|base| base.checked_add(SEGMENT_LEN.checked_mul(depth as usize)?))
        .ok_or(Error::InvalidProofLength)?;
    if proof.len() != expected_len {
        return Err(Error::InvalidProofLength);
    }

    let mut offset = proof.len();
    let mut x = Form::deserialize(discriminant, x_s)?;
    // The discriminant and its NUCOMP bound are shared by every segment and the final Wesolowski
    // check — compute the bound once per proof, as the prover already does. The verifier was
    // paying two 1024-bit sqrts per exponentiation plus a b²−4ac discriminant recompute in every
    // segment's composition.
    let nl = nucomp_bound(discriminant);
    while offset > FORM_SIZE * 2 {
        offset -= SEGMENT_LEN;
        let segment_iters = u64::from_be_bytes(
            proof[offset..offset + 8]
                .try_into()
                .expect("slice has 8 bytes"),
        );
        let b = bigint_from_be(&proof[offset + 8..offset + 8 + B_BYTES]);
        let proof_form = Form::deserialize(
            discriminant,
            &proof[offset + 8 + B_BYTES..offset + SEGMENT_LEN],
        )?;
        x = verify_segment(
            discriminant,
            &nl,
            &x,
            &proof_form,
            &b,
            segment_iters,
            parallel,
        )?;

        if segment_iters > iterations {
            return Err(Error::InvalidSegmentIterations);
        }
        iterations -= segment_iters;
    }

    let y = Form::deserialize(discriminant, &proof[..FORM_SIZE])?;
    let witness = Form::deserialize(discriminant, &proof[FORM_SIZE..FORM_SIZE * 2])?;
    verify_wesolowski(discriminant, &nl, &x, &y, &witness, iterations, parallel)
}

// The Wesolowski check consumes the PRODUCT witness^b · x^r, never the individual powers.
// `parallel` chooses how the product is evaluated — group-identical results either way:
//  * parallel (single-proof latency, the live tip): the two exponentiations are independent, so
//    the two-thread split puts one ~336-op chain on each thread (critical path ≈ one chain) and
//    composes the results.
//  * serial (saturated batch drains, where throughput ≡ total work): the fused Straus/Shamir
//    chain shares the squaring run between the two exponents — ~411 group ops against the
//    two-chain 673 (0.61×), on one thread with no spawn/join.
fn pow_pair_product(
    witness: &Form,
    witness_exp: &BigInt,
    x: &Form,
    x_exp: &BigInt,
    discriminant: &BigInt,
    nl: &BigInt,
    parallel: bool,
) -> Result<Form> {
    if parallel {
        let (f1, f2) = std::thread::scope(|s| {
            let h1 = s.spawn(|| fast_pow_form_with(witness, discriminant, nl, witness_exp));
            let f2 = fast_pow_form_with(x, discriminant, nl, x_exp);
            (h1.join().unwrap_or(Err(Error::InvalidForm)), f2)
        });
        f1?.multiply_with(&f2?, discriminant, nl)
    } else {
        fast_pow_form_pair_with(witness, witness_exp, x, x_exp, discriminant, nl)
    }
}

fn verify_segment(
    discriminant: &BigInt,
    nl: &BigInt,
    x: &Form,
    witness: &Form,
    b: &BigInt,
    iterations: u64,
    parallel: bool,
) -> Result<Form> {
    let r = fast_pow_u64_mod(2, iterations, b)?;
    let y = pow_pair_product(witness, b, x, &r, discriminant, nl, parallel)?;
    if get_b(discriminant, x, &y)? == *b {
        Ok(y)
    } else {
        Err(Error::InvalidForm)
    }
}

fn verify_wesolowski(
    discriminant: &BigInt,
    nl: &BigInt,
    x: &Form,
    y: &Form,
    witness: &Form,
    iterations: u64,
    parallel: bool,
) -> Result<()> {
    let b = get_b(discriminant, x, y)?;
    let r = fast_pow_u64_mod(2, iterations, &b)?;
    if pow_pair_product(witness, &b, x, &r, discriminant, nl, parallel)? == *y {
        Ok(())
    } else {
        Err(Error::InvalidForm)
    }
}

fn prove_with_discriminant(
    discriminant: &BigInt,
    x: &Form,
    num_iterations: u64,
) -> Result<Vec<u8>> {
    let (spacing, window) = approximate_parameters(num_iterations)?;
    prove_with_parameters(discriminant, x, num_iterations, spacing, window)
}

fn prove_with_parameters(
    discriminant: &BigInt,
    x: &Form,
    num_iterations: u64,
    l: u64,
    k: u64,
) -> Result<Vec<u8>> {
    let d_bits = bit_len(discriminant);
    let kl = k.checked_mul(l).ok_or(Error::InvalidProofParameters)?;
    if kl == 0 || k > 20 {
        return Err(Error::InvalidProofParameters);
    }
    let intermediate_count = num_iterations.div_ceil(kl);
    let mut intermediates = Vec::new();
    intermediates
        .try_reserve_exact(
            usize::try_from(intermediate_count).map_err(|_| Error::ProverMemoryLimit)?,
        )
        .map_err(|_| Error::ProverMemoryLimit)?;
    // NUCOMP bound computed once for the whole iterated-squaring run.
    let nl = nucomp_bound(discriminant);

    let mut squaring = crate::form::SquaringState::new(x, discriminant, &nl);
    for i in 0..num_iterations {
        if i.is_multiple_of(kl) {
            intermediates.push(squaring.snapshot());
        }
        squaring.square();
    }
    let y = squaring.snapshot();

    let witness = generate_wesolowski(discriminant, &y, x, &intermediates, num_iterations, k, l)?;
    let mut out = y.serialize(d_bits)?.to_vec();
    out.extend_from_slice(&witness.serialize(d_bits)?);
    Ok(out)
}

fn generate_wesolowski(
    discriminant: &BigInt,
    y: &Form,
    x_init: &Form,
    intermediates: &[Form],
    num_iterations: u64,
    k: u64,
    l: u64,
) -> Result<Form> {
    generate_wesolowski_parallel(
        discriminant,
        y,
        x_init,
        intermediates,
        num_iterations,
        k,
        l,
        1,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_wesolowski_parallel(
    discriminant: &BigInt,
    y: &Form,
    x_init: &Form,
    intermediates: &[Form],
    num_iterations: u64,
    k: u64,
    l: u64,
    threads: usize,
    pool: Option<&rayon::ThreadPool>,
) -> Result<Form> {
    let b = get_b(discriminant, x_init, y)?;
    if num_iterations < bit_len(&b) as u64 {
        return Form::identity(discriminant);
    }
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
    let context = crate::form::CompositionContext::new(discriminant)?;
    let stride = k.checked_mul(l).ok_or(Error::InvalidProofParameters)?;
    let stride_mod = fast_pow_u64_mod(2, stride, &b)?;
    let checkpoints = num_iterations.div_ceil(stride);
    let range = |start: u64, end: u64, first: u64, last: u64| -> Result<crate::form::LimbForm> {
        let mut x = context.identity();
        for j in (start..end).rev() {
            for _ in 0..k {
                x = context.square(x);
            }

            let mut ys = Vec::new();
            ys.try_reserve_exact(bucket_count)
                .map_err(|_| Error::ProverMemoryLimit)?;
            ys.resize(bucket_count, None);
            let mut residue = None;
            // Reverse checkpoint traversal lets each quotient digit reuse the previous modular
            // residue instead of recomputing a full modular exponentiation for every bucket.
            // Class-group bucket multiplication is commutative.
            for i in (first..last).rev() {
                let block_number = i
                    .checked_mul(l)
                    .and_then(|base| base.checked_add(j))
                    .ok_or(Error::InvalidProofParameters)?;
                if let Some(end) = block_number
                    .checked_add(1)
                    .and_then(|next| k.checked_mul(next))
                    && end <= num_iterations
                {
                    let r = match residue.take() {
                        Some(previous) => (previous * &stride_mod) % &b,
                        None => fast_pow_u64_mod(2, num_iterations - end, &b)?,
                    };
                    let block = crate::discriminant::u64_low_word(&((&r << k as usize) / &b));
                    residue = Some(r);
                    let index =
                        usize::try_from(block).map_err(|_| Error::InvalidProofParameters)?;
                    if index >= ys.len() {
                        return Err(Error::InvalidProofParameters);
                    }
                    let checkpoint = crate::form::LimbForm::from_form(
                        intermediates
                            .get(i as usize)
                            .ok_or(Error::InvalidProofParameters)?,
                    );
                    ys[index] = Some(match ys[index] {
                        Some(old) => context.multiply(old, checkpoint),
                        None => checkpoint,
                    });
                }
            }

            for b1 in 1..bucket1_count {
                let mut z = context.identity();
                for b0 in 0..bucket0_count {
                    if let Some(bucket) = ys[b1 * bucket0_count + b0] {
                        z = context.multiply(z, bucket);
                    }
                }
                x = context.multiply(x, context.pow(z, (b1 as u64) * (1u64 << k0)));
            }
            for b0 in 1..bucket0_count {
                let mut z = context.identity();
                for b1 in 0..bucket1_count {
                    if let Some(bucket) = ys[b1 * bucket0_count + b0] {
                        z = context.multiply(z, bucket);
                    }
                }
                x = context.multiply(x, context.pow(z, b0 as u64));
            }
        }
        Ok(x)
    };
    let count = threads.min(usize::try_from(l).unwrap_or(usize::MAX)).max(1);
    let chunk = l.div_ceil(count as u64);
    let parts = if threads > l as usize {
        let columns = (threads as u64 / l).min(checkpoints).max(1);
        let width = checkpoints.div_ceil(columns);
        let mut jobs = Vec::new();
        for row in (0..l).rev() {
            for column in 0..columns {
                let first = column * width;
                let last = (first + width).min(checkpoints);
                if first < last {
                    jobs.push((row, first, last));
                }
            }
        }
        let pool = pool.ok_or(Error::InvalidProofParameters)?;
        let completed: Result<Vec<_>> = pool.install(|| {
            jobs.into_par_iter()
                .map(|(row, first, last)| Ok((row, range(row, row + 1, first, last)?)))
                .collect()
        });
        let mut rows = vec![context.identity(); l as usize];
        for (row, part) in completed? {
            rows[row as usize] = context.multiply(rows[row as usize], part);
        }
        rows.into_iter().rev().map(|row| (1, row)).collect()
    } else if count == 1 {
        vec![(l, range(0, l, 0, checkpoints)?)]
    } else {
        let mut jobs = Vec::new();
        for thread in 0..count {
            let end = l.saturating_sub(thread as u64 * chunk);
            let start = end.saturating_sub(chunk);
            if start < end {
                jobs.push((start, end));
            }
        }
        let pool = pool.ok_or(Error::InvalidProofParameters)?;
        let completed: Result<Vec<_>> = pool.install(|| {
            jobs.into_par_iter()
                .map(|(start, end)| Ok((start, end, range(start, end, 0, checkpoints)?)))
                .collect()
        });
        let mut completed = completed?;
        completed.sort_unstable_by_key(|(_, end, _)| std::cmp::Reverse(*end));
        completed
            .into_iter()
            .map(|(start, end, part)| (end - start, part))
            .collect()
    };
    let mut x = context.identity();
    for (length, part) in parts {
        for _ in 0..k.checked_mul(length).ok_or(Error::InvalidProofParameters)? {
            x = context.square(x);
        }
        x = context.multiply(x, part);
    }

    let mut x = x.into_form();
    x.reduce();
    Ok(x)
}

fn approximate_parameters(t: u64) -> Result<(u64, u64)> {
    if t == 0 {
        return Ok((1, 1));
    }

    let log_memory = 23.25349666_f64;
    let log_t = (t as f64).log2();
    let l = if log_t - log_memory > 0.000001 {
        2_f64.powf(log_memory - 20.0).ceil() as u64
    } else {
        1
    };
    let intermediate = (t as f64) * std::f64::consts::LN_2 / (2.0 * l as f64);
    let k = if intermediate <= 1.0 {
        1
    } else {
        (intermediate.ln() - intermediate.ln().ln() + 0.25)
            .round()
            .max(1.0) as u64
    };

    if l == 0 || k == 0 || k > 20 {
        return Err(Error::InvalidProofParameters);
    }
    Ok((l, k))
}

#[cfg(test)]
fn verify_memo_len() -> usize {
    verify_memo().len()
}

#[cfg(test)]
#[path = "../tests/unit/proof/tests.rs"]
mod tests;
