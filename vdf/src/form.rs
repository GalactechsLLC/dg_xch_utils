use crate::discriminant::{bigint_from_le, bigint_to_fixed_le, bit_len, hash_prime, u64_low_word};
use crate::error::{Error, Result};
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{One, Signed, Zero};

pub const MAX_D_BITS: usize = 1024;
pub const FORM_SIZE: usize = MAX_D_BITS.div_ceil(32) * 3 + 4;
pub const B_BITS: usize = 264;
pub const B_BYTES: usize = B_BITS.div_ceil(8);

const BQFC_B_SIGN: u8 = 1;
const BQFC_T_SIGN: u8 = 1 << 1;
const BQFC_IS_1: u8 = 1 << 2;
const BQFC_IS_GEN: u8 = 1 << 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub a: BigInt,
    pub b: BigInt,
    pub c: BigInt,
}

impl Form {
    pub fn from_abd(a: BigInt, b: BigInt, discriminant: &BigInt) -> Result<Self> {
        if a <= BigInt::zero() {
            return Err(Error::InvalidForm);
        }

        let divisor = &a << 2usize;
        let numerator = &b * &b - discriminant;
        if !numerator.is_multiple_of(&divisor) {
            return Err(Error::InvalidForm);
        }

        let mut form = Self {
            a,
            b,
            c: numerator / divisor,
        };
        form.reduce();
        Ok(form)
    }

    pub fn identity(discriminant: &BigInt) -> Result<Self> {
        Self::from_abd(BigInt::one(), BigInt::one(), discriminant)
    }

    pub fn generator(discriminant: &BigInt) -> Result<Self> {
        Self::from_abd(BigInt::from(2u8), BigInt::one(), discriminant)
    }

    pub fn reduce(&mut self) {
        normalize(&mut self.a, &mut self.b, &mut self.c);
        while self.a > self.c || (self.a == self.c && self.b < BigInt::zero()) {
            reduce_impl(&mut self.a, &mut self.b, &mut self.c);
        }
        normalize(&mut self.a, &mut self.b, &mut self.c);
    }

    pub fn is_reduced(&self) -> bool {
        (self.a < self.c || (self.a == self.c && self.b >= BigInt::zero()))
            && (self.a.abs() > self.b.abs() || self.a == self.b)
    }

    pub fn check_valid(&self, discriminant: &BigInt) -> bool {
        &self.b * &self.b - BigInt::from(4u8) * &self.a * &self.c == *discriminant
    }

    /// Squaring is composition of the form with itself, routed through the general composition
    /// [`Form::compose`] rather than a direct doubling formula.
    ///
    /// The classic direct-square formula solves `b·u ≡ c (mod a)`, which has NO solution for a primitive
    /// form with `gcd(a,b) = g > 1` (there `g ∤ c`, since `g | gcd(a,b,c) = 1` would force `g = 1`). The old
    /// code computed `u = ((c / g) · x) mod a` with a TRUNCATING `c / g`, silently producing a wrong `u`
    /// (and then a non-exact `(b·u − c) / a`) → a degenerate output form → `get_b` mismatch → spurious VDF
    /// rejection. It only worked for `gcd(a,b) = 1` (the common case), which is why most forms verified but
    /// occasional ones failed. The general composition absorbs `gcd(a,b)` into the 3-way gcd `w`, so it is
    /// correct for every form; the unique reduced representative makes the result identical to the reference.
    pub fn square(&self) -> Result<Self> {
        self.compose(self)
    }

    pub fn multiply(&self, rhs: &Self) -> Result<Self> {
        self.compose(rhs)
    }

