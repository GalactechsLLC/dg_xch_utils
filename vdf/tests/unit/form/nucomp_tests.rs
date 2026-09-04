use super::*;
use crate::discriminant::create_discriminant_int;

// lehmer_gcdinv must agree with the schoolbook extended GCD: g = gcd(b, a) and v·b ≡ g (mod a),
// 0 ≤ v < a, across a walk of real-size operands.
#[test]
fn lehmer_gcdinv_matches_extended_gcd() {
    let d = create_discriminant_int(b"lehmer-gcdinv-seed", 1024).expect("discriminant");
    let g = Form::generator(&d).expect("generator");
    let mut x = g.clone();
    for _ in 0..60 {
        x = x.square().expect("square");
        let a = x.a.clone();
        let b = x.b.mod_floor(&a);
        if b.is_zero() {
            continue;
        }
        let (got_g, got_v) = lehmer_gcdinv(&b, &a);
        let e = b.extended_gcd(&a);
        assert_eq!(got_g, e.gcd, "gcd mismatch");
        assert!(
            (&got_v * &b - &got_g).mod_floor(&a).is_zero(),
            "cofactor congruence v*b ≡ g (mod a) violated"
        );
        assert!(!got_v.is_negative() && got_v < a, "cofactor out of range");
    }
}

#[test]
fn nucomp_matches_reference_composition_walk() {
    let d = create_discriminant_int(b"nucomp-differential-seed", 1024).expect("discriminant");
    let g = Form::generator(&d).expect("generator");
    let mut x = g.clone();
    let mut y = g.square().expect("square");
    for step in 0..200 {
        let via_nucomp = x.multiply(&y).expect("nucomp multiply");
        let mut via_reference = x.compose_reference(&y).expect("reference multiply");
        via_reference.reduce();
        assert_eq!(
            (&via_nucomp.a, &via_nucomp.b, &via_nucomp.c),
            (&via_reference.a, &via_reference.b, &via_reference.c),
            "NUCOMP diverged from the reference composition at step {step}"
        );
        x = y;
        y = if step % 3 == 0 {
            via_nucomp.square().expect("square")
        } else {
            via_nucomp
        };
    }
}

// The fused Straus/Shamir pair exponentiation must agree with the two single-exponent
// windowed chains composed — byte-for-byte on the serialized reduced form — across the
// exponent-size ladder: the zero/identity degenerations, the ≤64-bit plain
// square-and-multiply fallback (both sides and mixed), the 64/65-bit route boundary, and
// full 264-bit Wesolowski-shaped pairs (top bit set, as hash_prime forces). Bases walk off
// the generator so operands look like mid-verification forms.
#[test]
fn fused_pair_pow_matches_composed_single_pows() {
    use num_traits::One;
    let d = create_discriminant_int(b"straus-pair-differential-seed", 1024).expect("discriminant");
    let l = nucomp_bound(&d);
    let g = Form::generator(&d).expect("generator");
    let mut x = g.clone();
    for _ in 0..40 {
        x = x.square().expect("square");
    }
    let mut y = x.square().expect("square").multiply(&g).expect("multiply");

    // Deterministic exponent ladder: bit sizes across every route boundary.
    let sizes = [1usize, 2, 17, 63, 64, 65, 100, 200, 263, 264];
    let mut exps: Vec<BigInt> = vec![BigInt::zero(), BigInt::one()];
    for (i, bits) in sizes.iter().enumerate() {
        // Top bit set (hash_prime forces bit 263 on real b), pseudo-random lower bits.
        let mut e = BigInt::one() << (bits - 1);
        let mut seed = 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(i as u64 + 1);
        for bit in 0..bits - 1 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            if seed >> 63 == 1 {
                e |= BigInt::one() << bit;
            }
        }
        exps.push(e);
    }

    let d_bits = bit_len(&d);
    for (i, xe) in exps.iter().enumerate() {
        for (j, ye) in exps.iter().enumerate() {
            let fused =
                fast_pow_form_pair_with(&x, xe, &y, ye, &d, &l).expect("fused pair exponentiation");
            let fx = fast_pow_form_with(&x, &d, &l, xe).expect("single pow x");
            let fy = fast_pow_form_with(&y, &d, &l, ye).expect("single pow y");
            let composed = fx.multiply_with(&fy, &d, &l).expect("compose");
            assert_eq!(
                fused.serialize(d_bits).expect("serialize fused"),
                composed.serialize(d_bits).expect("serialize composed"),
                "fused pair diverged at exponent pair ({i}, {j})"
            );
            assert_eq!(
                (&fused.a, &fused.b, &fused.c),
                (&composed.a, &composed.b, &composed.c),
                "reduced representatives diverged at exponent pair ({i}, {j})"
            );
        }
        // Walk the bases so successive rows exercise fresh operands.
        x = x.multiply(&y).expect("walk x");
        y = y.square().expect("walk y");
    }
}
