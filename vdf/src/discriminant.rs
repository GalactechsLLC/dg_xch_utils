use crate::error::{Error, Result};
use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_traits::{One, Signed, Zero};
use sha2::{Digest, Sha256};

const MAX_DISCRIMINANT_SIZE_BITS: usize = 1024;

/// Bounded challenge→prime memo for the discriminant derivation. Within a sub-slot every VDF on a
/// chain shares that chain's challenge, so the identical ~1024-bit Fiat-Shamir prime search recurs
/// per proof check. Capacity matches the reference node's discriminant lru_cache. `get_b`'s
/// 264-bit hash_prime inputs hash the proof forms themselves and are mostly unique —
/// deliberately NOT cached.
const DISCRIMINANT_CACHE_CAPACITY: usize = 200;

pub(crate) fn discriminant_memo() -> &'static crate::memo::Memo<(Vec<u8>, usize), BigInt> {
    static MEMO: std::sync::OnceLock<crate::memo::Memo<(Vec<u8>, usize), BigInt>> =
        std::sync::OnceLock::new();
    MEMO.get_or_init(|| crate::memo::Memo::new(DISCRIMINANT_CACHE_CAPACITY))
}

pub fn create_discriminant(seed: &[u8], result: &mut [u8]) -> bool {
    match create_discriminant_bytes(seed, result.len() * 8) {
        Ok(discriminant) if discriminant.len() <= result.len() => {
            result.fill(0);
            let offset = result.len() - discriminant.len();
            result[offset..].copy_from_slice(&discriminant);
            true
        }
        _ => false,
    }
}

pub fn create_discriminant_bytes(seed: &[u8], size_bits: usize) -> Result<Vec<u8>> {
    let discriminant = create_discriminant_int(seed, size_bits)?;
    let (_, bytes) = (-discriminant).to_bytes_be();
    Ok(bytes)
}

pub(crate) fn create_discriminant_int(seed: &[u8], size_bits: usize) -> Result<BigInt> {
    if size_bits == 0 || size_bits > MAX_DISCRIMINANT_SIZE_BITS || !size_bits.is_multiple_of(8) {
        return Err(Error::InvalidDiscriminantSize);
    }
    if seed.is_empty() {
        return Err(Error::EmptySeed);
    }

    let prime = discriminant_memo().get_or_init((seed.to_vec(), size_bits), || {
        hash_prime(seed, size_bits, &[0, 1, 2, size_bits - 1])
    });
    Ok(-prime.get().expect("discriminant initialized"))
}

pub(crate) fn hash_prime(seed: &[u8], size_bits: usize, bitmask: &[usize]) -> BigInt {
    let mut sprout = seed.to_vec();

    #[cfg(feature = "hashprime-probe")]
    let mut probe_candidates: u64 = 0;

    let mut blob = Vec::with_capacity(size_bits / 8);
    loop {
        blob.clear();
        while blob.len() * 8 < size_bits {
            increment_big_endian(&mut sprout);
            let hash = Sha256::digest(&sprout);
            let remaining = size_bits / 8 - blob.len();
            blob.extend_from_slice(&hash[..remaining.min(hash.len())]);
        }

        let mut candidate = BigUint::from_bytes_be(&blob);
        for bit in bitmask {
            candidate.set_bit((*bit).try_into().expect("bit index fits u64"), true);
        }
        candidate.set_bit(0, true);

        #[cfg(feature = "hashprime-probe")]
        {
            probe_candidates += 1;
        }

        if is_probable_prime(&candidate) {
            #[cfg(feature = "hashprime-probe")]
            {
                let seed_h = Sha256::digest(seed);
                eprintln!(
                    "HASHPRIME bits={} seedlen={} seedh={:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x} candidates={}",
                    size_bits,
                    seed.len(),
                    seed_h[0],
                    seed_h[1],
                    seed_h[2],
                    seed_h[3],
                    seed_h[4],
                    seed_h[5],
                    seed_h[6],
                    seed_h[7],
                    probe_candidates
                );
            }
            return BigInt::from_biguint(Sign::Plus, candidate);
        }
    }
}

fn increment_big_endian(bytes: &mut [u8]) {
    for byte in bytes.iter_mut().rev() {
        *byte = byte.wrapping_add(1);
        if *byte != 0 {
            break;
        }
    }
}

fn is_probable_prime(n: &BigUint) -> bool {
    // The discriminant search must use the same probable-prime strength for every node.
    if let Some(verdict) = small_factor_verdict(n) {
        return verdict;
    }
    let g = rug::Integer::from_digits(&n.to_bytes_be(), rug::integer::Order::MsfBe);
    g.is_probably_prime(24) != rug::integer::IsPrime::No
}

const SMALL_PRIMES: [u64; 64] = [
    2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163, 167, 173, 179, 181, 191, 193,
    197, 199, 211, 223, 227, 229, 233, 239, 241, 251, 257, 263, 269, 271, 277, 281, 283, 293, 307,
    311,
];

