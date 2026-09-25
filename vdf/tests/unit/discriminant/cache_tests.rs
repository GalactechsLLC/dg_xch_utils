use super::*;

#[test]
fn cached_discriminant_is_byte_identical_to_direct_derivation() {
    for i in 0..3u8 {
        let seed = [0x40 | i; 32];
        let direct = -hash_prime(&seed, 512, &[0, 1, 2, 511]);
        let first = create_discriminant_int(&seed, 512).expect("derivation succeeds");
        let second = create_discriminant_int(&seed, 512).expect("derivation succeeds");
        assert_eq!(direct, first, "miss path must equal direct derivation");
        assert_eq!(first, second, "hit path must equal miss path");
    }
}

/// The cache stays bounded past capacity, and an evicted entry re-derives byte-identically.
#[test]
fn cache_stays_bounded_and_eviction_rederives_identically() {
    let mut first_seed = [0u8; 32];
    first_seed[0] = 0x80;
    let direct = -hash_prime(&first_seed, 512, &[0, 1, 2, 511]);
    assert_eq!(
        create_discriminant_int(&first_seed, 512).expect("derivation succeeds"),
        direct
    );

    for i in 0..(DISCRIMINANT_CACHE_CAPACITY as u32 + 8) {
        let mut seed = [0u8; 32];
        seed[0] = 0x81;
        seed[28..32].copy_from_slice(&i.to_be_bytes());
        create_discriminant_int(&seed, 512).expect("derivation succeeds");
    }

    {
        let cache = discriminant_memo();
        assert!(
            cache.len() <= DISCRIMINANT_CACHE_CAPACITY,
            "cache exceeded its bound: {}",
            cache.len()
        );
    }

    // first_seed has been evicted by now; the re-derivation must match the original.
    assert_eq!(
        create_discriminant_int(&first_seed, 512).expect("derivation succeeds"),
        direct
    );
}

#[test]
fn small_factor_screen_never_changes_the_verdict() {
    let raw = |n: &BigUint| {
        let g = rug::Integer::from_digits(&n.to_bytes_be(), rug::integer::Order::MsfBe);
        g.is_probably_prime(24) != rug::integer::IsPrime::No
    };
    let mut cases: Vec<BigUint> = Vec::new();
    for p in [2u64, 3, 5, 127, 311, 313, 331] {
        cases.push(BigUint::from(p));
    }
    for c in [
        4u64,
        9,
        15,
        121,
        311 * 313,
        97 * 89,
        2 * 3 * 5 * 7 * 11 * 13,
    ] {
        cases.push(BigUint::from(c));
    }
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..300 {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let mut bytes = [0u8; 33];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (x.wrapping_mul(i as u64 + 1) >> 32) as u8;
        }
        let mut n = BigUint::from_bytes_be(&bytes);
        n.set_bit(0, true);
        n.set_bit(263, true);
        cases.push(n);
    }
    for n in cases {
        assert_eq!(
            is_probable_prime(&n),
            raw(&n),
            "screen changed the verdict for {n}"
        );
    }
}
