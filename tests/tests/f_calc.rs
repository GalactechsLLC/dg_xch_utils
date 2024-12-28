// use dg_xch_core::utils::bit_reader::BitReader;
//
// fn setup_test_key() -> [u8; 32] {
//     // Create a test key with known values
//     let mut key = [0u8; 32];
//     for i in 0..32 {
//         key[i] = i as u8;
//     }
//     key
// }
//
// #[test]
// fn test_f1_calculator_new() {
//     let k = 32u8;
//     let key = setup_test_key();
//     let calculator = F1Calculator::new(k, &key);
//
//     assert_eq!(calculator.k, k);
//     // Verify the first byte of the encryption context is properly initialized
//     assert_eq!(calculator.enc_ctx_.input[0], 0);
// }
//
// #[test]
// fn test_f1_calculate_f() {
//     let k = 32u8;
//     let key = setup_test_key();
//     let calculator = F1Calculator::new(k, &key);
//
//     // Test with a simple input
//     let input = BitReader::new(123456789, 64);
//     let result = calculator.calculate_f(&input).unwrap();
//
//     // Verify the output size is k + K_EXTRA_BITS
//     assert_eq!(result.get_size(), (k + K_EXTRA_BITS) as usize);
// }
//
// #[test]
// fn test_f1_calculate_buckets() {
//     let k = 32u8;
//     let key = setup_test_key();
//     let calculator = F1Calculator::new(k, &key);
//
//     let mut results = vec![0u64; 10];
//     calculator.calculate_buckets(0, 10, &mut results);
//
//     // Verify results are non-zero and unique
//     for i in 0..9 {
//         assert_ne!(results[i], 0);
//         assert_ne!(results[i], results[i + 1]);
//     }
// }
//
// #[test]
// fn test_f1_spans_two_blocks() {
//     let k = 32u8;
//     let key = setup_test_key();
//     let calculator = F1Calculator::new(k, &key);
//
//     // Create input that will span two blocks
//     let large_input = BitReader::new(u64::MAX, 64);
//     let result = calculator.calculate_f(&large_input).unwrap();
//
//     assert_eq!(result.get_size(), (k + K_EXTRA_BITS) as usize);
// }
//
// #[test]
// fn test_fx_calculator_new() {
//     let k = 32u8;
//     let table_index = 2u8;
//     let calculator = FXCalculator::new(k, table_index);
//
//     assert_eq!(calculator.k, k);
//     assert_eq!(calculator.table_index, table_index);
//     assert_eq!(calculator.rmap.len(), K_BC);
// }
//
// #[test]
// fn test_fx_calculate_bucket() {
//     let k = 32u8;
//     let calculator = FXCalculator::new(k, 2);
//
//     let y1 = BitReader::new(123456789, 64);
//     let l = BitReader::new(987654321, 64);
//     let r = BitReader::new(456789123, 64);
//
//     let (f, c) = calculator.calculate_bucket(&y1, &l, &r);
//
//     // Verify output sizes
//     assert_eq!(f.get_size(), (k + K_EXTRA_BITS) as usize);
//     assert!(c.get_size() > 0);
// }
//
// #[test]
// fn test_fx_calculate_bucket_high_table() {
//     let k = 32u8;
//     let calculator = FXCalculator::new(k, 5);
//
//     let y1 = BitReader::new(123456789, 64);
//     let l = BitReader::new(987654321, 64);
//     let r = BitReader::new(456789123, 64);
//
//     let (f, c) = calculator.calculate_bucket(&y1, &l, &r);
//
//     // Verify output sizes for higher table indices
//     assert_eq!(f.get_size(), (k + K_EXTRA_BITS) as usize);
//     assert!(c.get_size() > 0);
// }
//
// #[test]
// fn test_fx_find_matches() {
//     let k = 32u8;
//     let mut calculator = FXCalculator::new(k, 2);
//
//     // Create test buckets
//     let bucket_l = vec![
//         PlotEntry { y: K_BC as u64 * 2, pos: 0, offset: 0, metadata: 0 },
//         PlotEntry { y: K_BC as u64 * 2 + 1, pos: 1, offset: 0, metadata: 0 },
//     ];
//
//     let bucket_r = vec![
//         PlotEntry { y: K_BC as u64 * 3, pos: 0, offset: 0, metadata: 0 },
//         PlotEntry { y: K_BC as u64 * 3 + 1, pos: 1, offset: 0, metadata: 0 },
//     ];
//
//     let mut idx_l = vec![0u16; 100];
//     let mut idx_r = vec![0u16; 100];
//
//     let matches = calculator.find_matches(
//         &bucket_l,
//         &bucket_r,
//         Some(&mut idx_l),
//         Some(&mut idx_r)
//     );
//
//     assert!(matches >= 0);
// }
//
// #[test]
// fn test_fx_find_matches_empty_buckets() {
//     let k = 32u8;
//     let mut calculator = FXCalculator::new(k, 2);
//
//     let bucket_l: Vec<PlotEntry> = Vec::new();
//     let bucket_r: Vec<PlotEntry> = Vec::new();
//
//     let mut idx_l = vec![0u16; 100];
//     let mut idx_r = vec![0u16; 100];
//
//     let matches = calculator.find_matches(
//         &bucket_l,
//         &bucket_r,
//         Some(&mut idx_l),
//         Some(&mut idx_r)
//     );
//
//     assert_eq!(matches, 0);
// }
//
// #[test]
// fn test_rmap_item_default() {
//     let item = RmapItem::default();
//     assert_eq!(item.count, 4);
//     assert_eq!(item.pos, 12);
// }
//