    /// NUCOMP-style composition of two forms of the same discriminant, matching the reference class-group
    /// algorithm (Chia's `inkfish`/`chiavdf`). Correct for the general `gcd(a,b) > 1` case via
    /// `w = gcd(gcd(a₁,a₂), (b₁+b₂)/2)`, which divides out the common factor before solving.
    fn compose(&self, rhs: &Self) -> Result<Self> {
        let two = BigInt::from(2u8);
        let g = (&rhs.b + &self.b) / &two;
        let h = (&rhs.b - &self.b) / &two;
        let w = self.a.gcd(&rhs.a).gcd(&g);

        let j = w.clone();
        let s = &self.a / &w;
        let t = &rhs.a / &w;
        let u = &g / &w;

        let (k_temp, constant_factor) =
            solve_linear_congruence(&(&t * &u), &(&h * &u + &s * &self.c), &(&s * &t))?;
        let (n, _) = solve_linear_congruence(&(&t * &constant_factor), &(&h - &t * &k_temp), &s)?;

        let k = &k_temp + &constant_factor * &n;
        let l = (&t * &k - &h) / &s;
        let m = (&t * &u * &k - &h * &u - &s * &self.c) / (&s * &t);

        let mut result = Self {
            a: &s * &t,
            b: &j * &u - (&k * &t + &l * &s),
            c: &k * &l - &j * &m,
        };
        result.reduce();
        Ok(result)
    }

    pub fn serialize(&self, discriminant_bits: usize) -> Result<[u8; FORM_SIZE]> {
        let mut form = self.clone();
        form.reduce();
        serialize_form(&form.a, &form.b, discriminant_bits)
    }

    pub fn deserialize(discriminant: &BigInt, bytes: &[u8]) -> Result<Self> {
        if bytes.len() != FORM_SIZE {
            return Err(Error::InvalidFormSize);
        }

        let (a, b) = deserialize_ab(discriminant, bytes)?;
        let form = Self::from_abd(a, b, discriminant)?;
        if !form.is_reduced() {
            return Err(Error::FormNotReduced);
        }
        Ok(form)
    }
}

pub fn fast_pow_form(base: &Form, discriminant: &BigInt, exponent: &BigInt) -> Result<Form> {
    if exponent.is_zero() {
        return Form::identity(discriminant);
    }

    let mut result = base.clone();
    for bit in (0..bit_len(exponent).saturating_sub(1)).rev() {
        result = result.square()?;
        if exponent.bit(bit.try_into().expect("bit index fits u64")) {
            result = result.multiply(base)?;
        }
    }
    result.reduce();
    Ok(result)
}

pub fn get_b(discriminant: &BigInt, x: &Form, y: &Form) -> Result<BigInt> {
    let d_bits = bit_len(discriminant);
    let mut bytes = x.serialize(d_bits)?.to_vec();
    bytes.extend_from_slice(&y.serialize(d_bits)?);
    Ok(hash_prime(&bytes, B_BITS, &[B_BITS - 1]))
}

fn normalize(a: &mut BigInt, b: &mut BigInt, c: &mut BigInt) {
    let r = div_floor(&(a.clone() - b.clone()), &(a.clone() << 1usize));
    let next_a = a.clone();
    let next_b = b.clone() + ((a.clone() * &r) << 1usize);
    let next_c = a.clone() * &r * &r + b.clone() * &r + c.clone();
    *a = next_a;
    *b = next_b;
    *c = next_c;
}

fn reduce_impl(a: &mut BigInt, b: &mut BigInt, c: &mut BigInt) {
    let s = div_floor(&(c.clone() + b.clone()), &(c.clone() << 1usize));
    let next_a = c.clone();
    let next_b = ((c.clone() * &s) << 1usize) - b.clone();
    let next_c = c.clone() * &s * &s - b.clone() * &s + a.clone();
    *a = next_a;
    *b = next_b;
    *c = next_c;
}

fn solve_linear_congruence(a: &BigInt, b: &BigInt, m: &BigInt) -> Result<(BigInt, BigInt)> {
    let egcd = a.extended_gcd(m);
    if !b.is_multiple_of(&egcd.gcd) {
        return Err(Error::InvalidForm);
    }
    let q = b / &egcd.gcd;
    Ok((positive_mod(&(q * egcd.x), m), m / egcd.gcd))
}

fn div_floor(a: &BigInt, b: &BigInt) -> BigInt {
    a.div_floor(b)
}

fn positive_mod(a: &BigInt, modulus: &BigInt) -> BigInt {
    a.mod_floor(&modulus.abs())
}

