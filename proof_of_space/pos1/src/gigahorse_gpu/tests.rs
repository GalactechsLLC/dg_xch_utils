use super::*;
use crate::chacha8::{ChachaContext, chacha8_get_keystream, chacha8_keysetup};

#[test]
fn chacha_precomputed_columns_match_reference() {
    let key =
        hex::decode("01d5b7f4d52bf134c8eaf3623c45126d58a588bfca38501062ae208e78035138").unwrap();
    let mut parameters = Parameters::default();
    for (destination, bytes) in parameters.words[8..16]
        .iter_mut()
        .zip(key.as_chunks::<4>().0)
    {
        *destination = u32::from_le_bytes(*bytes);
    }
    parameters.precompute_chacha_columns();
    assert_eq!(
        &parameters.words[16..28],
        &[
            0x90c27989, 0x625d2448, 0x121c1391, 0x8ae8ce84, 0x0a52b1e4, 0x120bc1db, 0x21a538db,
            0x4e13a92d, 0x8fcd7f62, 0x81119800, 0xa7f81e75, 0xaccba3b8,
        ]
    );
}

fn hash_parity(device: &impl Device) {
    first_generation_boundaries(device);
    first_histogram_parity(device);
    first_partition_parity(device);
    first_generation_retry_parity(device);
    second_partition_parity(device);
    grouped_partition_scatter_parity(device);
    coalesced_local_parity(device);
    coalesced_local_boundaries(device);
    partitioned_index_parity(device);
    second_gather_layout_parity(device);
    device.launch_batch(&[]).unwrap();
    for operation in [21, u32::MAX] {
        for flags in 0..4 {
            let mut invalid = Parameters::default();
            invalid.words[0] = operation;
            invalid.words[5] = flags;
            assert!(device.launch(&invalid, 0).is_err());
        }
    }
    let status = device.allocate(4).unwrap();
    device.upload(&status, &[0; 4]).unwrap();
    let bitmap = device.allocate(16384).unwrap();
    device.upload(&bitmap, &[u32::MAX; 16384]).unwrap();
    let output = device.allocate(2048 * 4).unwrap();
    let mut parameters = Parameters::default();
    parameters.pointers[1] = output.address();
    parameters.pointers[5] = status.address();
    parameters.pointers[6] = bitmap.address();
    parameters.words[0] = 1;
    parameters.words[1] = 128;
    parameters.words[3] = 2048;
    parameters.words[4] = (1 << 28) - 128;
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &[37; 32], None);
    parameters.words[8..16].copy_from_slice(&context.input[4..12]);
    parameters.precompute_chacha_columns();
    device.launch(&parameters, 1).unwrap();
    assert_eq!(device.download(&status, 4).unwrap(), [2048, 0, 0, 0]);
    let words = device.download(&output, 2048 * 2).unwrap();
    let mut actual: Vec<_> = words
        .as_chunks::<2>()
        .0
        .iter()
        .map(|node| (node[1], u64::from(node[0]) << 6 | u64::from(node[1] >> 26)))
        .collect();
    actual.sort_unstable();
    let mut stream = Vec::new();
    chacha8_get_keystream(&context, u64::from(parameters.words[4]), 128, &mut stream);
    for ((proof_x, value), bytes) in actual.iter().zip(stream.as_chunks::<4>().0) {
        assert_eq!(
            *value,
            u64::from(u32::from_be_bytes(*bytes)) << 6 | u64::from(*proof_x >> 26)
        );
    }
    device.upload(&status, &[0; 4]).unwrap();
    parameters.words[1] = 64;
    let first_half = parameters;
    parameters.words[4] += 64;
    device
        .launch_batch(&[(first_half, 1), (parameters, 1)])
        .unwrap();
    assert_eq!(device.download(&status, 4).unwrap(), [2048, 0, 0, 0]);
    let mut batched: Vec<_> = device
        .download(&output, 2048 * 2)
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|node| (node[1], u64::from(node[0]) << 6 | u64::from(node[1] >> 26)))
        .collect();
    batched.sort_unstable();
    assert_eq!(batched, actual);
    device.upload(&status, &[0; 4]).unwrap();
    let launches: Vec<_> = (0..17)
        .map(|slot| {
            let mut chunk = first_half;
            chunk.words[4] += slot * 7;
            chunk.words[1] = if slot == 16 { 16 } else { 7 };
            (chunk, 1)
        })
        .collect();
    device.launch_batch(&launches).unwrap();
    assert_eq!(device.download(&status, 4).unwrap(), [2048, 0, 0, 0]);
    let mut split_batch: Vec<_> = device
        .download(&output, 2048 * 2)
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|node| (node[1], u64::from(node[0]) << 6 | u64::from(node[1] >> 26)))
        .collect();
    split_batch.sort_unstable();
    assert_eq!(split_batch, actual);
    device.upload(&bitmap, &[1; 16384]).unwrap();
    parameters.words[4] = (1 << 28) - 128;
    for blocks in [0, 1, 7, 33, 65, 127, 128] {
        device.upload(&status, &[0; 4]).unwrap();
        parameters.words[1] = blocks;
        device.launch(&parameters, 1).unwrap();
        let status_words = device.download(&status, 4).unwrap();
        assert_eq!(&status_words[1..], &[0; 3]);
        let mut selected: Vec<_> = device
            .download(&output, status_words[0] as usize * 2)
            .unwrap()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|node| (node[1], u64::from(node[0]) << 6 | u64::from(node[1] >> 26)))
            .collect();
        selected.sort_unstable();
        let expected: Vec<_> = actual
            .iter()
            .copied()
            .filter(|(proof_x, value)| {
                let hash = (value >> 6) as u32;
                let bucket = hash >> 13;
                proof_x / 16 - parameters.words[4] < blocks
                    && (bucket.is_multiple_of(32) || (bucket % 32 == 1 && hash & 8191 < 512))
            })
            .collect();
        assert_eq!(selected, expected);
    }
    device.upload(&bitmap, &[u32::MAX; 16384]).unwrap();
    device.upload(&status, &[0; 4]).unwrap();
    parameters.words[3] = 1;
    device
        .launch_batch(&[(parameters, 1), (parameters, 1)])
        .unwrap();
    assert_eq!(device.download(&status, 4).unwrap()[2], 1);
    device.upload(&status, &[0; 4]).unwrap();
    parameters.words[3] = 2048;
    for table in 2..=7 {
        let stride = if table <= 3 { 4 } else { 8 };
        let left_value = (1u64 << 37) + 123456;
        let left = [0x35_u8; 16];
        let right = [0xab_u8; 16];
        let mut nodes = vec![0u32; stride * 2];
        nodes[0] = left_value as u32;
        nodes[1] = (left_value >> 32) as u32;
        let words = if table == 2 {
            1
        } else if table == 3 {
            2
        } else {
            4
        };
        for index in 0..words {
            nodes[2 + index] = 0x35353535;
            nodes[stride + 2 + index] = 0xabababab;
        }
        let input = device.allocate(nodes.len()).unwrap();
        device.upload(&input, &nodes).unwrap();
        parameters.pointers[0] = input.address();
        parameters.words[0] = 6;
        parameters.words[1] = 1;
        parameters.words[2] = table as u32;
        device.launch(&parameters, 1).unwrap();
        let node = device.download(&output, 8).unwrap();
        let (expected_value, expected_meta) =
            crate::gigahorse_cpu::evaluate(table, left_value, &left, &right);
        assert_eq!(
            u64::from(node[0]) | u64::from(node[1]) << 32,
            expected_value,
            "F{table} value"
        );
        let metadata: Vec<_> = node[2..6]
            .iter()
            .flat_map(|word| word.to_be_bytes())
            .collect();
        assert_eq!(metadata, expected_meta, "F{table} metadata");
    }
    assert!(reconstruct(device, &[0; 32], &[], 0, &|| Ok(())).is_err());
    assert!(
        reconstruct(device, &[0; 32], &[0; 8192], 0, &|| Ok(()))
            .unwrap()
            .is_empty()
    );
    let invalid = C30Table5Entry {
        f_value: 1 << 38,
        metadata: [0; 16],
        xs: [0; 16],
    };
    assert!(finish(device, vec![invalid], &[0; 32], &|| Ok(())).is_err());
    let mut bitmap = [0; 8192];
    bitmap[0] = 1;
    assert!(reconstruct(device, &[0; 32], &bitmap, 0, &|| Ok(())).is_err());
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let cancelled = reconstruct(device, &[0; 32], &bitmap, 1 << 30, &|| {
        if checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 4 {
            Err(Error::new(ErrorKind::Interrupted, "cancelled GPU scan"))
        } else {
            Ok(())
        }
    });
    assert_eq!(cancelled.err().unwrap().kind(), ErrorKind::Interrupted);
    let workspace = Workspace::new(device).unwrap();
    let input = device.allocate(16).unwrap();
    device
        .upload(
            &input,
            &[0, 3, 0, 4, 236, (9 << 26) | 5, 236, (9 << 26) | 6],
        )
        .unwrap();
    let table = Table {
        data: input,
        count: 4,
        number: 1,
        histogram: None,
        partitions: None,
    };
    assert!(workspace.next(&table, 1, &|| Ok(())).is_err());
    for (left_count, right_count) in [(2usize, 33usize), (1, 34), (32, 32), (305, 1), (1, 305)] {
        let mut words = Vec::new();
        for index in 0..left_count {
            words.extend_from_slice(&[0, index as u32]);
        }
        for index in 0..right_count {
            words.extend_from_slice(&[236, (9 << 26) | index as u32]);
        }
        let input = device.allocate(words.len()).unwrap();
        device.upload(&input, &words).unwrap();
        let table = Table {
            data: input,
            count: left_count + right_count,
            number: 1,
            histogram: None,
            partitions: None,
        };
        let output = workspace
            .next(&table, left_count * right_count, &|| Ok(()))
            .unwrap();
        assert_eq!(output.count, left_count * right_count);
        let output = live_table_words(device, &output, 4);
        let mut pairs = std::collections::HashSet::new();
        for node in output.as_chunks::<4>().0 {
            let mut left_meta = [0; 16];
            let mut right_meta = [0; 16];
            left_meta[..4].copy_from_slice(&node[2].to_be_bytes());
            right_meta[..4].copy_from_slice(&node[3].to_be_bytes());
            let expected = crate::gigahorse_cpu::evaluate(2, 0, &left_meta, &right_meta);
            assert_eq!(u64::from(node[0]) | u64::from(node[1]) << 32, expected.0);
            assert!(pairs.insert((node[2], node[3])));
        }
    }
    let input = device.allocate(1025 * 4).unwrap();
    device.upload(&input, &vec![0; 1025 * 4]).unwrap();
    let table = Table {
        data: input,
        count: 1025,
        number: 1,
        histogram: None,
        partitions: None,
    };
    assert!(workspace.next(&table, 1, &|| Ok(())).is_err());
    for (bucket, entries) in [
        (0, 1usize),
        (127, 129),
        (128, 127),
        ((1 << LOCAL_COARSE_BITS) - 1, 129),
        ((1 << COARSE_BITS) - 1, 129),
        (BUCKETS - 2, 2),
    ] {
        let left_value = bucket as u64 * 15113;
        let right_value = left_value + 15113 + (bucket & 1) as u64;
        let mut nodes = Vec::new();
        for value in [left_value, right_value] {
            for index in 0..entries {
                nodes.extend_from_slice(&[
                    value as u32,
                    (value >> 32) as u32,
                    index as u32,
                    index as u32 + 1,
                ]);
            }
        }
        let input = device.allocate(nodes.len()).unwrap();
        device.upload(&input, &nodes).unwrap();
        let table = Table {
            data: input,
            count: entries * 2,
            number: 2,
            histogram: None,
            partitions: None,
        };
        let output = workspace
            .next(&table, entries * entries, &|| Ok(()))
            .unwrap();
        assert_eq!(output.count, entries * entries);
        let output = device.download(&output.data, output.count * 8).unwrap();
        let histogram = device.allocate(BUCKETS).unwrap();
        let mut counts = vec![0; BUCKETS];
        counts[bucket] = entries as u32 | ((entries as u32) << 16);
        counts[bucket + 1] = counts[bucket];
        device.upload(&histogram, &counts).unwrap();
        let cached = Table {
            histogram: Some(Histogram::Fine(histogram)),
            ..table
        };
        let mut expected: Vec<_> = output.as_chunks::<8>().0.to_vec();
        expected.sort_unstable();
        for _ in 0..2 {
            let reconstructed = workspace
                .next(&cached, entries * entries, &|| Ok(()))
                .unwrap();
            assert_eq!(reconstructed.count, entries * entries);
            let words = device
                .download(&reconstructed.data, reconstructed.count * 8)
                .unwrap();
            let mut actual = words.as_chunks::<8>().0.to_vec();
            actual.sort_unstable();
            assert_eq!(actual, expected);
        }
        let Some(Histogram::Fine(fine_histogram)) = &cached.histogram else {
            unreachable!();
        };
        counts[bucket] = entries as u32;
        counts[bucket + 1] = entries as u32;
        assert_eq!(device.download(fine_histogram, BUCKETS).unwrap(), counts);
        let coarse_histogram = device
            .allocate(BUCKETS.div_ceil(1 << LOCAL_COARSE_BITS))
            .unwrap();
        let mut coarse_counts = vec![0; BUCKETS.div_ceil(1 << LOCAL_COARSE_BITS)];
        coarse_counts[bucket >> LOCAL_COARSE_BITS] += entries as u32;
        coarse_counts[(bucket + 1) >> LOCAL_COARSE_BITS] += entries as u32;
        device.upload(&coarse_histogram, &coarse_counts).unwrap();
        let cached = Table {
            histogram: Some(Histogram::Coarse {
                counts: coarse_histogram,
                bits: LOCAL_COARSE_BITS,
            }),
            ..cached
        };
        for _ in 0..2 {
            let reconstructed = workspace
                .next(&cached, entries * entries, &|| Ok(()))
                .unwrap();
            let words = device
                .download(&reconstructed.data, reconstructed.count * 8)
                .unwrap();
            let mut actual = words.as_chunks::<8>().0.to_vec();
            actual.sort_unstable();
            assert_eq!(actual, expected);
        }
        let Some(Histogram::Coarse {
            counts: coarse_histogram,
            ..
        }) = &cached.histogram
        else {
            unreachable!();
        };
        assert_eq!(
            device
                .download(coarse_histogram, coarse_counts.len())
                .unwrap(),
            coarse_counts
        );
        let mut pairs = std::collections::HashSet::new();
        for node in output.as_chunks::<8>().0 {
            let mut left_meta = [0; 16];
            let mut right_meta = [0; 16];
            let left = node[6] as usize;
            let right = node[7] as usize;
            assert!(left < entries && (entries..entries * 2).contains(&right));
            for offset in 0..2 {
                left_meta[offset * 4..offset * 4 + 4]
                    .copy_from_slice(&nodes[left * 4 + 2 + offset].to_be_bytes());
                right_meta[offset * 4..offset * 4 + 4]
                    .copy_from_slice(&nodes[right * 4 + 2 + offset].to_be_bytes());
            }
            let (value, metadata) =
                crate::gigahorse_cpu::evaluate(3, left_value, &left_meta, &right_meta);
            assert_eq!(u64::from(node[0]) | u64::from(node[1]) << 32, value);
            assert_eq!(
                node[2..6]
                    .iter()
                    .flat_map(|word| word.to_be_bytes())
                    .collect::<Vec<_>>(),
                metadata
            );
            assert!(pairs.insert((left, right)));
        }
    }
}

