use criterion::{Criterion, criterion_group, criterion_main};
use dg_xch_vdf::discriminant::create_discriminant_bytes;
use dg_xch_vdf::form::{Form, fast_pow_form_pair_with, fast_pow_form_with, nucomp_bound};
use num_bigint::{BigInt, Sign};
use std::hint::black_box;

fn exponentiation(criterion: &mut Criterion) {
    let bytes = create_discriminant_bytes(b"verification-benchmark", 1024).unwrap();
    let discriminant = BigInt::from_bytes_be(Sign::Minus, &bytes);
    let bound = nucomp_bound(&discriminant);
    let mut base = Form::generator(&discriminant).unwrap();
    for _ in 0..100 {
        base = base.square_with(&discriminant, &bound).unwrap();
    }
    let other = base.square_with(&discriminant, &bound).unwrap();
    let mut exponent_bytes: [u8; 33] =
        std::array::from_fn(|index| (index as u8).wrapping_mul(73).wrapping_add(29));
    exponent_bytes[0] |= 0x80;
    let exponent = BigInt::from_bytes_be(Sign::Plus, &exponent_bytes);
    let other_exponent = &exponent - BigInt::from(62);
    let mut group = criterion.benchmark_group("vdf_uncached_arithmetic");
    group.sample_size(20);
    group.bench_function("single_264_bit", |bench| {
        bench.iter(|| {
            black_box(
                fast_pow_form_with(
                    black_box(&base),
                    &discriminant,
                    &bound,
                    black_box(&exponent),
                )
                .unwrap(),
            )
        });
    });
    group.bench_function("fused_264_bit_pair", |bench| {
        bench.iter(|| {
            black_box(
                fast_pow_form_pair_with(
                    black_box(&base),
                    black_box(&exponent),
                    &other,
                    &other_exponent,
                    &discriminant,
                    &bound,
                )
                .unwrap(),
            )
        });
    });
    group.finish();
}

criterion_group!(benches, exponentiation);
criterion_main!(benches);