// Pack small primes into products so each chunk can be screened with one remainder operation.
#[derive(Clone, Copy)]
struct PrimeChunk {
    start: usize,
    end: usize,
    shift: u32,
    normalized: u64,
    recip: u64,
}

const CHUNK_COUNT: usize = count_prime_chunks();

const fn count_prime_chunks() -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < SMALL_PRIMES.len() {
        let mut chunk: u64 = 1;
        while i < SMALL_PRIMES.len() {
            match chunk.checked_mul(SMALL_PRIMES[i]) {
                Some(c) => {
                    chunk = c;
                    i += 1;
                }
                None => break,
            }
        }
        count += 1;
    }
    count
}

const PRIME_CHUNKS: [PrimeChunk; CHUNK_COUNT] = build_prime_chunks();

const fn build_prime_chunks() -> [PrimeChunk; CHUNK_COUNT] {
    let mut out = [PrimeChunk {
        start: 0,
        end: 0,
        shift: 0,
        normalized: 0,
        recip: 0,
    }; CHUNK_COUNT];
    let mut ci = 0;
    let mut i = 0;
    while i < SMALL_PRIMES.len() {
        let start = i;
        let mut chunk: u64 = 1;
        while i < SMALL_PRIMES.len() {
            match chunk.checked_mul(SMALL_PRIMES[i]) {
                Some(c) => {
                    chunk = c;
                    i += 1;
                }
                None => break,
            }
        }
        let shift = chunk.leading_zeros();
        let normalized = chunk << shift;
        out[ci] = PrimeChunk {
            start,
            end: i,
            shift,
            normalized,
            recip: crate::limbs::recip_2by1(normalized),
        };
        ci += 1;
    }
    out
}

fn small_factor_verdict(n: &BigUint) -> Option<bool> {
    if *n < BigUint::from(2u8) {
        return Some(false);
    }
    let mut digits = [0u64; 32];
    let mut len = 0;
    for (i, d) in n.iter_u64_digits().enumerate() {
        if i >= digits.len() {
            // Wider than any consensus candidate; let the reference path judge it.
            return None;
        }
        digits[i] = d;
        len = i + 1;
    }
    for c in &PRIME_CHUNKS {
        // Folding the s-shifted digits mod the normalized product yields (n mod chunk) << s:
        // the pre-shift spill seeds the remainder and every step keeps rem < normalized.
        let s = c.shift;
        let mut rem: u64 = if s == 0 {
            0
        } else {
            digits[len - 1] >> (64 - s)
        };
        for i in (0..len).rev() {
            let lo = if i == 0 { 0 } else { digits[i - 1] };
            let cur = if s == 0 {
                digits[i]
            } else {
                (digits[i] << s) | (lo >> (64 - s))
            };
            let (_, r) = crate::limbs::div_2by1(rem, cur, c.normalized, c.recip);
            rem = r;
        }
        let rem = rem >> s;
        for &p in &SMALL_PRIMES[c.start..c.end] {
            if rem.is_multiple_of(p) {
                return Some(*n == BigUint::from(p));
            }
        }
    }
    None
}

/// Baillie–PSW exactly as the reference implements it for `reps <= 24`: the small-prime
/// screen (the shared chunked trial division), a strong Miller–Rabin test to base 2, and a
/// strong Lucas test with Selfridge's Method A parameters. Deterministic — an independent
/// implementation must agree with the reference on every input or one of them has a bug,
/// which is what the differential gates exist to prove.
#[allow(dead_code)]
pub(crate) fn is_probable_prime_native(n: &BigUint) -> bool {
    if let Some(verdict) = small_factor_verdict(n) {
        return verdict;
    }
    // The hot width takes the fixed-limb Montgomery ladder; anything wider falls back to the
    // bigint implementation of the same test. Identical verdicts by differential.
    let mr2 =
        crate::mont::miller_rabin_base2_fixed::<5>(n).unwrap_or_else(|| miller_rabin_base2(n));
    if !mr2 {
        return false;
    }
    // A perfect square passes no Lucas parameter search; the reference rejects it here.
    if is_perfect_square(n) {
        return false;
    }
    strong_lucas_selfridge(n)
}

/// Strong Miller–Rabin to base 2: n-1 = d·2^s with d odd; 2^d ≡ ±1, or 2^(d·2^r) ≡ -1 for
/// some r < s.
pub(crate) fn miller_rabin_base2(n: &BigUint) -> bool {
    let one = BigUint::one();
    let two = BigUint::from(2u8);
    let n_minus_one = n - &one;
    let mut d = n_minus_one.clone();
    let mut s = 0usize;
    while d.is_even() {
        d >>= 1;
        s += 1;
    }
    let mut x = two.modpow(&d, n);
    if x == one || x == n_minus_one {
        return true;
    }
    for _ in 1..s {
        x = x.modpow(&two, n);
        if x == n_minus_one {
            return true;
        }
    }
    false
}

fn is_perfect_square(n: &BigUint) -> bool {
    let root = n.sqrt();
    &root * &root == *n
}

