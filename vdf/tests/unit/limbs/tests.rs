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

fn sw_from_limbs(limbs: &[u64]) -> Sw<8> {
    let mut v = BigInt::from(0u8);
    for l in limbs.iter().rev() {
        v = (v << 64) + l;
    }
    Sw::<8>::from_bigint(&v)
}

#[test]
fn divrem_mag_matches_bigint_division() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut cases: Vec<(Vec<u64>, Vec<u64>)> = vec![
        // qhat = B-1 overestimate / add-back territory
        (vec![0, 0, 1 << 63, u64::MAX - 1], vec![1, 1 << 63]),
        (vec![0, u64::MAX, u64::MAX], vec![u64::MAX, 1 << 63]),
        // all-ones dividend, minimal normalized divisor
        (vec![u64::MAX; 6], vec![0, 1 << 63]),
        (vec![u64::MAX; 6], vec![1 << 63]),
        // single-word divisors incl. 1 and MAX
        (vec![u64::MAX, u64::MAX, u64::MAX], vec![1]),
        (vec![123, 456, 789], vec![u64::MAX]),
        // dividend < divisor
        (vec![7], vec![0, 1]),
        // exact multiples
        (vec![0, 0, 0, 1 << 63], vec![0, 1 << 63]),
    ];
    for _ in 0..4000 {
        let dl = 1 + (rng.next() as usize) % 6;
        let vl = 1 + (rng.next() as usize) % dl.clamp(1, 4);
        let mut d: Vec<u64> = (0..dl).map(|_| rng.next()).collect();
        let mut v: Vec<u64> = (0..vl).map(|_| rng.next()).collect();
        // Bias toward carry-heavy limbs.
        if rng.next().is_multiple_of(3) {
            for x in &mut d {
                *x |= 0xFFFF_FFFF_0000_0000;
            }
        }
        if rng.next().is_multiple_of(3) {
            for x in &mut v {
                *x |= 0xFFFF_FFFF_FFFF_0000;
            }
        }
        if v.iter().all(|&x| x == 0) {
            v[0] = 1;
        }
        cases.push((d, v));
    }
    for (dl, vl) in cases {
        let a = sw_from_limbs(&dl);
        let b = sw_from_limbs(&vl);
        if b.len == 0 {
            continue;
        }
        let (q, r) = a.divrem_mag(&b);
        let (ab, bb) = (a.to_bigint(), b.to_bigint());
        assert_eq!(
            q.to_bigint(),
            &ab / &bb,
            "quotient diverged for {dl:x?} / {vl:x?}"
        );
        assert_eq!(
            r.to_bigint(),
            &ab % &bb,
            "remainder diverged for {dl:x?} / {vl:x?}"
        );
    }
}