fn compact_second_words(nodes: &[u32], regional_capacity: usize) -> Vec<u32> {
    let capacity = regional_capacity * 64;
    assert_eq!(nodes.len(), capacity * 4);
    let mut words = vec![u32::MAX; capacity * 3];
    for (index, node) in nodes.as_chunks::<4>().0.iter().enumerate() {
        words[index] = node[0];
        words[capacity + index * 2..capacity + index * 2 + 2].copy_from_slice(&node[2..4]);
    }
    words
}

#[test]
fn second_hybrid_layout_preserves_region_boundaries() {
    for regional_capacity in [1, 17, 129] {
        let capacity = regional_capacity * 64;
        let nodes: Vec<_> = (0..capacity)
            .flat_map(|index| {
                [
                    if index % 2 == 0 { 0 } else { u32::MAX },
                    (index / regional_capacity) as u32,
                    0x12345678 ^ index as u32,
                    0xabcdef01 ^ index as u32,
                ]
            })
            .collect();
        let words = compact_second_words(&nodes, regional_capacity);
        assert_eq!(capacity * 4 % 8, 0);
        for (index, expected) in nodes.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(
                [
                    words[index],
                    (index / regional_capacity) as u32,
                    words[capacity + index * 2],
                    words[capacity + index * 2 + 1],
                ],
                *expected
            );
        }
    }
}

fn second_partition_soa_guards<Backend: Device>(
    device: &Backend,
    input: &Backend::Memory,
    count: usize,
    expected: [u32; 4],
) {
    let region_capacity = 17_usize;
    let capacity = region_capacity * 64;
    let guard = 2;
    let words = capacity * 3 + guard * 2;
    let sentinel = u32::MAX;
    let output = device.allocate(words).unwrap();
    device.upload(&output, &vec![sentinel; words]).unwrap();
    let metadata: Vec<_> = (0..64)
        .flat_map(|region| {
            [
                (region * region_capacity) as u32,
                (region_capacity - 1) as u32,
                0,
            ]
        })
        .collect();
    let ranges = device.allocate(metadata.len()).unwrap();
    device.upload(&ranges, &metadata).unwrap();
    let status = device.allocate(4).unwrap();
    device.upload(&status, &[0; 4]).unwrap();
    let mut parameters = Parameters::default();
    parameters.pointers[0] = input.address();
    parameters.pointers[1] = output.address() + (guard * 4) as u64;
    parameters.pointers[5] = status.address();
    parameters.pointers[9] = ranges.address();
    parameters.words[0] = 17;
    parameters.words[1] = count as u32;
    parameters.words[2] = 2;
    parameters.words[5] = region_capacity as u32;
    device
        .launch(
            &parameters,
            count.div_ceil(device.partition_second_kernel_threads()),
        )
        .unwrap();
    assert_eq!(device.download(&status, 4).unwrap()[2], 4);
    let actual = device.download(&output, words).unwrap();
    assert!(actual[..guard].iter().all(|word| *word == sentinel));
    assert!(actual[words - guard..].iter().all(|word| *word == sentinel));
    for index in 0..capacity {
        let row = [
            actual[guard + index],
            actual[guard + capacity + index * 2],
            actual[guard + capacity + index * 2 + 1],
        ];
        if index / region_capacity == expected[1] as usize
            && index % region_capacity < region_capacity - 1
        {
            assert_eq!(row, [expected[0], expected[2], expected[3]]);
        } else {
            assert_eq!(row, [sentinel; 3]);
        }
    }
    let actual_ranges = device.download(&ranges, metadata.len()).unwrap();
    for (region, range) in actual_ranges.as_chunks::<3>().0.iter().enumerate() {
        assert_eq!(&range[..2], &metadata[region * 3..region * 3 + 2]);
        assert_eq!(
            range[2] as usize,
            if region == expected[1] as usize {
                count
            } else {
                0
            }
        );
    }
}

