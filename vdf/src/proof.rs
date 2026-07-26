use crate::discriminant::{bigint_from_be, bit_len, create_discriminant_int};
use crate::error::{Error, Result};
use crate::form::{B_BYTES, FORM_SIZE, Form, fast_pow_form, fast_pow_u64_mod, get_b, get_block};
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

pub fn verify_vdf(
    challenge: &[u8],
    x_s: &[u8],
    proof: &[u8],
    discriminant_size_bits: usize,
    num_iterations: u64,
    recursion: u64,
) -> bool {
    let Ok(discriminant) = create_discriminant_int(challenge, discriminant_size_bits) else {
        return false;
    };
    check_n_wesolowski(&discriminant, x_s, proof, num_iterations, recursion).is_ok()
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
    mut iterations: u64,
    depth: u64,
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
        x = verify_segment(discriminant, &x, &proof_form, &b, segment_iters)?;

        if segment_iters > iterations {
            return Err(Error::InvalidSegmentIterations);
        }
        iterations -= segment_iters;
    }

    let y = Form::deserialize(discriminant, &proof[..FORM_SIZE])?;
    let witness = Form::deserialize(discriminant, &proof[FORM_SIZE..FORM_SIZE * 2])?;
    verify_wesolowski(discriminant, &x, &y, &witness, iterations)
}

fn verify_segment(
    discriminant: &BigInt,
    x: &Form,
    witness: &Form,
    b: &BigInt,
    iterations: u64,
) -> Result<Form> {
    let r = fast_pow_u64_mod(2, iterations, b)?;
    let f1 = fast_pow_form(witness, discriminant, b)?;
    let f2 = fast_pow_form(x, discriminant, &r)?;
    let y = f1.multiply(&f2)?;
    if get_b(discriminant, x, &y)? == *b {
        Ok(y)
    } else {
        Err(Error::InvalidForm)
    }
}

fn verify_wesolowski(
    discriminant: &BigInt,
    x: &Form,
    y: &Form,
    witness: &Form,
    iterations: u64,
) -> Result<()> {
    let b = get_b(discriminant, x, y)?;
    let r = fast_pow_u64_mod(2, iterations, &b)?;
    let f1 = fast_pow_form(witness, discriminant, &b)?;
    let f2 = fast_pow_form(x, discriminant, &r)?;
    if f1.multiply(&f2)? == *y {
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

    for i in 0..num_iterations {
        if i % kl == 0 {
            intermediates.push(y.clone());
        }
        y = y.square()?;
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

    let mut x = Form::identity(discriminant)?;
    for j in (0..l).rev() {
        x = fast_pow_form(&x, discriminant, &(BigInt::one() << k as usize))?;

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
                ys[block_index] = ys[block_index].multiply(
                    intermediates
                        .get(i as usize)
                        .ok_or(Error::InvalidProofParameters)?,
                )?;
            }
        }

        for b1 in 0..bucket1_count {
            let mut z = Form::identity(discriminant)?;
            for b0 in 0..bucket0_count {
                z = z.multiply(&ys[b1 * bucket0_count + b0])?;
            }
            z = fast_pow_form(&z, discriminant, &BigInt::from((b1 as u64) * (1u64 << k0)))?;
            x = x.multiply(&z)?;
        }

        for b0 in 0..bucket0_count {
            let mut z = Form::identity(discriminant)?;
            for b1 in 0..bucket1_count {
                z = z.multiply(&ys[b1 * bucket0_count + b0])?;
            }
            z = fast_pow_form(&z, discriminant, &BigInt::from(b0 as u64))?;
            x = x.multiply(&z)?;
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
mod tests {
    use super::*;
    use crate::discriminant::create_discriminant_int;
    use crate::form::{B_BYTES, get_b};

    #[test]
    fn recursive_proof_verifies_segment_before_final_witness() {
        let challenge =
            hex::decode("ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb")
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
}
