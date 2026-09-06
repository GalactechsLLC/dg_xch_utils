use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dg_xch_pos::verifier::validate_proof;
use std::hint::black_box;

fn verification(criterion: &mut Criterion) {
    let fixture = include_bytes!("../tests/fixtures/pospace_6281496.bin");
    let size = fixture[0];
    let plot_id: [u8; 32] = fixture[1..33].try_into().unwrap();
    let challenge = &fixture[33..65];
    let proof = &fixture[65..];
    let expected = validate_proof(&plot_id, size, proof, challenge).unwrap();
    let mut group = criterion.benchmark_group("pos_verification");
    group.throughput(Throughput::Elements(1));
    group.bench_function("k41_valid", |bench| {
        bench.iter(|| {
            let quality = validate_proof(
                black_box(&plot_id),
                size,
                black_box(proof),
                black_box(challenge),
            )
            .unwrap();
            assert_eq!(quality, expected);
            black_box(quality);
        });
    });
    let mut invalid = proof.to_vec();
    invalid[0] ^= 0x80;
    group.bench_function("k41_tampered", |bench| {
        bench.iter(|| {
            black_box(validate_proof(
                black_box(&plot_id),
                size,
                black_box(&invalid),
                black_box(challenge),
            ))
        });
    });
    group.finish();
}

criterion_group!(benches, verification);
criterion_main!(benches);