/// Selfridge Method A: the first D in 5, -7, 9, -11, ... with Jacobi(D/n) = -1; P = 1,
/// Q = (1 - D) / 4. The strong test: with n+1 = d·2^s (d odd), U_d ≡ 0 or V_(d·2^r) ≡ 0 for
/// some r < s.
fn strong_lucas_selfridge(n: &BigUint) -> bool {
    use num_bigint::BigInt;
    let n_int = BigInt::from(n.clone());
    // Find D.
    let mut d_abs: u64 = 5;
    let mut sign_pos = true;
    let d: BigInt = loop {
        let candidate = if sign_pos {
            BigInt::from(d_abs)
        } else {
            -BigInt::from(d_abs)
        };
        match jacobi(&candidate, &n_int) {
            0 => {
                // gcd(D, n) > 1: for D < n that means a factor; composite (n > 11 here,
                // past the small-prime screen).
                return false;
            }
            -1 => break candidate,
            _ => {
                d_abs += 2;
                sign_pos = !sign_pos;
            }
        }
    };
    let p = BigInt::from(1u8);
    let q: BigInt = (BigInt::from(1u8) - &d) / 4;

    // n + 1 = d_odd · 2^s
    let n_plus_one: BigInt = &n_int + 1;
    let mut d_odd = n_plus_one.clone();
    let mut s = 0usize;
    while (&d_odd % 2u8) == BigInt::ZERO {
        d_odd /= 2;
        s += 1;
    }

    // Lucas sequences by binary ladder over the bits of d_odd, working mod n.
    let modn = |x: &BigInt| -> BigInt {
        let m = x % &n_int;
        if m < BigInt::ZERO { m + &n_int } else { m }
    };
    // Doubling: U_2k = U_k·V_k;  V_2k = V_k² - 2·Q^k
    // Increment: U_{2k+1} = (P·U_2k + V_2k)/2;  V_{2k+1} = (D·U_2k + P·V_2k)/2  (mod n, halving
    // via the modular inverse of 2 — n is odd, so (x + n·(x&1)) / 2).
    let half = |x: &BigInt| -> BigInt {
        let m = modn(x);
        if (&m % 2u8) == BigInt::ZERO {
            m / 2
        } else {
            (m + &n_int) / 2
        }
    };
    let mut u = BigInt::from(1u8);
    let mut v = p.clone();
    let mut qk = modn(&q);
    let bits = d_odd.bits();
    for i in (0..bits - 1).rev() {
        // double
        u = modn(&(&u * &v));
        v = modn(&(&v * &v - 2 * &qk));
        qk = modn(&(&qk * &qk));
        if d_odd.bit(i) {
            let u_new = half(&(&p * &u + &v));
            let v_new = half(&(&d * &u + &p * &v));
            u = u_new;
            v = v_new;
            qk = modn(&(&qk * &q));
        }
    }
    if u == BigInt::ZERO || v == BigInt::ZERO {
        return true;
    }
    for _ in 1..s {
        v = modn(&(&v * &v - 2 * &qk));
        if v == BigInt::ZERO {
            return true;
        }
        qk = modn(&(&qk * &qk));
    }
    false
}

/// Jacobi symbol (a/n) for odd positive n.
fn jacobi(a: &num_bigint::BigInt, n: &num_bigint::BigInt) -> i32 {
    use num_bigint::BigInt;
    let mut a = a % n;
    if a < BigInt::ZERO {
        a += n;
    }
    let mut n = n.clone();
    let mut result = 1i32;
    while a != BigInt::ZERO {
        while (&a % 2u8) == BigInt::ZERO {
            a /= 2;
            let r = (&n % 8u8).try_into().unwrap_or(0u8);
            if r == 3 || r == 5 {
                result = -result;
            }
        }
        std::mem::swap(&mut a, &mut n);
        if (&a % 4u8) == BigInt::from(3u8) && (&n % 4u8) == BigInt::from(3u8) {
            result = -result;
        }
        a %= &n;
    }
    if n == BigInt::from(1u8) { result } else { 0 }
}

pub(crate) fn bigint_from_be(bytes: &[u8]) -> BigInt {
    BigInt::from_biguint(Sign::Plus, BigUint::from_bytes_be(bytes))
}

pub(crate) fn bigint_to_fixed_le(value: &BigInt, size: usize) -> Option<Vec<u8>> {
    if value.is_negative() {
        return None;
    }
    let (_, mut bytes) = value.to_bytes_le();
    if bytes.len() > size {
        return None;
    }
    bytes.resize(size, 0);
    Some(bytes)
}

pub(crate) fn bigint_from_le(bytes: &[u8]) -> BigInt {
    BigInt::from_biguint(Sign::Plus, BigUint::from_bytes_le(bytes))
}

pub(crate) fn bit_len(value: &BigInt) -> usize {
    if value.is_zero() {
        0
    } else {
        value.abs().to_biguint().expect("abs is nonnegative").bits() as usize
    }
}

pub(crate) fn u64_low_word(value: &BigInt) -> u64 {
    value
        .to_biguint()
        .and_then(|v| v.iter_u64_digits().next())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "../tests/unit/discriminant/cache_tests.rs"]
mod cache_tests;