fn deserialize_ab(discriminant: &BigInt, bytes: &[u8]) -> Result<(BigInt, BigInt)> {
    if bytes[0] & (BQFC_IS_1 | BQFC_IS_GEN) != 0 {
        let a = if bytes[0] & BQFC_IS_GEN != 0 {
            BigInt::from(2u8)
        } else {
            BigInt::one()
        };
        return Ok((a, BigInt::one()));
    }

    let d_bits = rounded_discriminant_bits(discriminant)?;
    let g_size = usize::from(bytes[1]);
    if g_size >= d_bits / 32 {
        return Err(Error::InvalidCompressedForm);
    }

    let mut offset = 2usize;
    let a_size = d_bits / 16 - g_size;
    let mut a = bigint_from_le(&bytes[offset..offset + a_size]);
    offset += a_size;

    let t_size = d_bits / 32 - g_size;
    let mut t = bigint_from_le(&bytes[offset..offset + t_size]);
    offset += t_size;

    let g_bytes = g_size + 1;
    let g = bigint_from_le(&bytes[offset..offset + g_bytes]);
    offset += g_bytes;
    let b0 = bigint_from_le(&bytes[offset..offset + g_bytes]);

    if bytes[0] & BQFC_T_SIGN != 0 {
        t = -t;
    }

    let b_sign = bytes[0] & BQFC_B_SIGN != 0;
    let (out_a, out_b) = decompress_ab(discriminant, &mut a, &t, &g, &b0, b_sign)?;
    if serialize_form(&out_a, &out_b, bit_len(discriminant))? != bytes {
        return Err(Error::InvalidCompressedForm);
    }
    Ok((out_a, out_b))
}

fn decompress_ab(
    discriminant: &BigInt,
    a: &mut BigInt,
    c_t: &BigInt,
    g: &BigInt,
    b0: &BigInt,
    b_sign: bool,
) -> Result<(BigInt, BigInt)> {
    if c_t.is_zero() {
        return Ok((a.clone(), a.clone()));
    }

    let t = if c_t.is_negative() {
        c_t + a.clone()
    } else {
        c_t.clone()
    };
    if a.is_zero() {
        return Err(Error::InvalidCompressedForm);
    }

    let egcd = t.extended_gcd(a);
    if egcd.gcd != BigInt::one() {
        return Err(Error::InvalidCompressedForm);
    }
    let t_inv = positive_mod(&egcd.x, a);

    let d = discriminant.mod_floor(a);
    let tmp = positive_mod(&(c_t * c_t * d), a);
    let sqrt = sqrt_bigint(&tmp);
    if &sqrt * &sqrt != tmp {
        return Err(Error::InvalidCompressedForm);
    }

    let mut out_b = (&sqrt * t_inv).mod_floor(a);
    let out_a = if *g > BigInt::one() {
        a.clone() * g
    } else {
        a.clone()
    };
    if b0 > &BigInt::zero() {
        out_b += a.clone() * b0;
    }
    if b_sign {
        out_b = -out_b;
    }
    if out_b.abs() > out_a {
        return Err(Error::InvalidCompressedForm);
    }

    Ok((out_a, out_b))
}

fn serialize_form(a: &BigInt, b: &BigInt, d_bits: usize) -> Result<[u8; FORM_SIZE]> {
    let mut out = [0u8; FORM_SIZE];

    if *b == BigInt::one() && *a <= BigInt::from(2u8) {
        out[0] = if *a == BigInt::from(2u8) {
            BQFC_IS_GEN
        } else {
            BQFC_IS_1
        };
        return Ok(out);
    }

    let compressed = compress_ab(a, b);
    serialize_compressed(&mut out, &compressed, d_bits)?;
    Ok(out)
}

struct CompressedForm {
    a: BigInt,
    t: BigInt,
    g: BigInt,
    b0: BigInt,
    b_sign: bool,
}

