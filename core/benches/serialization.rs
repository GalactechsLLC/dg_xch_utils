use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dg_xch_core::protocols::full_node::RespondBlocks;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::hint::black_box;

fn serialization(criterion: &mut Criterion) {
    let wire = include_bytes!("../tests/fixtures/respond_blocks_mainnet_9138873_9138904.bin");
    let version = ChiaProtocolVersion::default();
    let response = RespondBlocks::from_bytes_exact(wire, version).unwrap();
    let mut group = criterion.benchmark_group("block_serialization");
    group.throughput(Throughput::Bytes(wire.len() as u64));
    group.bench_function("fresh_buffer", |bench| {
        bench.iter(|| black_box(response.to_bytes(version).unwrap()));
    });
    let mut bytes = Vec::with_capacity(wire.len());
    group.bench_function("reused_buffer", |bench| {
        bench.iter(|| {
            bytes.clear();
            response.append_bytes(&mut bytes, version).unwrap();
            black_box(&bytes);
        });
    });
    group.finish();
}

criterion_group!(benches, serialization);
criterion_main!(benches);