fn second_gather_layout_parity(device: &impl Device) {
    for region_capacity in [1_usize, 17, 129] {
        let capacity = region_capacity * 64;
        let references = [
            0,
            region_capacity - 1,
            region_capacity,
            region_capacity * 2 - 1,
            region_capacity * 31,
            region_capacity * 32,
            region_capacity * 63,
            capacity - 1,
        ];
        let mut nodes = vec![u32::MAX; capacity * 4];
        for index in references {
            nodes[index * 4..index * 4 + 4].copy_from_slice(&[
                if index % 2 == 0 { 0 } else { u32::MAX },
                (index / region_capacity) as u32,
                0x12345678 ^ index as u32,
                0xabcdef01 ^ index as u32,
            ]);
        }
        let mut third_words = vec![0; 4 * 8];
        for (index, reference) in references.iter().enumerate() {
            third_words[index / 2 * 8 + 6 + index % 2] = *reference as u32;
        }
        let mut fourth_words = vec![0; 2 * 8];
        for index in 0..4 {
            fourth_words[index / 2 * 8 + 6 + index % 2] = index as u32;
        }
        let fifth_words = [0x01234567, 7, 1, 2, 3, 4, 0, 1];
        let third = device.allocate(third_words.len()).unwrap();
        let fourth = device.allocate(fourth_words.len()).unwrap();
        let fifth = device.allocate(fifth_words.len()).unwrap();
        device.upload(&third, &third_words).unwrap();
        device.upload(&fourth, &fourth_words).unwrap();
        device.upload(&fifth, &fifth_words).unwrap();
        let mut expected = fifth_words[..6].to_vec();
        for index in references {
            expected.extend_from_slice(&nodes[index * 4 + 2..index * 4 + 4]);
        }
        let mut expected_hashed = third_words.clone();
        for (index, pair) in references.as_chunks::<2>().0.iter().enumerate() {
            let left = &nodes[pair[0] * 4..pair[0] * 4 + 4];
            let right = &nodes[pair[1] * 4..pair[1] * 4 + 4];
            let mut left_meta = [0; 16];
            let mut right_meta = [0; 16];
            for word in 0..2 {
                left_meta[word * 4..word * 4 + 4].copy_from_slice(&left[word + 2].to_be_bytes());
                right_meta[word * 4..word * 4 + 4].copy_from_slice(&right[word + 2].to_be_bytes());
            }
            let value = u64::from(left[0]) | (u64::from(left[1]) << 32);
            let (value, metadata) =
                crate::gigahorse_cpu::evaluate(3, value, &left_meta, &right_meta);
            expected_hashed[index * 8] = value as u32;
            expected_hashed[index * 8 + 1] = (value >> 32) as u32;
            for (word, bytes) in metadata.as_chunks::<4>().0.iter().enumerate() {
                expected_hashed[index * 8 + word + 2] = u32::from_be_bytes(*bytes);
            }
        }
        for compact_f2 in [false, true] {
            let storage = if compact_f2 {
                compact_second_words(&nodes, region_capacity)
            } else {
                nodes.clone()
            };
            let second = device.allocate(storage.len()).unwrap();
            device.upload(&second, &storage).unwrap();
            device.upload(&third, &third_words).unwrap();
            let mut hash = Parameters::default();
            hash.pointers[0] = second.address();
            hash.pointers[1] = third.address();
            hash.words[0] = 8;
            hash.words[1] = 4;
            hash.words[2] = 3;
            hash.words[5] = if compact_f2 {
                region_capacity as u32
            } else {
                0
            };
            device.launch(&hash, 1).unwrap();
            assert_eq!(
                device.download(&third, third_words.len()).unwrap(),
                expected_hashed
            );
            let output = device.allocate(26).unwrap();
            device.upload(&output, &[u32::MAX; 26]).unwrap();
            let mut parameters = Parameters::default();
            parameters.pointers[0] = fifth.address();
            parameters.pointers[1] = output.address() + 8;
            parameters.pointers[7] = second.address();
            parameters.pointers[8] = third.address();
            parameters.pointers[9] = fourth.address();
            parameters.words[0] = 4;
            parameters.words[1] = 1;
            parameters.words[5] = if compact_f2 {
                region_capacity as u32
            } else {
                0
            };
            device.launch(&parameters, 1).unwrap();
            let actual = device.download(&output, 26).unwrap();
            assert_eq!(&actual[..2], &[u32::MAX; 2]);
            assert_eq!(&actual[2..24], expected);
            assert_eq!(&actual[24..], &[u32::MAX; 2]);
            assert_eq!(device.download(&second, storage.len()).unwrap(), storage);
        }
    }
}

fn live_table_words<Backend: Device>(
    device: &Backend,
    table: &Table<Backend::Memory>,
    stride: usize,
) -> Vec<u32> {
    let Some(partitions) = &table.partitions else {
        return device.download(&table.data, table.count * stride).unwrap();
    };
    let ranges = device
        .download(&partitions.ranges, partitions.regions * 3)
        .unwrap();
    if partitions.f2_region_capacity != 0 {
        assert_eq!(table.number, 2);
        assert_eq!(stride, 4);
        assert_eq!(partitions.regions, 64);
        let capacity = partitions.f2_region_capacity as usize * 64;
        let words = device.download(&table.data, capacity * 3).unwrap();
        let mut live = Vec::with_capacity(table.count * 4);
        for (region, range) in ranges.as_chunks::<3>().0.iter().enumerate() {
            assert!(range[2] <= range[1]);
            for index in range[0] as usize..(range[0] + range[2]) as usize {
                assert_eq!(index / partitions.f2_region_capacity as usize, region);
                live.extend_from_slice(&[
                    words[index],
                    region as u32,
                    words[capacity + index * 2],
                    words[capacity + index * 2 + 1],
                ]);
            }
        }
        assert_eq!(live.len(), table.count * 4);
        return live;
    }
    let end = ranges
        .as_chunks::<3>()
        .0
        .iter()
        .map(|range| (range[0] + range[2]) as usize)
        .max()
        .unwrap_or(0);
    let words = device.download(&table.data, end * stride).unwrap();
    let mut live = Vec::with_capacity(table.count * stride);
    for range in ranges.as_chunks::<3>().0 {
        assert!(range[2] <= range[1]);
        live.extend_from_slice(
            &words[range[0] as usize * stride..(range[0] + range[2]) as usize * stride],
        );
    }
    assert_eq!(live.len(), table.count * stride);
    live
}

fn first_partition_parity(device: &impl Device) {
    for regions in [32, 64] {
        for bitmap_summary in [false, true] {
            first_partition_layout_parity(device, regions, bitmap_summary);
        }
    }
}