fn compress_ab(a: &BigInt, b: &BigInt) -> CompressedForm {
    if a == b {
        return CompressedForm {
            a: a.clone(),
            t: BigInt::zero(),
            g: BigInt::zero(),
            b0: BigInt::zero(),
            b_sign: false,
        };
    }

    let b_sign = b.is_negative();
    let a_sqrt = sqrt_bigint(a);
    let mut a_copy = a.clone();
    let mut b_copy = b.abs();
    let mut t = xgcd_partial_co1(&mut a_copy, &mut b_copy, &a_sqrt);
    t = -t;

    let g = a.gcd(&t);
    if g == BigInt::one() {
        CompressedForm {
            a: a.clone(),
            t,
            g,
            b0: BigInt::zero(),
            b_sign,
        }
    } else {
        let reduced_a = a / &g;
        let reduced_t = &t / &g;
        let mut b0 = b / &reduced_a;
        if b_sign {
            b0 = -b0;
        }
        CompressedForm {
            a: reduced_a,
            t: reduced_t,
            g,
            b0,
            b_sign,
        }
    }
}

fn serialize_compressed(
    out: &mut [u8; FORM_SIZE],
    form: &CompressedForm,
    d_bits: usize,
) -> Result<()> {
    if d_bits == 0 || d_bits > MAX_D_BITS {
        return Err(Error::InvalidDiscriminant);
    }

    let d_bits = (d_bits + 31) & !31usize;
    if d_bits > MAX_D_BITS {
        return Err(Error::InvalidDiscriminant);
    }

    out[0] = u8::from(form.b_sign);
    if form.t.is_negative() {
        out[0] |= BQFC_T_SIGN;
    }

    let g_size = bit_len(&form.g).div_ceil(8).saturating_sub(1);
    if g_size > u8::MAX.into() {
        return Err(Error::InvalidCompressedForm);
    }
    out[1] = g_size as u8;
    let mut offset = 2usize;

    export_field(out, &mut offset, d_bits / 16 - g_size, &form.a)?;
    export_field(out, &mut offset, d_bits / 32 - g_size, &form.t.abs())?;
    export_field(out, &mut offset, g_size + 1, &form.g)?;
    export_field(out, &mut offset, g_size + 1, &form.b0)?;
    Ok(())
}

fn export_field(
    out: &mut [u8; FORM_SIZE],
    offset: &mut usize,
    size: usize,
    value: &BigInt,
) -> Result<()> {
    let bytes = bigint_to_fixed_le(value, size).ok_or(Error::InvalidCompressedForm)?;
    out[*offset..*offset + size].copy_from_slice(&bytes);
    *offset += size;
    Ok(())
}

fn rounded_discriminant_bits(discriminant: &BigInt) -> Result<usize> {
    let d_bits = bit_len(discriminant);
    if d_bits == 0 || d_bits > MAX_D_BITS {
        return Err(Error::InvalidDiscriminant);
    }
    Ok((d_bits + 31) & !31usize)
}

/// The bit length of a non-negative value, matching chiavdf's `mpz_sizeinbase(x, 2)` (returns 1 for 0).
fn xgcd_bitlen(x: &BigInt) -> i64 {
    let b = bit_len(x);
    if b == 0 { 1 } else { b as i64 }
}

/// The low 64-bit word of `(x >> shift)`, interpreted as signed — chiavdf's
/// `chiavdf_mpz_extract_uword_from_shift_nonneg`. `x` is non-negative here; the shift is chosen so the
/// extracted word is ~63 bits, hence non-negative.
fn xgcd_extract_word(x: &BigInt, shift: i64) -> i128 {
    let w = if shift > 0 {
        u64_low_word(&(x >> (shift as usize)))
    } else {
        u64_low_word(x)
    };
    i64::from_ne_bytes(w.to_ne_bytes()) as i128
}

