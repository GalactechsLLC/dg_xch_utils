use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dg_xch_core::{
    blockchain::sized_bytes::Bytes32,
    signatures::{
        AuthorizationMode, AuthorizationRules, AuthorizationVerifier, AuthorizationWitness,
        SignatureOccurrence, SignatureRequirement, SignatureSigner,
        extension::{AuthorizationExtension, FalconAuthorization},
        falcon::{FalconDirect, FalconSigner},
        falcon_block::{FalconHeaderAuthorization, public_key_id},
    },
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::{hint::black_box, time::Duration};

fn headers(c: &mut Criterion) {
    let genesis = Bytes32::const_new([73; 32]);
    let plot = FalconSigner::generate().unwrap();
    let pool = FalconSigner::generate().unwrap();
    let mut group = c.benchmark_group("falcon_headers");
    group
        .sample_size(30)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    for count in [4usize, 5] {
        let mut auth = FalconAuthorization {
            labrador: None,
            genesis,
            header: (0..count)
                .map(|i| {
                    FalconHeaderAuthorization::sign(
                        if i + 1 == count { &pool } else { &plot },
                        genesis,
                        &vec![i as u8; if i + 1 == count { 36 } else { 32 }],
                    )
                    .unwrap()
                })
                .collect(),
            transactions: vec![],
        };
        auth.canonicalize();
        let requirements: Vec<_> = auth
            .header
            .iter()
            .map(|p| {
                (
                    public_key_id(&p.public_key),
                    p.message.clone(),
                    p.reference(genesis),
                )
            })
            .collect();
        let extension = AuthorizationExtension::from(auth.clone());
        let compact_bytes = extension
            .to_bytes(ChiaProtocolVersion::default())
            .unwrap()
            .len();
        let legacy_bytes = 49
            + auth
                .header
                .iter()
                .map(|p| 897 + 2 + p.message.len() + 666)
                .sum::<usize>();
        println!(
            "falcon_header_bytes count={count} keys=2 legacy={legacy_bytes} compact={compact_bytes}"
        );
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(BenchmarkId::new("repeated_crypto", count), |b| {
            b.iter(|| {
                black_box(&auth).verify(genesis).unwrap();
                for (key, message, reference) in &requirements {
                    assert!(auth.verify_header(key, message, reference));
                }
            })
        });
        group.bench_function(BenchmarkId::new("verified_context", count), |b| {
            b.iter(|| {
                let verified = black_box(&auth).verified(genesis).unwrap();
                for (key, message, reference) in &requirements {
                    assert!(verified.verify_header(key, message, reference));
                }
            })
        });
    }
    group.finish();
}

fn transactions(c: &mut Criterion) {
    let signer = FalconSigner::generate().unwrap();
    let key = signer.public_key();
    let rules = AuthorizationRules::for_mode(AuthorizationMode::FalconDirect).unwrap();
    let mut group = c.benchmark_group("falcon_transactions");
    group
        .sample_size(30)
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    // Same-key distinct execution occurrences. Setup/signing is outside verification timing.
    for count in [1usize, 16, 128, 1024] {
        let requirements: Vec<_> = (0..count)
            .map(|i| SignatureRequirement::Falcon {
                occurrence: SignatureOccurrence {
                    coin_id: [91; 32].into(),
                    condition_index: i as u32,
                },
                public_key: Box::new(key),
                message: vec![i as u8; 96],
            })
            .collect();
        let signatures: Vec<_> = requirements
            .iter()
            .map(|r| {
                let SignatureRequirement::Falcon { message, .. } = r else {
                    unreachable!()
                };
                signer.sign_message(message).unwrap()
            })
            .collect();
        println!(
            "falcon_transaction_bytes count={count} direct_signatures={}",
            signatures.len() * 666
        );
        group.throughput(Throughput::Elements(count as u64));
        group.bench_function(BenchmarkId::new("direct_verify", count), |b| {
            b.iter(|| {
                FalconDirect
                    .verify(
                        &rules,
                        black_box(&requirements),
                        &AuthorizationWitness::FalconDirect(black_box(&signatures)),
                    )
                    .unwrap();
            })
        });
    }
    group.finish();
    c.bench_function("falcon_sign_96_bytes", |b| {
        b.iter(|| {
            black_box(signer.sign_message(black_box(&[42; 96])).unwrap());
        })
    });
}

criterion_group!(benches, headers, transactions);
criterion_main!(benches);