fn first_partition_layout_parity(device: &impl Device, regions: usize, bitmap_summary: bool) {
    let threads = device.generation_threads();
    let maximum_blocks = threads + 1;
    let regional_capacity = maximum_blocks * 16;
    let regional_span = regional_capacity + 2;
    let status = device.allocate(4).unwrap();
    let bitmap = device
        .allocate(if bitmap_summary { 20480 } else { 16384 })
        .unwrap();
    let ranges = device.allocate(regions * 3).unwrap();
    let output_words = regional_span * regions * 2;
    let output = device.allocate(output_words).unwrap();
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &[37; 32], None);
    for (bitmap_word, blocks, capacity) in [
        (0_u32, maximum_blocks, regional_capacity),
        (0x00000001, maximum_blocks, regional_capacity),
        (0x01010101, threads - 1, regional_capacity),
        (u32::MAX, maximum_blocks, regional_capacity),
        (u32::MAX, maximum_blocks, 0),
        (u32::MAX, maximum_blocks, 1),
        (0x01010101, threads, 0),
        (0x01010101, threads, 3),
    ] {
        let metadata: Vec<_> = (0..regions)
            .flat_map(|region| [(region * regional_span + 1) as u32, capacity as u32, 0])
            .collect();
        device.upload(&ranges, &metadata).unwrap();
        device.upload(&status, &[0; 4]).unwrap();
        let mut bitmap_words = vec![bitmap_word; 16384];
        if bitmap_summary {
            bitmap_words.extend(first_bitmap_summary(&bitmap_words));
        }
        device.upload(&bitmap, &bitmap_words).unwrap();
        device
            .upload(&output, &vec![u32::MAX; output_words])
            .unwrap();
        let mut parameters = Parameters::default();
        parameters.pointers[1] = output.address();
        parameters.pointers[3] = ranges.address();
        parameters.pointers[5] = status.address();
        parameters.pointers[6] = bitmap.address();
        parameters.words[0] = 18;
        parameters.words[1] = blocks as u32;
        parameters.words[3] = (regional_capacity * regions) as u32;
        parameters.words[4] = (1 << 28) - blocks as u32;
        parameters.words[5] = u32::from(bitmap_summary) | if regions == 64 { 2 } else { 0 };
        parameters.words[8..16].copy_from_slice(&context.input[4..12]);
        parameters.precompute_chacha_columns();
        device
            .launch(&parameters, blocks.div_ceil(threads))
            .unwrap();
        let counters = device.download(&status, 4).unwrap();
        let metadata = device.download(&ranges, regions * 3).unwrap();
        let words = device.download(&output, output_words).unwrap();
        let mut stream = Vec::new();
        chacha8_get_keystream(
            &context,
            u64::from(parameters.words[4]),
            blocks as u32,
            &mut stream,
        );
        let mut expected = Vec::new();
        let mut expected_counts = vec![0_u32; regions];
        let region_shift = if regions == 64 { 26 } else { 27 };
        for (offset, bytes) in stream.as_chunks::<4>().0.iter().enumerate() {
            let hash = u32::from_be_bytes(*bytes);
            let bucket = hash >> 13;
            if (bitmap_word >> (bucket % 32)) & 1 != 0
                || (bucket > 0
                    && hash & 8191 < 512
                    && (bitmap_word >> ((bucket - 1) % 32)) & 1 != 0)
            {
                expected.push([hash, parameters.words[4] * 16 + offset as u32]);
                expected_counts[(hash >> region_shift) as usize] += 1;
            }
        }
        assert_eq!(counters[0], 0);
        assert_eq!(
            metadata
                .as_chunks::<3>()
                .0
                .iter()
                .map(|range| range[2] as usize)
                .sum::<usize>(),
            expected.len(),
        );
        let overflow = expected_counts
            .iter()
            .any(|count| *count as usize > capacity);
        assert_eq!(counters[2], if overflow { 4 } else { 0 });
        let mut actual = Vec::new();
        for (region, range) in metadata.as_chunks::<3>().0.iter().enumerate() {
            assert_eq!(range[2], expected_counts[region]);
            let start = range[0] as usize * 2;
            let end = start + range[2].min(range[1]) as usize * 2;
            assert!(
                words[region * regional_span * 2..start]
                    .iter()
                    .all(|word| *word == u32::MAX),
                "F1 region {region} leading guard"
            );
            assert!(
                words[end..(region + 1) * regional_span * 2]
                    .iter()
                    .all(|word| *word == u32::MAX),
                "F1 region {region} trailing guard"
            );
            for node in words[start..end].as_chunks::<2>().0 {
                assert_eq!(node[0] >> region_shift, region as u32);
                actual.push(*node);
            }
        }
        actual.sort_unstable();
        expected.sort_unstable();
        if overflow {
            assert_eq!(
                actual.len(),
                expected_counts
                    .iter()
                    .map(|count| (*count as usize).min(capacity))
                    .sum::<usize>()
            );
            assert!(actual.windows(2).all(|pair| pair[0] != pair[1]));
            assert!(
                actual
                    .iter()
                    .all(|node| expected.binary_search(node).is_ok())
            );
        } else {
            assert_eq!(actual, expected);
        }
    }
}