/// A faithful port of chiavdf's `mpz_xgcd_partial` (Lehmer partial extended GCD, `xgcd_partial.c`). The
/// returned cofactor `co1` is the canonical value chiavdf's `bqfc_compress` uses for `t`; a pure-slow
/// Euclidean version diverges (it stops one step short of chiavdf's word-batched fast path), which is
/// exactly the bqfc serialization mismatch that broke `serialize_form` round-trips and `get_b` hashes.
/// The Lehmer word arithmetic uses `i128` (panic-safe); chiavdf's `i64` never overflows on valid inputs,
/// so the results are identical there.
fn xgcd_partial_co1(r2: &mut BigInt, r1: &mut BigInt, limit: &BigInt) -> BigInt {
    let mut co2 = BigInt::zero();
    let mut co1 = -BigInt::one();

    while !r1.is_zero() && &*r1 > limit {
        let bits = (xgcd_bitlen(r2).max(xgcd_bitlen(r1)) - 64 + 1).max(0);
        let mut rr2 = xgcd_extract_word(r2, bits);
        let mut rr1 = xgcd_extract_word(r1, bits);
        let bb = xgcd_extract_word(limit, bits);

        let (mut aa2, mut aa1, mut bb2, mut bb1): (i128, i128, i128, i128) = (0, 1, 1, 0);
        let mut i: i64 = 0;
        while rr1 != 0 && rr1 > bb {
            let qq = rr2 / rr1; // trunc toward zero, matching C integer division
            let t1 = rr2 - qq * rr1;
            let t2 = aa2 - qq * aa1;
            let t3 = bb2 - qq * bb1;
            if i & 1 != 0 {
                if t1 < -t3 || rr1 - t1 < t2 - aa1 {
                    break;
                }
            } else if t1 < -t2 || rr1 - t1 < t3 - bb1 {
                break;
            }
            rr2 = rr1;
            rr1 = t1;
            aa2 = aa1;
            aa1 = t2;
            bb2 = bb1;
            bb1 = t3;
            i += 1;
        }

        if i == 0 {
            // Single exact big-integer Euclidean step (chiavdf's slow branch).
            let q = &*r2 / &*r1;
            let rem = &*r2 - &q * &*r1;
            *r2 = r1.clone();
            *r1 = rem;
            let next = &co2 - &q * &co1;
            co2 = co1;
            co1 = next;
        } else {
            // Apply the accumulated word-level transform to the big integers, from OLD (r2, r1)/(co2, co1).
            let new_r2 = &*r2 * bb2 + &*r1 * aa2;
            let new_r1 = &*r1 * aa1 + &*r2 * bb1;
            *r2 = new_r2;
            *r1 = new_r1;
            let new_co2 = &co2 * bb2 + &co1 * aa2;
            let new_co1 = &co1 * aa1 + &co2 * bb1;
            co2 = new_co2;
            co1 = new_co1;
            if r1.is_negative() {
                co1 = -co1;
                *r1 = -&*r1;
            }
            if r2.is_negative() {
                co2 = -co2;
                *r2 = -&*r2;
            }
        }
    }

    if r2.is_negative() {
        co1 = -co1;
    }
    co1
}

fn sqrt_bigint(value: &BigInt) -> BigInt {
    if value <= &BigInt::zero() {
        return BigInt::zero();
    }

    let n = value.to_biguint().expect("positive value");
    let mut low = BigInt::one();
    let mut high = BigInt::one() << bit_len(value).div_ceil(2);
    let one = BigInt::one();
    while low <= high {
        let mid = (&low + &high) >> 1usize;
        let squared = &mid * &mid;
        if squared <= BigInt::from_biguint(num_bigint::Sign::Plus, n.clone()) {
            low = &mid + &one;
        } else {
            high = mid - &one;
        }
    }
    high
}

pub(crate) fn fast_pow_u64_mod(base: u64, exponent: u64, modulus: &BigInt) -> Result<BigInt> {
    if modulus.is_zero() {
        return Err(Error::InvalidProofParameters);
    }
    let m = modulus
        .abs()
        .to_biguint()
        .ok_or(Error::InvalidProofParameters)?;
    let b = num_bigint::BigUint::from(base);
    let e = num_bigint::BigUint::from(exponent);
    Ok(BigInt::from_biguint(
        num_bigint::Sign::Plus,
        b.modpow(&e, &m),
    ))
}

pub(crate) fn get_block(i: u64, k: u64, t: u64, b: &BigInt) -> Result<u64> {
    let mut res = fast_pow_u64_mod(2, t - k * (i + 1), b)?;
    res <<= k as usize;
    res /= b;
    Ok(u64_low_word(&res))
}
