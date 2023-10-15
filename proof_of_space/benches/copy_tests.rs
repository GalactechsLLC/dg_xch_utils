// #![feature(portable_simd)]
// use criterion::{black_box, Criterion};
// use dg_xch_pos::utils::radix_sort::{ONES, RADIX, ZEROS};
// use std::simd::{u64x16, u64x32, u64x64, Simd};
// pub const SIMD_512_ZEROS: u64x64 = u64x64::from_array([0u64; 64]);
// pub const SIMD_512_ONES: u64x64 = u64x64::from_array([1u64; 64]);
// pub const SIMD_256_ZEROS: u64x32 = u64x32::from_array([0u64; 32]);
// pub const SIMD_256_ONES: u64x32 = u64x32::from_array([1u64; 32]);
// pub const SIMD_128_ZEROS: u64x16 = u64x16::from_array([0u64; 16]);
// pub const SIMD_128_ONES: u64x16 = u64x16::from_array([1u64; 16]);
//
// fn simd_512_benchmark(c: &mut Criterion) {
//     c.bench_function("SIMD Bench", |b| {
//         b.iter(|| {
//             let mut s_all_ones: [u64x64; 4] = [u64x64::splat(1); 4];
//             for _ in 0..1_000_000 {
//                 black_box({
//                     s_all_ones[0] = SIMD_512_ZEROS;
//                     s_all_ones[1] = SIMD_512_ZEROS;
//                     s_all_ones[2] = SIMD_512_ZEROS;
//                     s_all_ones[3] = SIMD_512_ZEROS;
//                     s_all_ones[0] = SIMD_512_ONES;
//                     s_all_ones[1] = SIMD_512_ONES;
//                     s_all_ones[2] = SIMD_512_ONES;
//                     s_all_ones[3] = SIMD_512_ONES;
//                 });
//             }
//             assert!(s_all_ones.iter().all(|v| v.eq(&SIMD_512_ONES)));
//         })
//     });
// }
//
// fn simd_256_benchmark(c: &mut Criterion) {
//     c.bench_function("SIMD Bench", |b| {
//         b.iter(|| {
//             let mut s_all_ones: [u64x32; 8] = [u64x32::splat(1); 8];
//             for _ in 0..1_000_000 {
//                 s_all_ones[0] = SIMD_256_ZEROS;
//                 s_all_ones[1] = SIMD_256_ZEROS;
//                 s_all_ones[2] = SIMD_256_ZEROS;
//                 s_all_ones[3] = SIMD_256_ZEROS;
//                 s_all_ones[0] = SIMD_256_ONES;
//                 s_all_ones[1] = SIMD_256_ONES;
//                 s_all_ones[2] = SIMD_256_ONES;
//                 s_all_ones[3] = SIMD_256_ONES;
//             }
//             assert!(s_all_ones.iter().all(|v| v.eq(&SIMD_256_ONES)));
//         })
//     });
// }
//
// fn simd_128_benchmark(c: &mut Criterion) {
//     c.bench_function("SIMD Bench", |b| {
//         b.iter(|| {
//             let mut s_all_ones: [u64x16; 16] = [u64x16::splat(1); 16];
//             for _ in 0..1_000_000 {
//                 s_all_ones[0] = SIMD_128_ZEROS;
//                 s_all_ones[1] = SIMD_128_ZEROS;
//                 s_all_ones[2] = SIMD_128_ZEROS;
//                 s_all_ones[3] = SIMD_128_ZEROS;
//                 s_all_ones[0] = SIMD_128_ONES;
//                 s_all_ones[1] = SIMD_128_ONES;
//                 s_all_ones[2] = SIMD_128_ONES;
//                 s_all_ones[3] = SIMD_128_ONES;
//             }
//             assert!(s_all_ones.iter().all(|v| v.eq(&SIMD_128_ONES)));
//         })
//     });
// }
//
// fn fill_benchmark(c: &mut Criterion) {
//     c.bench_function("Fill Bench", |b| {
//         b.iter(|| {
//             let mut all_ones: [u64; RADIX] = [1u64; RADIX];
//             for _ in 0..1_000_000 {
//                 all_ones.fill(0u64);
//                 all_ones.fill(1u64);
//             }
//             assert!(all_ones.iter().all(|v| *v == 1));
//         })
//     });
// }
//
// fn copy_benchmark(c: &mut Criterion) {
//     c.bench_function("Copy Bench", |b| {
//         b.iter(|| {
//             let mut all_ones: [u64; RADIX] = [1u64; RADIX];
//             for _ in 0..1_000_000 {
//                 all_ones.copy_from_slice(ZEROS.as_slice());
//                 all_ones.copy_from_slice(ONES.as_slice());
//             }
//             assert!(all_ones.iter().all(|v| *v == 1));
//         })
//     });
// }
//
// pub fn benches() {
//     let mut criterion = Criterion::default().configure_from_args();
//     copy_benchmark(&mut criterion);
//     fill_benchmark(&mut criterion);
//     simd_512_benchmark(&mut criterion);
//     simd_256_benchmark(&mut criterion);
//     simd_128_benchmark(&mut criterion);
//     criterion.final_summary();
// }
//
// fn main() {
//     benches();
// }