fn first_generation_retry_parity(device: &impl Device) {
    let workspace = Workspace::new(device).unwrap();
    let bitmap = device.allocate(20480).unwrap();
    let mut bitmap_words = vec![0; 16384];
    bitmap_words[0] = 1;
    bitmap_words.extend(first_bitmap_summary(&bitmap_words));
    device.upload(&bitmap, &bitmap_words).unwrap();
    let ordinary_checks = std::sync::atomic::AtomicUsize::new(0);
    let ordinary = generate_first(&workspace, &[37; 32], &bitmap, false, 65536, None, &|| {
        ordinary_checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    })
    .unwrap();
    assert!(ordinary.count > 0);
    let expected = live_table_words(device, &ordinary, 2);
    let mut expected = expected.as_chunks::<2>().0.to_vec();
    expected.sort_unstable();
    for regions in [32, 64] {
        let retry_checks = std::sync::atomic::AtomicUsize::new(0);
        let retried = generate_first(
            &workspace,
            &[37; 32],
            &bitmap,
            true,
            65536,
            Some(&vec![0; regions * 3]),
            &|| {
                retry_checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();
        assert!(retried.partitions.is_none());
        assert!(matches!(&retried.histogram, Some(Histogram::Fine(_))));
        assert!(
            retry_checks.load(std::sync::atomic::Ordering::Relaxed)
                > ordinary_checks.load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(retried.count, ordinary.count);
        let actual = live_table_words(device, &retried, 2);
        let mut actual = actual.as_chunks::<2>().0.to_vec();
        actual.sort_unstable();
        assert_eq!(actual, expected);
        let status = workspace.status().unwrap();
        assert_eq!(status[0] as usize, retried.count);
        assert_eq!(status[2], 0);
    }
}

fn second_partition_parity(device: &impl Device) {
    let threads = device.partition_second_kernel_threads();
    let workspace = Workspace::new(device).unwrap();
    for (compact_f2, count, repeated) in [false, true].into_iter().flat_map(|compact_f2| {
        [
            (0, false),
            (1, false),
            (17, false),
            (threads + 1, false),
            (threads + 1, true),
        ]
        .map(|(count, repeated)| (compact_f2, count, repeated))
    }) {
        let mut nodes = Vec::new();
        let mut expected = Vec::new();
        for index in 0..count {
            let seed = if repeated { 0 } else { index as u64 };
            let value = (1_u64 << 37) + seed * 15113;
            let left = (0x12345678_u64 + seed) as u32;
            let right = (0xabcdef01_u64 - seed) as u32;
            nodes.extend_from_slice(&[value as u32, (value >> 32) as u32, left, right]);
            let mut left_meta = [0; 16];
            let mut right_meta = [0; 16];
            left_meta[..4].copy_from_slice(&left.to_be_bytes());
            right_meta[..4].copy_from_slice(&right.to_be_bytes());
            let (value, _) = crate::gigahorse_cpu::evaluate(2, value, &left_meta, &right_meta);
            expected.push([value as u32, (value >> 32) as u32, left, right]);
        }
        let input = device.allocate(nodes.len().max(1)).unwrap();
        device.upload(&input, &nodes).unwrap();
        let table = workspace
            .partition_second(
                &input,
                count,
                count.max(1) * 64,
                threads,
                compact_f2,
                &|| Ok(()),
            )
            .unwrap()
            .unwrap();
        let words = live_table_words(device, &table, 4);
        let mut actual = words.as_chunks::<4>().0.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        let bits = device.local_coarse_bits();
        let mut coarse_counts = vec![0; BUCKETS.div_ceil(1 << bits)];
        for node in &expected {
            let value = u64::from(node[0]) | u64::from(node[1]) << 32;
            coarse_counts[(value / 15113) as usize >> bits] += 1;
        }
        let Some(Histogram::Coarse {
            counts: histogram,
            bits: actual_bits,
        }) = &table.histogram
        else {
            panic!("partitioned F2 must retain its coarse histogram");
        };
        assert_eq!(*actual_bits, bits);
        assert_eq!(
            device.download(histogram, coarse_counts.len()).unwrap(),
            coarse_counts
        );
        assert_eq!(
            actual, expected,
            "partitioned F2 count={count}, repeated={repeated}"
        );
        assert_eq!(device.download(&input, nodes.len()).unwrap(), nodes);
        if repeated {
            assert!(
                workspace
                    .partition_second(&input, count, 1, threads, compact_f2, &|| Ok(()))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(device.download(&input, nodes.len()).unwrap(), nodes);
            if compact_f2 {
                second_partition_soa_guards(device, &input, count, expected[0]);
            }
        }
        if count == 0 {
            let empty = workspace
                .partition_second(&input, 0, 0, threads, compact_f2, &|| Ok(()))
                .unwrap()
                .unwrap();
            assert_eq!(empty.count, 0);
            assert_eq!(empty.partitions.as_ref().unwrap().f2_region_capacity, 0);
            assert!(live_table_words(device, &empty, 4).is_empty());
            continue;
        }
        let ranges = device.allocate(64 * 3).unwrap();
        device.upload(&ranges, &[0; 64 * 3]).unwrap();
        let output = device.allocate(4).unwrap();
        device.upload(&output, &[u32::MAX; 4]).unwrap();
        device.upload(&workspace.status, &[0; 4]).unwrap();
        let mut parameters = Parameters::default();
        parameters.pointers[0] = input.address();
        parameters.pointers[1] = output.address();
        parameters.pointers[5] = workspace.status.address();
        parameters.pointers[9] = ranges.address();
        parameters.words[0] = 17;
        parameters.words[1] = count as u32;
        parameters.words[2] = 2;
        parameters.words[5] = u32::from(compact_f2);
        device.launch(&parameters, count.div_ceil(threads)).unwrap();
        assert_eq!(device.download(&workspace.status, 4).unwrap()[2], 4);
        assert_eq!(device.download(&output, 4).unwrap(), [u32::MAX; 4]);
    }
}

fn grouped_partition_scatter_parity(device: &impl Device) {
    if !device.grouped_partition_scatter() {
        return;
    }
    for bits in [9, 10] {
        for compact_f2 in [false, true] {
            grouped_partition_scatter_bits(device, bits, compact_f2);
        }
    }
}

fn grouped_partition_scatter_bits(device: &impl Device, bits: u32, compact_f2: bool) {
    let coarse_count = BUCKETS.div_ceil(1 << bits);
    let region_capacity = 2061;
    let mut nodes = if compact_f2 {
        vec![u32::MAX; 64 * region_capacity * 4]
    } else {
        Vec::new()
    };
    let mut jobs = Vec::new();
    let mut expected = vec![Vec::new(); coarse_count];
    for (region, count) in [(0_u32, 2057_usize), (1, 257), (9, 1), (63, 31)] {
        let start = if compact_f2 {
            region as usize * region_capacity + 1
        } else {
            nodes.extend_from_slice(&[u32::MAX; 12]);
            nodes.len() / 4
        };
        for index in 0..count {
            let low = if region == 0 && index < 256 {
                1234
            } else {
                (index as u32).wrapping_mul(0x9e3779b9)
            };
            let value = u64::from(low) | u64::from(region) << 32;
            let coarse = (value / 15113) as usize >> bits;
            let relative = value - ((coarse as u64) << bits) * 15113;
            expected[coarse].push([relative as u32, (start + index) as u32]);
            let row = [low, region, index as u32, index as u32 + 1];
            if compact_f2 {
                nodes[(start + index) * 4..(start + index + 1) * 4].copy_from_slice(&row);
            } else {
                nodes.extend_from_slice(&row);
            }
        }
        for offset in (0..count).step_by(128) {
            jobs.extend_from_slice(&[
                (start + offset) as u32,
                (count - offset).min(128) as u32,
                region,
            ]);
        }
    }
    let mut metadata = Vec::new();
    let mut total = 0;
    for entries in &expected {
        total += entries.len();
        metadata.extend_from_slice(&[total as u32, entries.len() as u32]);
    }
    let storage = if compact_f2 {
        compact_second_words(&nodes, region_capacity)
    } else {
        nodes.clone()
    };
    let input = device.allocate(storage.len()).unwrap();
    device.upload(&input, &storage).unwrap();
    let descriptors = device.allocate(jobs.len()).unwrap();
    device.upload(&descriptors, &jobs).unwrap();
    let coarse = device.allocate(metadata.len()).unwrap();
    let staging = device.allocate(total * 2).unwrap();
    let mut parameters = Parameters::default();
    parameters.pointers[0] = input.address();
    parameters.pointers[7] = coarse.address();
    parameters.pointers[8] = staging.address();
    parameters.pointers[9] = descriptors.address();
    parameters.words[0] = 19;
    parameters.words[1] = (jobs.len() / 3) as u32;
    parameters.words[2] = 2;
    parameters.words[5] = if compact_f2 {
        region_capacity as u32
    } else {
        0
    };
    parameters.words[7] = bits;
    for _ in 0..2 {
        device.upload(&coarse, &metadata).unwrap();
        device.upload(&staging, &vec![u32::MAX; total * 2]).unwrap();
        device
            .launch(&parameters, (jobs.len() / 3).div_ceil(16))
            .unwrap();
        let words = device.download(&staging, total * 2).unwrap();
        let remaining = device.download(&coarse, metadata.len()).unwrap();
        let mut start = 0;
        for (bucket, expected) in expected.iter_mut().enumerate() {
            let end = start + expected.len();
            let mut actual = words[start * 2..end * 2].as_chunks::<2>().0.to_vec();
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, *expected, "grouped scatter coarse bucket {bucket}");
            assert_eq!(remaining[bucket * 2], start as u32);
            assert_eq!(remaining[bucket * 2 + 1], expected.len() as u32);
            start = end;
        }
        assert_eq!(device.download(&input, storage.len()).unwrap(), storage);
    }
}

fn coalesced_local_parity(device: &impl Device) {
    if device.local_csr_threads() == 0 {
        return;
    }
    let heads = device.allocate(512).unwrap();
    let counts = device.allocate(512).unwrap();
    let status = device.allocate(4).unwrap();
    let coarse = device.allocate(2).unwrap();
    for (count, invalid) in [
        (0, 0),
        (1, 0),
        (127, 0),
        (4096, 0),
        (4097, 0),
        (4608, 0),
        (1025, 2),
        (1, 3),
    ] {
        let start = 7_usize;
        let sentinel = u32::MAX;
        let mut input = vec![sentinel; (start + count) * 2];
        let mut expected = vec![Vec::new(); 512];
        for index in 0..count {
            let bucket = if invalid == 2 { 0 } else { index % 512 };
            let relative = if invalid == 3 {
                512 * 15113
            } else {
                bucket * 15113 + index % 15113
            };
            let source = 0x80000000_u32 + index as u32;
            input[(start + index) * 2] = relative as u32;
            input[(start + index) * 2 + 1] = source;
            if invalid == 0 {
                expected[bucket].push([(relative % (128 * 15113)) as u32, source]);
            }
        }
        let staging = device.allocate(input.len()).unwrap();
        device.upload(&staging, &input).unwrap();
        let output_words = (start + count + 3) * 2;
        let packed = device.allocate(output_words).unwrap();
        let scatter_jobs = device.allocate((count.div_ceil(128) + 1) * 3).unwrap();
        let matching_jobs = device.allocate((count.div_ceil(128) + 4) * 3).unwrap();
        device
            .upload(&packed, &vec![sentinel; output_words])
            .unwrap();
        device.upload(&status, &[0; 4]).unwrap();
        device
            .upload(&coarse, &[start as u32, count as u32])
            .unwrap();
        let mut parameters = Parameters::default();
        parameters.pointers[2] = heads.address();
        parameters.pointers[3] = counts.address();
        parameters.pointers[5] = status.address();
        parameters.pointers[6] = packed.address();
        parameters.pointers[7] = coarse.address();
        parameters.pointers[8] = staging.address();
        parameters.pointers[9] = scatter_jobs.address();
        parameters.pointers[11] = matching_jobs.address();
        parameters.words[0] = 20;
        parameters.words[1] = count as u32;
        parameters.words[2] = 2;
        parameters.words[7] = 9;
        device.launch(&parameters, 1).unwrap();
        let counters = device.download(&status, 4).unwrap();
        assert_eq!(counters[2], invalid);
        if invalid != 0 {
            assert_eq!(
                device.download(&packed, output_words).unwrap(),
                vec![sentinel; output_words]
            );
            continue;
        }
        let expected_fallback = if count > 4096 { count.div_ceil(128) } else { 0 };
        assert_eq!(counters[0] as usize, expected_fallback);
        if expected_fallback != 0 {
            parameters.words[0] = 12;
            parameters.words[6] = 4;
            device.launch(&parameters, expected_fallback).unwrap();
        }
        let words = device.download(&packed, output_words).unwrap();
        assert!(words[..start * 2].iter().all(|word| *word == sentinel));
        assert!(
            words[(start + count) * 2..]
                .iter()
                .all(|word| *word == sentinel)
        );
        let actual_heads = device.download(&heads, 512).unwrap();
        let actual_counts = device.download(&counts, 512).unwrap();
        let mut cursor = start;
        let mut expected_jobs = Vec::new();
        for (partition, buckets) in expected.chunks_mut(128).enumerate() {
            let partition_start = cursor;
            let partition_count = buckets.iter().map(Vec::len).sum::<usize>();
            for offset in (0..partition_count).step_by(128) {
                expected_jobs.push([
                    (partition_start + offset) as u32,
                    (partition_count - offset).min(128) as u32,
                    (partition * 128) as u32,
                ]);
            }
            for (bucket_offset, entries) in buckets.iter_mut().enumerate() {
                let bucket = partition * 128 + bucket_offset;
                assert_eq!(actual_heads[bucket] as usize, cursor);
                assert_eq!(actual_counts[bucket] as usize, entries.len());
                let end = cursor + entries.len();
                let mut actual = words[cursor * 2..end * 2].as_chunks::<2>().0.to_vec();
                actual.sort_unstable();
                entries.sort_unstable();
                assert_eq!(actual, *entries, "local CSR count={count}, bucket={bucket}");
                cursor = end;
            }
        }
        assert_eq!(counters[1] as usize, expected_jobs.len());
        let mut actual_jobs = device
            .download(&matching_jobs, counters[1] as usize * 3)
            .unwrap()
            .as_chunks::<3>()
            .0
            .to_vec();
        actual_jobs.sort_unstable();
        expected_jobs.sort_unstable();
        assert_eq!(actual_jobs, expected_jobs);
        assert_eq!(device.download(&staging, input.len()).unwrap(), input);
    }
}

fn coalesced_local_boundaries(device: &impl Device) {
    if device.local_csr_threads() == 0 {
        return;
    }
    let final_group = BUCKETS.div_ceil(512) - 1;
    for (group_count, groups) in [
        (3, vec![(0, 129), (1, 4097), (2, 4096)]),
        (final_group + 1, vec![(final_group, 257)]),
    ] {
        let guard = 4_usize;
        let sentinel = u32::MAX;
        let fine_count = (group_count * 512).min(BUCKETS);
        let metadata_words = fine_count + guard * 2;
        let heads = device.allocate(metadata_words).unwrap();
        let counts = device.allocate(metadata_words).unwrap();
        let initial_metadata = vec![sentinel; metadata_words];
        device.upload(&heads, &initial_metadata).unwrap();
        device.upload(&counts, &initial_metadata).unwrap();
        drop(initial_metadata);
        let mut metadata = vec![0_u32; group_count * 2];
        let mut input = Vec::new();
        let mut expected = std::collections::BTreeMap::new();
        let mut expected_scatter = Vec::new();
        let mut expected_matching = Vec::new();
        let mut occupied = Vec::new();
        for (group, count) in groups {
            input.extend_from_slice(&[sentinel; 14]);
            let start = input.len() / 2;
            let buckets = (BUCKETS - group * 512).min(512);
            let mut entries = vec![Vec::new(); buckets];
            for index in 0..count {
                let bucket = index % buckets;
                let relative = (bucket * 15113) as u32;
                let source = 0x80000000 | (start + index) as u32;
                input.extend_from_slice(&[relative, source]);
                entries[bucket].push([relative % (128 * 15113), source]);
            }
            metadata[group * 2] = start as u32;
            metadata[group * 2 + 1] = count as u32;
            occupied.push(start..start + count);
            if count > 4096 {
                for offset in (0..count).step_by(128) {
                    expected_scatter.push([
                        (start + offset) as u32,
                        (count - offset).min(128) as u32,
                        group as u32,
                    ]);
                }
            }
            let mut cursor = start;
            for (partition, buckets) in entries.chunks_mut(128).enumerate() {
                let partition_count = buckets.iter().map(Vec::len).sum::<usize>();
                for offset in (0..partition_count).step_by(128) {
                    expected_matching.push([
                        (cursor + offset) as u32,
                        (partition_count - offset).min(128) as u32,
                        (group * 512 + partition * 128) as u32,
                    ]);
                }
                for (offset, rows) in buckets.iter_mut().enumerate() {
                    rows.sort_unstable();
                    expected.insert(
                        group * 512 + partition * 128 + offset,
                        (cursor, rows.clone()),
                    );
                    cursor += rows.len();
                }
            }
            assert_eq!(cursor, start + count);
        }
        input.extend_from_slice(&[sentinel; 14]);
        let staging = device.allocate(input.len()).unwrap();
        let packed = device.allocate(input.len()).unwrap();
        let coarse = device.allocate(metadata.len()).unwrap();
        let status = device.allocate(4).unwrap();
        let scatter_jobs = device.allocate(expected_scatter.len().max(1) * 3).unwrap();
        let matching_jobs = device.allocate(expected_matching.len().max(1) * 3).unwrap();
        device.upload(&staging, &input).unwrap();
        device
            .upload(&packed, &vec![sentinel; input.len()])
            .unwrap();
        device.upload(&coarse, &metadata).unwrap();
        device.upload(&status, &[0; 4]).unwrap();
        let mut parameters = Parameters::default();
        parameters.pointers[2] = heads.address() + (guard * 4) as u64;
        parameters.pointers[3] = counts.address() + (guard * 4) as u64;
        parameters.pointers[5] = status.address();
        parameters.pointers[6] = packed.address();
        parameters.pointers[7] = coarse.address();
        parameters.pointers[8] = staging.address();
        parameters.pointers[9] = scatter_jobs.address();
        parameters.pointers[11] = matching_jobs.address();
        parameters.words[0] = 20;
        parameters.words[1] = occupied.iter().map(|range| range.len()).sum::<usize>() as u32;
        parameters.words[2] = 2;
        parameters.words[7] = 9;
        device.launch(&parameters, group_count).unwrap();
        let counters = device.download(&status, 4).unwrap();
        assert_eq!(
            counters,
            [
                expected_scatter.len() as u32,
                expected_matching.len() as u32,
                0,
                0
            ]
        );
        let before_fallback = device.download(&heads, metadata_words).unwrap();
        let actual_counts = device.download(&counts, metadata_words).unwrap();
        for (&bucket, (start, rows)) in &expected {
            let fallback = metadata[(bucket / 512) * 2 + 1] > 4096;
            assert_eq!(
                before_fallback[guard + bucket] as usize,
                start + if fallback { rows.len() } else { 0 }
            );
        }
        if !expected_scatter.is_empty() {
            parameters.words[0] = 12;
            parameters.words[6] = 4;
            device.launch(&parameters, expected_scatter.len()).unwrap();
            assert_eq!(device.download(&status, 4).unwrap(), counters);
        }
        let actual_heads = device.download(&heads, metadata_words).unwrap();
        let output = device.download(&packed, input.len()).unwrap();
        for words in [&actual_heads, &actual_counts] {
            assert!(words[..guard].iter().all(|word| *word == sentinel));
            assert!(
                words[guard + fine_count..]
                    .iter()
                    .all(|word| *word == sentinel)
            );
        }
        for (&bucket, (start, rows)) in &expected {
            assert_eq!(actual_heads[guard + bucket] as usize, *start);
            assert_eq!(actual_counts[guard + bucket] as usize, rows.len());
            let mut actual = output[start * 2..(start + rows.len()) * 2]
                .as_chunks::<2>()
                .0
                .to_vec();
            actual.sort_unstable();
            assert_eq!(
                actual, *rows,
                "coalesced boundary group_count={group_count}, bucket={bucket}"
            );
        }
        for (group, coarse_range) in metadata.as_chunks::<2>().0.iter().enumerate() {
            if coarse_range[1] == 0 {
                let range = guard + group * 512..guard + ((group + 1) * 512).min(fine_count);
                assert!(actual_heads[range.clone()].iter().all(|word| *word == 0));
                assert!(actual_counts[range].iter().all(|word| *word == 0));
            }
        }
        for (index, row) in output.as_chunks::<2>().0.iter().enumerate() {
            if !occupied.iter().any(|range| range.contains(&index)) {
                assert_eq!(*row, [sentinel; 2]);
            }
        }
        for (buffer, expected_jobs) in [
            (&scatter_jobs, &mut expected_scatter),
            (&matching_jobs, &mut expected_matching),
        ] {
            if !expected_jobs.is_empty() {
                let mut actual = device
                    .download(buffer, expected_jobs.len() * 3)
                    .unwrap()
                    .as_chunks::<3>()
                    .0
                    .to_vec();
                actual.sort_unstable();
                expected_jobs.sort_unstable();
                assert_eq!(actual, *expected_jobs);
            }
        }
        assert_eq!(device.download(&staging, input.len()).unwrap(), input);
        assert_eq!(device.download(&coarse, metadata.len()).unwrap(), metadata);
    }
}

fn partitioned_index_parity(device: &impl Device) {
    let workspace = Workspace::new(device).unwrap();
    for (number, compact_f2) in [(1_u32, false), (2, false), (2, true)] {
        let bits = if number == 1 { 33 } else { 32 };
        let regions = if number == 1 { 32 } else { 64 };
        let stride = if number == 1 { 2 } else { 4 };
        let mut nodes = Vec::new();
        let mut values = vec![
            (0_u64, 1),
            (127 * 15113, 129),
            ((1 << bits) - 1, 1),
            (1 << bits, 1),
        ];
        if number == 2 {
            values.extend([(63_u64 << 32, 1), ((1_u64 << 38) - 1, 1)]);
        }
        for (value, repetitions) in values {
            let bucket = value / 15113;
            let remainder = value % 15113;
            let right =
                (bucket + 1) * 15113 + remainder / 127 * 127 + (remainder + (bucket & 1)) % 127;
            for value in [value, right] {
                if value >= 1 << 38 {
                    continue;
                }
                for _ in 0..repetitions {
                    let index = nodes.len() as u32;
                    nodes.push(if number == 1 {
                        vec![(value >> 6) as u32, ((value as u32 & 63) << 26) | index]
                    } else {
                        vec![value as u32, (value >> 32) as u32, index, index + 1]
                    });
                }
            }
        }
        let value_of = |node: &[u32]| {
            if number == 1 {
                u64::from(node[0]) << 6 | u64::from(node[1] >> 26)
            } else {
                u64::from(node[0]) | u64::from(node[1]) << 32
            }
        };
        let regional_capacity = nodes.len() + 3;
        let mut data = vec![u32::MAX; regions * regional_capacity * stride];
        let mut metadata: Vec<_> = (0..regions)
            .flat_map(|region| {
                [
                    (region * regional_capacity + 1) as u32,
                    (regional_capacity - 2) as u32,
                    0,
                ]
            })
            .collect();
        let mut physical = Vec::new();
        for node in &nodes {
            let region = (value_of(node) >> bits) as usize;
            let destination = (metadata[region * 3] + metadata[region * 3 + 2]) as usize;
            metadata[region * 3 + 2] += 1;
            data[destination * stride..(destination + 1) * stride].copy_from_slice(node);
            physical.push(destination as u32);
        }
        let mut expected = Vec::new();
        for (left_index, left) in nodes.iter().enumerate() {
            let left_value = value_of(left);
            let bucket = left_value / 15113;
            let remainder = left_value % 15113;
            for (right_index, right) in nodes.iter().enumerate() {
                let right_value = value_of(right);
                let right_remainder = right_value % 15113;
                let target = (right_remainder / 127 + 119 - remainder / 127) % 119;
                let parity = 2 * target + (bucket & 1);
                if right_value / 15113 != bucket + 1
                    || target >= 64
                    || (parity * parity + remainder) % 127 != right_remainder % 127
                {
                    continue;
                }
                let mut left_meta = [0; 16];
                let mut right_meta = [0; 16];
                let start = if number == 1 { 1 } else { 2 };
                for offset in 0..number as usize {
                    left_meta[offset * 4..offset * 4 + 4]
                        .copy_from_slice(&left[start + offset].to_be_bytes());
                    right_meta[offset * 4..offset * 4 + 4]
                        .copy_from_slice(&right[start + offset].to_be_bytes());
                }
                let (value, meta) = crate::gigahorse_cpu::evaluate(
                    (number + 1) as usize,
                    left_value,
                    &left_meta,
                    &right_meta,
                );
                let mut result = vec![value as u32, (value >> 32) as u32];
                if number == 1 {
                    result.extend_from_slice(&[left[1], right[1]]);
                } else {
                    result.extend(
                        meta.as_chunks::<4>()
                            .0
                            .iter()
                            .map(|word| u32::from_be_bytes(*word)),
                    );
                    result.extend_from_slice(&[physical[left_index], physical[right_index]]);
                }
                expected.push(result);
            }
        }
        let storage = if compact_f2 {
            compact_second_words(&data, regional_capacity)
        } else {
            data.clone()
        };
        let input = device.allocate(storage.len()).unwrap();
        device.upload(&input, &storage).unwrap();
        let ranges = device.allocate(metadata.len()).unwrap();
        device.upload(&ranges, &metadata).unwrap();
        let jobs = metadata
            .as_chunks::<3>()
            .0
            .iter()
            .map(|range| (range[2] as usize).div_ceil(128))
            .sum();
        let mut table = Table {
            data: input,
            histogram: None,
            partitions: Some(Partitions {
                ranges,
                regions,
                jobs,
                f2_region_capacity: if compact_f2 {
                    regional_capacity as u32
                } else {
                    0
                },
            }),
            count: nodes.len(),
            number,
        };
        for cached_histogram in [0, 1, 2] {
            if cached_histogram != 0 && number != 2 {
                break;
            }
            let cached_counts = if cached_histogram != 0 {
                let bits = if cached_histogram == 2 {
                    0
                } else {
                    LOCAL_COARSE_BITS
                };
                let mut counts = vec![0; BUCKETS.div_ceil(1 << bits)];
                for node in &nodes {
                    counts[(value_of(node) / 15113) as usize >> bits] += 1;
                }
                let histogram = device.allocate(counts.len()).unwrap();
                device.upload(&histogram, &counts).unwrap();
                table.histogram = Some(if bits == 0 {
                    Histogram::Fine(histogram)
                } else {
                    Histogram::Coarse {
                        counts: histogram,
                        bits,
                    }
                });
                Some(counts)
            } else {
                None
            };
            for _ in 0..2 {
                let output = workspace.next(&table, expected.len(), &|| Ok(())).unwrap();
                assert_eq!(output.count, expected.len());
                let words = live_table_words(device, &output, if number == 1 { 4 } else { 8 });
                let mut actual: Vec<_> = words
                    .chunks_exact(if number == 1 { 4 } else { 8 })
                    .map(<[u32]>::to_vec)
                    .collect();
                actual.sort_unstable();
                expected.sort_unstable();
                assert_eq!(
                    actual, expected,
                    "partitioned F{number} references, cached={cached_histogram}"
                );
                if let Some(counts) = &cached_counts {
                    let histogram = match &table.histogram {
                        Some(Histogram::Coarse { counts, .. }) | Some(Histogram::Fine(counts)) => {
                            counts
                        }
                        None => unreachable!(),
                    };
                    assert_eq!(device.download(histogram, counts.len()).unwrap(), *counts);
                }
            }
        }
        assert_eq!(
            device.download(&table.data, storage.len()).unwrap(),
            storage
        );
    }
}

fn first_generation_boundaries(device: &impl Device) {
    let status = device.allocate(4).unwrap();
    let bitmap = device.allocate(16384).unwrap();
    let threads = device.generation_threads();
    let output = device.allocate((threads + 1) * 16 * 2).unwrap();
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &[37; 32], None);
    for bitmap_word in [0_u32, 0x01010101, u32::MAX] {
        device.upload(&bitmap, &[bitmap_word; 16384]).unwrap();
        for blocks in [threads - 1, threads, threads + 1] {
            device.upload(&status, &[0; 4]).unwrap();
            let mut parameters = Parameters::default();
            parameters.pointers[1] = output.address();
            parameters.pointers[5] = status.address();
            parameters.pointers[6] = bitmap.address();
            parameters.words[0] = 1;
            parameters.words[1] = blocks as u32;
            parameters.words[3] = (blocks * 16) as u32;
            parameters.words[4] = (1 << 28) - blocks as u32;
            parameters.words[8..16].copy_from_slice(&context.input[4..12]);
            parameters.precompute_chacha_columns();
            device
                .launch(&parameters, blocks.div_ceil(threads))
                .unwrap();
            let counters = device.download(&status, 4).unwrap();
            assert_eq!(counters[2], 0);
            let output = device.download(&output, counters[0] as usize * 2).unwrap();
            let mut actual = output.as_chunks::<2>().0.to_vec();
            actual.sort_unstable();
            let mut stream = Vec::new();
            chacha8_get_keystream(
                &context,
                u64::from(parameters.words[4]),
                blocks as u32,
                &mut stream,
            );
            let mut expected = Vec::new();
            for (offset, bytes) in stream.as_chunks::<4>().0.iter().enumerate() {
                let hash = u32::from_be_bytes(*bytes);
                let bucket = hash >> 13;
                if (bitmap_word >> (bucket % 32)) & 1 != 0
                    || (bucket > 0
                        && hash & 8191 < 512
                        && (bitmap_word >> ((bucket - 1) % 32)) & 1 != 0)
                {
                    expected.push([hash, parameters.words[4] * 16 + offset as u32]);
                }
            }
            expected.sort_unstable();
            assert_eq!(
                actual, expected,
                "F1 threads={threads}, blocks={blocks}, bitmap={bitmap_word:x}"
            );
        }
    }
}

fn first_histogram_parity(device: &impl Device) {
    let histogram = device.allocate(BUCKETS).unwrap();
    let heads = device.allocate(BUCKETS).unwrap();
    let status = device.allocate(4).unwrap();
    let bitmap = device.allocate(16384).unwrap();
    let output = device.allocate(4096).unwrap();
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &[37; 32], None);
    for (bitmap_word, blocks, capacity) in
        [(u32::MAX, 128, 2048), (1, 65, 2048), (u32::MAX, 128, 1)]
    {
        device.upload(&status, &[0; 4]).unwrap();
        device.upload(&bitmap, &[bitmap_word; 16384]).unwrap();
        let mut parameters = Parameters::default();
        parameters.pointers[2] = heads.address();
        parameters.pointers[3] = histogram.address();
        parameters.words[0] = 9;
        parameters.words[1] = BUCKETS as u32;
        device.launch(&parameters, BUCKETS.div_ceil(128)).unwrap();
        parameters.pointers[1] = output.address();
        parameters.pointers[5] = status.address();
        parameters.pointers[6] = bitmap.address();
        parameters.words[0] = 1;
        parameters.words[1] = blocks;
        parameters.words[3] = capacity;
        parameters.words[4] = (1 << 28) - 128;
        parameters.words[8..16].copy_from_slice(&context.input[4..12]);
        parameters.precompute_chacha_columns();
        device.launch(&parameters, 1).unwrap();
        let status_words = device.download(&status, 4).unwrap();
        assert_eq!(status_words[2], u32::from(capacity == 1));
        let written = status_words[0].min(capacity) as usize;
        let words = device.download(&output, written * 2).unwrap();
        let mut expected = vec![0_u32; BUCKETS];
        for node in words.as_chunks::<2>().0 {
            let value = (u64::from(node[0]) << 6) | u64::from(node[1] >> 26);
            expected[(value / 15113) as usize] += 1;
        }
        for (bucket, (actual, expected)) in device
            .download(&histogram, BUCKETS)
            .unwrap()
            .into_iter()
            .zip(expected)
            .enumerate()
        {
            assert_eq!(actual, expected, "F1 histogram bucket {bucket}");
        }
    }
}

#[test]
#[ignore = "requires a CUDA GPU"]
#[cfg(feature = "cuda")]
fn cuda_hashes_match_cpu() {
    hash_parity(&cuda::Device::new(0).unwrap());
}

#[test]
#[ignore = "requires a CUDA GPU"]
#[cfg(feature = "cuda")]
fn cuda_allocation_outlives_backend() {
    let device = cuda::Device::new(0).unwrap();
    let allocation = device.allocate(4).unwrap();
    let expected = [13, 29, 37, 41];
    device.upload(&allocation, &expected).unwrap();
    drop(device);
    let replacement = cuda::Device::new(0).unwrap();
    assert_eq!(replacement.download(&allocation, 4).unwrap(), expected);
    drop(allocation);
    hash_parity(&replacement);
}

#[test]
#[ignore = "requires a Vulkan GPU with buffer device addresses"]
#[cfg(feature = "vulkan")]
fn vulkan_hashes_match_cpu() {
    hash_parity(&vulkan::Device::new(0).unwrap());
}

fn gpu_logger(level: log::LevelFilter) {
    struct Logger;
    impl log::Log for Logger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            if record.target().contains("gigahorse_gpu") {
                eprintln!("{}", record.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: Logger = Logger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}

fn real_plot(engine: Engine) {
    let counter = std::env::var("GH_TEST_COUNTER")
        .map(|counter| counter.parse::<u64>().unwrap())
        .unwrap_or(1);
    let proof_index = std::env::var("GH_TEST_PROOF_INDEX")
        .map(|index| index.parse::<usize>().unwrap())
        .unwrap_or(0);
    real_plot_case(engine, counter, proof_index, false);
}

fn real_plot_case(
    mut engine: Engine,
    counter: u64,
    proof_index: usize,
    require_cross_bucket: bool,
) {
    gpu_logger(log::LevelFilter::Debug);
    use crate::gigahorse::{Gh3Reader, challenge_for};
    let path = std::env::var_os("GH_C30_PLOT").expect("set GH_C30_PLOT");
    let mut reader = Gh3Reader::new(std::fs::File::open(path).unwrap()).unwrap();
    let challenge = challenge_for(counter);
    let index = reader.matching_f7_indices(&challenge).unwrap()[proof_index];
    let started = std::time::Instant::now();
    let check = || {
        if started.elapsed().as_secs() > 60 {
            Err(Error::new(ErrorKind::TimedOut, "GPU test deadline"))
        } else {
            Ok(())
        }
    };
    let mut entries = Vec::new();
    let quality_buckets = reader
        .c30_quality_bucket_indices(index, &challenge)
        .unwrap();
    let mut quality_candidates = Vec::new();
    let mut bucket_entries = Vec::new();
    for bucket in reader.c30_bucket_indices(index).unwrap() {
        let bitmap = reader.c30_bitmap(bucket).unwrap();
        let reconstructed = engine
            .reconstruct(&reader.header().plot_id, &bitmap, 12 << 30, &check)
            .unwrap();
        if quality_buckets.contains(&bucket) {
            quality_candidates.extend(
                reconstructed
                    .iter()
                    .map(|entry| crate::gigahorse_cpu::c30_quality_candidate(entry, &challenge)),
            );
        }
        entries.extend_from_slice(&reconstructed);
        bucket_entries.push((bucket, reconstructed));
        eprintln!(
            "GPU bucket {bucket} completed at {:.3}s ({} F5 entries)",
            started.elapsed().as_secs_f64(),
            entries.len()
        );
    }
    entries.extend_from_within(..32);
    let proofs = engine.finish(entries, &challenge, &check).unwrap();
    let proof_elapsed = started.elapsed();
    assert_eq!(proofs.len(), 1);
    let mut cross_bucket_branches = 0;
    for proof in proofs {
        let quality =
            crate::verifier::validate_proof(&reader.header().plot_id, 32, &proof, &challenge)
                .unwrap();
        assert!(quality_candidates.contains(AsRef::<[u8; 32]>::as_ref(&quality)));
        for branch in 0..32 {
            let mut branch_challenge = challenge;
            branch_challenge[31] = (branch_challenge[31] & !31) | branch;
            let selected = reader
                .c30_quality_bucket_indices(index, &branch_challenge)
                .unwrap();
            cross_bucket_branches += usize::from(selected.len() == 2);
            let expected = crate::verifier::get_quality_string(
                32,
                &proof,
                u16::from(branch) * 2,
                &branch_challenge,
            )
            .unwrap();
            assert!(
                crate::gigahorse_cpu::c30_quality_entries_in_buckets(
                    bucket_entries
                        .iter()
                        .filter(|(bucket, _)| selected.contains(bucket))
                        .map(|(_, entries)| entries.as_slice()),
                    &branch_challenge,
                    &check,
                )
                .unwrap()
                .into_iter()
                .any(
                    |entry| crate::gigahorse_cpu::c30_quality_candidate(entry, &branch_challenge)
                        == *AsRef::<[u8; 32]>::as_ref(&expected)
                ),
                "missing quality branch {branch}"
            );
        }
    }
    if require_cross_bucket {
        assert!(
            cross_bucket_branches > 0,
            "missing two-bucket quality branch"
        );
    }
    eprintln!(
        "GPU full proof completed in {:.3}s",
        proof_elapsed.as_secs_f64()
    );
}

#[test]
#[ignore = "requires CUDA, GH_C30_PLOT and up to 12 GiB VRAM; release mode only"]
#[cfg(feature = "cuda")]
fn cuda_real_c30_proof() {
    real_plot(Engine::cuda(0).unwrap());
}

#[test]
#[ignore = "requires Vulkan, GH_C30_PLOT and up to 12 GiB VRAM; release mode only"]
#[cfg(feature = "vulkan")]
fn vulkan_real_c30_proof() {
    real_plot(Engine::vulkan(0).unwrap());
}

#[test]
#[ignore = "requires CUDA, GH_C30_PLOT and up to 12 GiB VRAM; release mode only"]
#[cfg(feature = "cuda")]
fn cuda_real_c30_cross_bucket_quality() {
    real_plot_case(Engine::cuda(0).unwrap(), 76, 0, true);
}

#[test]
#[ignore = "requires Vulkan, GH_C30_PLOT and up to 12 GiB VRAM; release mode only"]
#[cfg(feature = "vulkan")]
fn vulkan_real_c30_cross_bucket_quality() {
    real_plot_case(Engine::vulkan(0).unwrap(), 76, 0, true);
}

#[test]
#[ignore = "requires AMD/Vulkan, GH_C30_PLOT, release mode and external thermal monitoring"]
#[cfg(feature = "vulkan")]
fn vulkan_real_c30_farm_workload() {
    farm_workload(Engine::vulkan(0).unwrap());
}

#[test]
#[ignore = "requires CUDA, GH_C30_PLOT, release mode and external thermal monitoring"]
#[cfg(feature = "cuda")]
fn cuda_real_c30_farm_workload() {
    farm_workload(Engine::cuda(0).unwrap());
}

fn farm_workload(engine: Engine) {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap()
        .install(|| farm_workload_on_worker(engine));
}

fn farm_workload_on_worker(mut engine: Engine) {
    gpu_logger(log::LevelFilter::Info);
    use crate::gigahorse::{Gh3Reader, challenge_for};
    use crate::gigahorse_cpu::{c30_quality_candidate, c30_quality_entries_in_buckets};
    use dg_xch_core::consensus::constants::ConsensusConstants;
    use dg_xch_core::consensus::pot_iterations::calculate_iterations_quality;
    use std::collections::HashMap;

    let count = std::env::var("GH_BENCH_CHALLENGES")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(48);
    let first = std::env::var("GH_BENCH_COUNTER")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(0);
    assert!((1..=4096).contains(&count));
    let mut reader = Gh3Reader::new(
        std::fs::File::open(std::env::var_os("GH_C30_PLOT").expect("set GH_C30_PLOT")).unwrap(),
    )
    .unwrap();
    let plot_id = reader.header().plot_id;
    let constants = ConsensusConstants::default();
    let interval = constants.pool_sub_slot_iters / u64::from(constants.num_sps_sub_slot);
    let mut timings = Vec::new();
    let mut total_matches = 0;
    let mut total_buckets = 0;
    let mut full_recoveries = 0;
    let mut valid_partials = 0;
    let started = std::time::Instant::now();
    for counter in first..first.checked_add(count).unwrap() {
        let lookup_started = std::time::Instant::now();
        let check = || {
            if lookup_started.elapsed().as_secs() >= 20 {
                Err(Error::new(
                    ErrorKind::TimedOut,
                    "GigaHorse benchmark lookup deadline",
                ))
            } else {
                Ok(())
            }
        };
        let challenge = challenge_for(counter);
        let signage_hash = dg_xch_core::utils::hash_256(counter.to_be_bytes()).into();
        let eligible = |quality| {
            calculate_iterations_quality(
                constants.difficulty_constant_factor,
                quality,
                32,
                20_000,
                signage_hash,
            ) < interval
        };
        let indices = reader.matching_f7_indices(&challenge).unwrap();
        total_matches += indices.len();
        let mut cache = HashMap::new();
        for index in indices {
            let quality_buckets = reader
                .c30_quality_bucket_indices(index, &challenge)
                .unwrap();
            for bucket in &quality_buckets {
                if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(*bucket) {
                    let bitmap = reader.c30_bitmap(*bucket).unwrap();
                    entry.insert(
                        engine
                            .reconstruct(&plot_id, &bitmap, 12 << 30, &check)
                            .unwrap(),
                    );
                    total_buckets += 1;
                }
            }
            if !c30_quality_entries_in_buckets(
                quality_buckets
                    .iter()
                    .map(|bucket| cache[bucket].as_slice()),
                &challenge,
                &check,
            )
            .unwrap()
            .into_iter()
            .any(|entry| eligible(c30_quality_candidate(entry, &challenge).into()))
            {
                continue;
            }
            let mut entries = Vec::new();
            for bucket in reader.c30_bucket_indices(index).unwrap() {
                if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(bucket) {
                    let bitmap = reader.c30_bitmap(bucket).unwrap();
                    entry.insert(
                        engine
                            .reconstruct(&plot_id, &bitmap, 12 << 30, &check)
                            .unwrap(),
                    );
                    total_buckets += 1;
                }
                entries.extend_from_slice(&cache[&bucket]);
            }
            full_recoveries += 1;
            for proof in engine.finish(entries, &challenge, &check).unwrap() {
                let quality =
                    crate::verifier::validate_proof(&plot_id, 32, &proof, &challenge).unwrap();
                if eligible(quality) {
                    valid_partials += 1;
                }
            }
        }
        let elapsed = lookup_started.elapsed().as_secs_f64();
        timings.push(elapsed);
        eprintln!("GigaHorse filtered lookup {counter}: {elapsed:.4}s");
    }
    let elapsed = started.elapsed().as_secs_f64();
    timings.sort_by(f64::total_cmp);
    let rate = count as f64 / elapsed;
    let estimated_pib = rate * 9.375 * 256.0 * 43.3 / 1_048_576.0;
    eprintln!(
        "GigaHorse single-plot workload: lookups={count}, f7_matches={total_matches}, buckets={total_buckets}, full_recoveries={full_recoveries}, valid_partials={valid_partials}, seconds={elapsed:.3}, lookups_per_second={rate:.3}, p95={:.4}s, max={:.4}s, filter256_estimated_pib={estimated_pib:.3}, with_20_percent_headroom={:.3}",
        timings[(timings.len() - 1) * 95 / 100],
        timings.last().unwrap(),
        estimated_pib * 0.8
    );
    if std::env::var_os("GH_BENCH_REQUIRE_TARGET").is_some() {
        assert!(
            estimated_pib * 0.8 >= 0.51,
            "GigaHorse throughput target not reached"
        );
    }
}
