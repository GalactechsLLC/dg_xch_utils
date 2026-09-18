use crate::discriminant::{bigint_from_be, bit_len, create_discriminant_int};
use crate::error::{Error, Result};
use crate::form::{
    B_BYTES, FORM_SIZE, Form, fast_pow_form_pair_with, fast_pow_form_with, fast_pow_u64_mod, get_b,
    get_block, nucomp_bound,
};
use num_bigint::BigInt;
use num_traits::One;

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
    let d_bits = bit_len(discriminant);
    let mut y = x.clone();
    let (l, k) = approximate_parameters(num_iterations)?;
    let kl = k.checked_mul(l).ok_or(Error::InvalidProofParameters)?;
    let intermediate_count = num_iterations.div_ceil(kl);
    let mut intermediates = Vec::with_capacity(intermediate_count as usize);
    // NUCOMP bound computed once for the whole iterated-squaring run.
    let nl = nucomp_bound(discriminant);

    for i in 0..num_iterations {
        if i.is_multiple_of(kl) {
            intermediates.push(y.clone());
        }
        y = y.square_with(discriminant, &nl)?;
    }

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

        let mut ys = vec![Form::identity(discriminant)?; bucket_count];
        let chunks = num_iterations.div_ceil(k * l);
        for i in 0..chunks {
            if num_iterations >= k * (i * l + j + 1) {
                let block = get_block(i * l + j, k, num_iterations, &b)?;
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
