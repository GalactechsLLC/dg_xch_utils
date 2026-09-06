use super::*;
use num_bigint::BigInt;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[cfg(target_arch = "aarch64")]
fn addmul2_kernel(
    x: &[u64; 34],
    y: &[u64; 34],
    n: usize,
    a1: u64,
    a2: u64,
    out: &mut [u64; 34],
) -> (u64, u64) {
    // SAFETY: fixed 34-limb arrays, n <= 30 in every caller here.
    unsafe { crate::limbs_a72::addmul2_rows(x.as_ptr(), y.as_ptr(), n, a1, a2, out.as_mut_ptr()) }
}

#[cfg(target_arch = "aarch64")]
fn submul2_kernel(
    x: &[u64; 34],
    y: &[u64; 34],
    n: usize,
    a1: u64,
    a2: u64,
    out: &mut [u64; 34],
) -> (u64, u64, u64) {
    // SAFETY: as above.
    unsafe { crate::limbs_a72::submul2_rows(x.as_ptr(), y.as_ptr(), n, a1, a2, out.as_mut_ptr()) }
}

#[cfg(target_arch = "aarch64")]
fn addmul1_kernel(y: &[u64; 34], n: usize, a: u64, out: &mut [u64]) -> u64 {
    // SAFETY: as above.
    unsafe { crate::limbs_a72::addmul1_row(y.as_ptr(), n, a, out.as_mut_ptr()) }
}

fn sw_from_limbs(limbs: &[u64]) -> Sw<34> {
    let mut v = BigInt::from(0u8);
    for l in limbs.iter().rev() {
        v = (v << 64) + l;
    }
    Sw::<34>::from_bigint(&v)
}

#[test]
fn linear2_and_mul_match_bigint() {
    let mut rng = Rng(0xA5A5_5A5A_DEAD_BEEF);
    let mut cases = 0u64;
    for round in 0..6_000u64 {
        let xl = 1 + (rng.next() as usize) % 12;
        let yl = 1 + (rng.next() as usize) % 12;
        let mut xd: Vec<u64> = (0..xl).map(|_| rng.next()).collect();
        let mut yd: Vec<u64> = (0..yl).map(|_| rng.next()).collect();
        // Carry-dense shapes every third round.
        if round % 3 == 0 {
            for v in &mut xd {
                *v |= 0xFFFF_FFFF_FF00_0000;
            }
            for v in &mut yd {
                *v = v.wrapping_mul(0xFF00_0000_0000_0001) | 1;
            }
        }
        let x = sw_from_limbs(&xd);
        let y = sw_from_limbs(&yd);
        let (xb, yb) = (x.to_bigint(), y.to_bigint());

        // linear2 across all four sign quadrants, including max-magnitude coefficients.
        for (w1, w2) in [
            (rng.next() as i64 as i128, rng.next() as i64 as i128),
            (i128::from(i64::MAX), i128::from(i64::MAX)),
            (i128::from(i64::MAX), -i128::from(i64::MAX)),
            (-1, 1),
        ] {
            let got = x.linear2(w1, &y, w2).to_bigint();
            let want = &xb * w1 + &yb * w2;
            assert_eq!(
                got, want,
                "linear2 diverged: x={xd:x?} y={yd:x?} w1={w1} w2={w2}"
            );
            cases += 1;
        }

        let got = x.mul(&y).to_bigint();
        assert_eq!(got, &xb * &yb, "mul diverged: x={xd:x?} y={yd:x?}");
        cases += 1;
    }
    eprintln!("  linear2/mul vs bigint: {cases} cases agreed");
}

// Row-level kernel differential: the assembly against the portable loop it replaces, on
// the exact accumulator discipline (65-bit add carry; P/Q/borrow chains; addmul_1 carry).
#[cfg(target_arch = "aarch64")]
#[test]
fn kernels_match_portable_rows() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let shapes: Vec<Vec<u64>> = vec![
        vec![u64::MAX; 12],
        vec![0; 12],
        vec![u64::MAX, 0, u64::MAX, 0, u64::MAX, 0, u64::MAX, 0],
        (0..12).map(|i| 1u64 << (63 - i * 5)).collect(),
    ];
    let coeffs = [0u64, 1, 2, u64::MAX, u64::MAX - 1, 1 << 63];
    let mut cases = 0u64;
    let mut check = |xd: &[u64], yd: &[u64], a1: u64, a2: u64| {
        let n = xd.len().min(yd.len()).min(30);
        let mut x = [0u64; 34];
        let mut y = [0u64; 34];
        x[..xd.len().min(34)].copy_from_slice(&xd[..xd.len().min(34)]);
        y[..yd.len().min(34)].copy_from_slice(&yd[..yd.len().min(34)]);
        let mut out_k = [0u64; 34];
        let mut out_p = [0u64; 34];
        let k = addmul2_kernel(&x, &y, n, a1, a2, &mut out_k);
        let p = Sw::<34>::addmul2_portable(&x, &y, n, a1, a2, &mut out_p);
        assert_eq!(
            (k, out_k),
            (p, out_p),
            "addmul2 diverged n={n} a1={a1:x} a2={a2:x}"
        );
        let mut out_k = [0u64; 34];
        let mut out_p = [0u64; 34];
        let k = submul2_kernel(&x, &y, n, a1, a2, &mut out_k);
        let p = Sw::<34>::submul2_portable(&x, &y, n, a1, a2, &mut out_p);
        assert_eq!(
            (k, out_k),
            (p, out_p),
            "submul2 diverged n={n} a1={a1:x} a2={a2:x}"
        );
        let mut out_k = [7u64; 34];
        let mut out_p = [7u64; 34];
        let k = addmul1_kernel(&y, n, a1, &mut out_k[..]);
        let p = Sw::<34>::addmul1_portable(&y, n, a1, &mut out_p[..]);
        assert_eq!((k, out_k), (p, out_p), "addmul1 diverged n={n} a={a1:x}");
        cases += 3;
    };
    for xs in &shapes {
        for ys in &shapes {
            for &a1 in &coeffs {
                for &a2 in &coeffs {
                    check(xs, ys, a1, a2);
                }
            }
        }
    }
    for _ in 0..4_000 {
        let n = 1 + (rng.next() as usize) % 20;
        let xd: Vec<u64> = (0..n).map(|_| rng.next()).collect();
        let yd: Vec<u64> = (0..n).map(|_| rng.next()).collect();
        check(&xd, &yd, rng.next(), rng.next());
    }
    eprintln!("  kernel-vs-portable rows: {cases} cases agreed");
}
