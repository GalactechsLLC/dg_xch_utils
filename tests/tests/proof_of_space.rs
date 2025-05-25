// #[tokio::test]
// #[allow(clippy::cast_possible_truncation)]
// pub async fn test_qualities() {
//     use crate::constants::ucdiv_t;
//     use crate::plots::decompressor::DecompressorPool;
//     use crate::plots::disk_plot::DiskPlot;
//     use crate::plots::plot_reader::PlotReader;
//     use crate::verifier::proof_to_bytes;
//     use crate::verifier::validate_proof;
//     use dg_xch_core::plots::PlotFile;
//     use std::thread::available_parallelism;
//     let d_pool = Arc::new(DecompressorPool::new(
//         1,
//         available_parallelism()
//             .map(std::num::NonZero::get)
//             .unwrap_or(4) as u8,
//     ));
//     let compressed_reader = PlotReader::new(
//         DiskPlot::new(Path::new(
//             "/home/luna/plot-k32-c05-2023-06-09-02-25-11d916cf9c847158f76affb30a38ca36f83da452c37f4b4d10a1a0addcfa932b.plot"
//         )).await.unwrap(),
//         Some(d_pool.clone()),
//         Some(d_pool.clone()),
//     )
//     .await
//     .unwrap();
//     let uncompressed_reader = PlotReader::new(
//         DiskPlot::new(Path::new(
//             "/home/luna/plot-k32-2023-03-31-06-24-ad3814ecb6ffcfeae3ec68f41b9922e1484b886c614fef4db405468550812dd4.plot"
//         )).await.unwrap(),
//         Some(d_pool.clone()),
//         Some(d_pool),
//     )
//     .await
//     .unwrap();
//     let k = compressed_reader.plot_file().k();
//     let mut challenge =
//         hex::decode("00000000ff04b8ee9355068689bd558eafe07cc7af47ad1574b074fc34d6913a").unwrap();
//     let f7_size = ucdiv_t(k as usize, 8);
//     for (i, v) in challenge[0..f7_size].iter_mut().enumerate() {
//         *v = (0 >> ((f7_size - i - 1) * 8)) as u8;
//     }
//     let qualities = uncompressed_reader
//         .fetch_qualities_for_challenge(&challenge)
//         .await
//         .unwrap();
//     for (index, quality) in &qualities {
//         let proof = uncompressed_reader
//             .fetch_ordered_proof(*index)
//             .await
//             .unwrap();
//         let v_quality = validate_proof(
//             uncompressed_reader.plot_id().to_sized_bytes(),
//             k,
//             &proof_to_bytes(&proof),
//             &challenge,
//         )
//         .unwrap();
//         assert_eq!(*quality, v_quality);
//     }
//     let qualities2 = compressed_reader
//         .fetch_qualities_for_challenge(&challenge)
//         .await
//         .unwrap();
//     for (index, quality) in &qualities2 {
//         let proof = compressed_reader.fetch_ordered_proof(*index).await.unwrap();
//         let v_quality = validate_proof(
//             compressed_reader.plot_id().to_sized_bytes(),
//             k,
//             &proof_to_bytes(&proof),
//             &challenge,
//         )
//         .unwrap();
//         assert_eq!(*quality, v_quality);
//     }
// }
