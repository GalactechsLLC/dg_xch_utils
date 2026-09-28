use crate::chacha8::{ChachaContext, chacha8_get_keystream, chacha8_keysetup};
use crate::constants::{K_BC, L_TARGETS};
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::time::Instant;

#[derive(Clone)]
struct Node {
    f_value: u64,
    metadata: [u8; 16],
    children: [u32; 2],
}

struct CompactNode {
    f_value: u64,
    children: [u32; 2],
}

trait OutputNode: Send {
    fn from_node(node: Node) -> Self;
    fn f_value(&self) -> u64;
}

impl OutputNode for Node {
    fn from_node(node: Node) -> Self {
        node
    }
    fn f_value(&self) -> u64 {
        self.f_value
    }
}

impl OutputNode for CompactNode {
    fn from_node(node: Node) -> Self {
        Self {
            f_value: node.f_value,
            children: node.children,
        }
    }
    fn f_value(&self) -> u64 {
        self.f_value
    }
}

impl Entries for Vec<CompactNode> {
    fn len(&self) -> usize {
        self.len()
    }
    fn f_value(&self, index: usize) -> u64 {
        self[index].f_value
    }
    fn metadata(&self, index: usize) -> [u8; 16] {
        let mut metadata = [0; 16];
        metadata[..4].copy_from_slice(&self[index].children[0].to_be_bytes());
        metadata[4..8].copy_from_slice(&self[index].children[1].to_be_bytes());
        metadata
    }
    fn child(&self, index: usize) -> u32 {
        index as u32
    }
}

#[derive(Clone)]
pub struct C30Table5Entry {
    pub f_value: u64,
    pub metadata: [u8; 16],
    pub xs: [u32; 16],
}

pub fn c30_quality_candidate(entry: &C30Table5Entry, challenge: &[u8; 32]) -> [u8; 32] {
    let ordered = order_c30_xs(entry.xs);
    let index = usize::from(challenge[31] & 7) * 2;
    let mut input = [0; 40];
    input[..32].copy_from_slice(challenge);
    input[32..36].copy_from_slice(&ordered[index].to_be_bytes());
    input[36..].copy_from_slice(&ordered[index + 1].to_be_bytes());
    dg_xch_core::utils::hash_256(input)
}

fn order_c30_xs(mut ordered: [u32; 16]) -> [u32; 16] {
    for half in [1, 2, 4, 8] {
        for group in ordered.chunks_exact_mut(half * 2) {
            let (left, right) = group.split_at_mut(half);
            if left.iter().rev().cmp(right.iter().rev()).is_gt() {
                left.swap_with_slice(right);
            }
        }
    }
    ordered
}

pub fn c30_quality_entries<'entry>(
    entries: impl IntoIterator<Item = &'entry C30Table5Entry>,
    challenge: &[u8; 32],
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<&'entry C30Table5Entry>, Error> {
    c30_quality_entries_with_membership(
        entries.into_iter().map(|entry| (entry, 1)),
        false,
        challenge,
        check,
    )
}

pub fn c30_quality_entries_in_buckets<'entry>(
    buckets: impl IntoIterator<Item = &'entry [C30Table5Entry]>,
    challenge: &[u8; 32],
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<&'entry C30Table5Entry>, Error> {
    check()?;
    let mut groups = Vec::with_capacity(2);
    for bucket in buckets {
        check()?;
        if groups.len() == 2 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "GigaHorse quality needs one or two bucket groups",
            ));
        }
        groups.push(bucket);
    }
    if groups.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse quality needs one or two bucket groups",
        ));
    }
    let require_cross_bucket = groups.len() == 2;
    c30_quality_entries_with_membership(
        groups
            .into_iter()
            .enumerate()
            .flat_map(|(group, entries)| entries.iter().map(move |entry| (entry, 1_u8 << group))),
        require_cross_bucket,
        challenge,
        check,
    )
}

fn c30_quality_entries_with_membership<'entry>(
    entries: impl IntoIterator<Item = (&'entry C30Table5Entry, u8)>,
    require_cross_bucket: bool,
    challenge: &[u8; 32],
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<&'entry C30Table5Entry>, Error> {
    check()?;
    let mut candidates = Vec::new();
    for (index, (entry, membership)) in entries.into_iter().enumerate() {
        if index % 256 == 0 {
            check()?;
        }
        if entry.f_value >= 1 << 38 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Invalid GigaHorse F5 value",
            ));
        }
        candidates.push((entry, membership));
    }
    check()?;
    candidates.sort_unstable_by_key(|(entry, _)| (entry.f_value, entry.xs));
    candidates.dedup_by(|left, right| {
        if left.0.f_value == right.0.f_value && left.0.xs == right.0.xs {
            right.1 |= left.1;
            true
        } else {
            false
        }
    });
    check()?;
    let mut fifth = Vec::with_capacity(candidates.len());
    let mut ordered = Vec::with_capacity(candidates.len());
    for (index, (entry, _)) in candidates.iter().enumerate() {
        if index % 256 == 0 {
            check()?;
        }
        fifth.push(Node {
            f_value: entry.f_value,
            metadata: entry.metadata,
            children: [0; 2],
        });
        ordered.push(order_c30_xs(entry.xs));
    }
    let sixth: Vec<CompactNode> = next_table(&fifth, 6, check)?;
    let mut selected = vec![false; candidates.len()];
    let side = usize::from((challenge[31] >> 3) & 1);
    for (index, node) in sixth.iter().enumerate() {
        if index % 256 == 0 {
            check()?;
        }
        let mut children = node.children.map(|child| child as usize);
        if require_cross_bucket && (candidates[children[0]].1 | candidates[children[1]].1) != 3 {
            continue;
        }
        if ordered[children[0]]
            .iter()
            .rev()
            .cmp(ordered[children[1]].iter().rev())
            .is_gt()
        {
            children.swap(0, 1);
        }
        selected[children[side]] = true;
    }
    check()?;
    Ok(candidates
        .into_iter()
        .zip(selected)
        .filter_map(|((entry, _), selected)| selected.then_some(entry))
        .collect())
}

trait Entries: Sync {
    fn len(&self) -> usize;
    fn f_value(&self, index: usize) -> u64;
    fn metadata(&self, index: usize) -> [u8; 16];
    fn child(&self, index: usize) -> u32;
}

impl Entries for Vec<u64> {
    fn len(&self) -> usize {
        self.len()
    }
    fn f_value(&self, index: usize) -> u64 {
        (self[index] >> 32) << 6 | ((self[index] as u32) >> 26) as u64
    }
    fn metadata(&self, index: usize) -> [u8; 16] {
        let mut metadata = [0; 16];
        metadata[..4].copy_from_slice(&(self[index] as u32).to_be_bytes());
        metadata
    }
    fn child(&self, index: usize) -> u32 {
        self[index] as u32
    }
}

impl Entries for Vec<Node> {
    fn len(&self) -> usize {
        self.len()
    }
    fn f_value(&self, index: usize) -> u64 {
        self[index].f_value
    }
    fn metadata(&self, index: usize) -> [u8; 16] {
        self[index].metadata
    }
    fn child(&self, index: usize) -> u32 {
        index as u32
    }
}

pub(crate) fn evaluate(
    table: usize,
    f_value: u64,
    left: &[u8; 16],
    right: &[u8; 16],
) -> (u64, [u8; 16]) {
    let length = [0, 0, 4, 8, 16, 16, 12, 8][table];
    let mut input = [0_u8; 37];
    input[..5].copy_from_slice(&(f_value << 2).to_be_bytes()[3..]);
    for (index, byte) in left[..length].iter().chain(&right[..length]).enumerate() {
        input[index + 4] |= byte >> 6;
        input[index + 5] = byte << 2;
    }
    let hash = blake3::hash(&input[..5 + length * 2]);
    let bytes = hash.as_bytes();
    let output_y = u64::from_be_bytes(bytes[..8].try_into().unwrap()) >> 26;
    let mut metadata = [0; 16];
    if table < 4 {
        metadata[..length].copy_from_slice(&left[..length]);
        metadata[length..2 * length].copy_from_slice(&right[..length]);
    } else {
        let output_size = [0, 0, 0, 0, 16, 12, 8, 0][table];
        for index in 0..output_size {
            metadata[index] = (bytes[index + 4] << 6) | (bytes[index + 5] >> 2);
        }
    }
    (output_y, metadata)
}

fn next_table<Output: OutputNode>(
    entries: &impl Entries,
    table: usize,
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<Output>, Error> {
    check()?;
    if entries.len() == 0 {
        return Ok(Vec::new());
    }
    let started = Instant::now();
    let mut boundaries = vec![0];
    let mut previous = entries.f_value(0) / K_BC as u64;
    for index in 1..entries.len() {
        if index % (1 << 20) == 0 {
            check()?;
        }
        let bucket = entries.f_value(index) / K_BC as u64;
        if bucket != previous {
            boundaries.push(index);
            previous = bucket;
        }
    }
    boundaries.push(entries.len());
    let groups = boundaries.len().saturating_sub(2);
    let targets = &*L_TARGETS;
    let chunks: Vec<Vec<Output>> = (0..groups.div_ceil(1024))
        .into_par_iter()
        .map(|chunk| {
            check()?;
            let mut positions = vec![usize::MAX; K_BC];
            let mut counts = vec![0_usize; K_BC];
            let mut output = Vec::new();
            for group in chunk * 1024..((chunk + 1) * 1024).min(groups) {
                if group % 64 == 0 {
                    check()?;
                }
                let left_start = boundaries[group];
                let right_start = boundaries[group + 1];
                let right_end = boundaries[group + 2];
                let bucket = entries.f_value(left_start) / K_BC as u64;
                if entries.f_value(right_start) / K_BC as u64 != bucket + 1 {
                    continue;
                }
                let right_base = (bucket + 1) * K_BC as u64;
                for index in right_start..right_end {
                    if (index - right_start) % 4096 == 0 {
                        check()?;
                    }
                    let remainder = (entries.f_value(index) - right_base) as usize;
                    if counts[remainder] == 0 {
                        positions[remainder] = index;
                    }
                    counts[remainder] += 1;
                }
                for left_index in left_start..right_start {
                    if (left_index - left_start) % 256 == 0 {
                        check()?;
                    }
                    let left_y = entries.f_value(left_index);
                    let remainder = (left_y - bucket * K_BC as u64) as usize;
                    for target in &targets[(bucket & 1) as usize][remainder] {
                        let target = usize::from(*target);
                        for duplicate in 0..counts[target] {
                            if output.len() % 4096 == 4095 {
                                check()?;
                            }
                            let right_index = positions[target] + duplicate;
                            let (f_value, metadata) = evaluate(
                                table,
                                left_y,
                                &entries.metadata(left_index),
                                &entries.metadata(right_index),
                            );
                            output.push(Output::from_node(Node {
                                f_value,
                                metadata,
                                children: [entries.child(left_index), entries.child(right_index)],
                            }));
                        }
                    }
                }
                for index in right_start..right_end {
                    if (index - right_start) % 4096 == 0 {
                        check()?;
                    }
                    counts[(entries.f_value(index) - right_base) as usize] = 0;
                }
            }
            Ok(output)
        })
        .collect::<Result<_, Error>>()?;
    check()?;
    let mut result = flatten_chunks(chunks);
    result.par_sort_unstable_by_key(OutputNode::f_value);
    check()?;
    log::debug!(
        "CPU F{table}: {} entries in {:.1}s",
        result.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(result)
}

fn flatten_chunks<Entry>(chunks: Vec<Vec<Entry>>) -> Vec<Entry> {
    let mut result = Vec::with_capacity(chunks.iter().map(Vec::len).sum());
    for mut chunk in chunks {
        result.append(&mut chunk);
    }
    result
}

pub fn reconstruct_c30_table5(
    plot_id: &[u8; 32],
    bitmap: &[u64],
) -> Result<Vec<C30Table5Entry>, Error> {
    reconstruct_c30_table5_checked(plot_id, bitmap, &|| Ok(()))
}

pub fn reconstruct_c30_table5_checked(
    plot_id: &[u8; 32],
    bitmap: &[u64],
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<C30Table5Entry>, Error> {
    check()?;
    if bitmap.len() != 8192 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "C30 bitmap requires 524288 bits",
        ));
    }
    let selected_count: u32 = bitmap.iter().map(|word| word.count_ones()).sum();
    if selected_count == 0 {
        return Ok(Vec::new());
    }
    if selected_count > 32768 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "C30 bitmap exceeds the reconstruction resource limit",
        ));
    }
    let started = Instant::now();
    let mut key = [0; 32];
    key[0] = 1;
    key[1..].copy_from_slice(&plot_id[..31]);
    let mut context = ChachaContext { input: [0; 16] };
    chacha8_keysetup(&mut context, &key, None);
    let selected = |index: u32| (bitmap[index as usize / 64] >> (index % 64)) & 1 != 0;
    let chunks: Vec<Vec<u64>> = (0..4096_u64)
        .into_par_iter()
        .map(|chunk| {
            check()?;
            let mut stream = Vec::with_capacity(1 << 22);
            chacha8_get_keystream(&context, chunk << 16, 1 << 16, &mut stream);
            let mut retained = Vec::new();
            for (offset, bytes) in stream.as_chunks::<4>().0.iter().enumerate() {
                let hash = u32::from_be_bytes(*bytes);
                let index = hash >> 13;
                if selected(index) || (index > 0 && hash & 8191 < 512 && selected(index - 1)) {
                    let proof_x = (chunk << 20) + offset as u64;
                    retained.push(u64::from(hash) << 32 | proof_x);
                }
            }
            Ok(retained)
        })
        .collect::<Result<_, Error>>()?;
    check()?;
    let mut first = flatten_chunks(chunks);
    log::debug!(
        "CPU F1 scan: {} entries in {:.1}s",
        first.len(),
        started.elapsed().as_secs_f64()
    );
    first.par_sort_unstable();
    check()?;
    log::debug!(
        "CPU F1 sorted after {:.1}s",
        started.elapsed().as_secs_f64()
    );
    let second: Vec<CompactNode> = next_table(&first, 2, check)?;
    drop(first);
    let third: Vec<Node> = next_table(&second, 3, check)?;
    let fourth: Vec<Node> = next_table(&third, 4, check)?;
    let fifth: Vec<Node> = next_table(&fourth, 5, check)?;
    let mut result = Vec::with_capacity(fifth.len());
    for node in fifth {
        let mut xs = Vec::with_capacity(16);
        for fourth_index in node.children {
            for third_index in fourth[fourth_index as usize].children {
                for second_index in third[third_index as usize].children {
                    xs.extend_from_slice(&second[second_index as usize].children);
                }
            }
        }
        result.push(C30Table5Entry {
            f_value: node.f_value,
            metadata: node.metadata,
            xs: xs.try_into().unwrap(),
        });
    }
    Ok(result)
}

pub fn finish_c30_proofs(mut entries: Vec<C30Table5Entry>, challenge: &[u8; 32]) -> Vec<Vec<u8>> {
    entries.sort_unstable_by_key(|entry| (entry.f_value, entry.xs));
    entries.dedup_by(|left, right| left.f_value == right.f_value && left.xs == right.xs);
    let fifth: Vec<_> = entries
        .iter()
        .map(|entry| Node {
            f_value: entry.f_value,
            metadata: entry.metadata,
            children: [0; 2],
        })
        .collect();
    let sixth: Vec<Node> = next_table(&fifth, 6, &|| Ok(())).unwrap();
    let seventh: Vec<Node> = next_table(&sixth, 7, &|| Ok(())).unwrap();
    let target = u32::from_be_bytes(challenge[..4].try_into().unwrap());
    seventh
        .into_iter()
        .filter(|node| (node.f_value >> 6) as u32 == target)
        .map(|node| {
            let mut proof = Vec::with_capacity(256);
            for sixth_index in node.children {
                for fifth_index in sixth[sixth_index as usize].children {
                    for proof_x in entries[fifth_index as usize].xs {
                        proof.extend_from_slice(&proof_x.to_be_bytes());
                    }
                }
            }
            proof
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quality_filter_pair() -> Vec<C30Table5Entry> {
        vec![
            C30Table5Entry {
                f_value: 0,
                metadata: [0; 16],
                xs: [9; 16],
            },
            C30Table5Entry {
                f_value: K_BC as u64,
                metadata: [0; 16],
                xs: [3; 16],
            },
        ]
    }

    #[test]
    fn grouped_quality_filter_validates_bucket_count_and_empty_groups() {
        let empty: &[C30Table5Entry] = &[];
        for buckets in [vec![], vec![empty; 3]] {
            let error = c30_quality_entries_in_buckets(buckets, &[0; 32], &|| Ok(()))
                .err()
                .unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
        for buckets in [vec![empty], vec![empty; 2]] {
            assert!(
                c30_quality_entries_in_buckets(buckets, &[0; 32], &|| Ok(()))
                    .unwrap()
                    .is_empty()
            );
        }
        let entries = quality_filter_pair();
        assert_eq!(
            c30_quality_entries_in_buckets([entries.as_slice()], &[0; 32], &|| Ok(()))
                .unwrap()
                .len(),
            1
        );
        for buckets in [[entries.as_slice(), empty], [empty, entries.as_slice()]] {
            assert!(
                c30_quality_entries_in_buckets(buckets, &[0; 32], &|| Ok(()))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn grouped_quality_filter_excludes_intra_bucket_matches() {
        let entries = quality_filter_pair();
        let unrelated = [C30Table5Entry {
            f_value: 8 * K_BC as u64,
            metadata: [0; 16],
            xs: [1; 16],
        }];
        assert!(
            c30_quality_entries_in_buckets(
                [entries.as_slice(), unrelated.as_slice()],
                &[0; 32],
                &|| Ok(()),
            )
            .unwrap()
            .is_empty()
        );
        let cross = [C30Table5Entry {
            xs: [5; 16],
            ..entries[1].clone()
        }];
        for buckets in [
            [entries.as_slice(), cross.as_slice()],
            [cross.as_slice(), entries.as_slice()],
        ] {
            for side in [0, 8] {
                let mut challenge = [0; 32];
                challenge[31] = side;
                let filtered =
                    c30_quality_entries_in_buckets(buckets, &challenge, &|| Ok(())).unwrap();
                assert_eq!(filtered.len(), 1);
                assert_eq!(filtered[0].xs, [if side == 0 { 5 } else { 9 }; 16]);
            }
        }
    }

    #[test]
    fn grouped_quality_filter_preserves_overlapping_membership() {
        let entries = quality_filter_pair();
        let mut duplicates = entries.clone();
        duplicates.extend(entries.clone());
        for buckets in [
            [entries.as_slice(), &entries[..1]],
            [entries.as_slice(), &entries[1..]],
            [&entries[..1], entries.as_slice()],
            [&entries[1..], entries.as_slice()],
            [entries.as_slice(), entries.as_slice()],
            [duplicates.as_slice(), duplicates.as_slice()],
        ] {
            let filtered = c30_quality_entries_in_buckets(buckets, &[0; 32], &|| Ok(())).unwrap();
            assert_eq!(filtered.len(), 1);
            assert_eq!(filtered[0].xs, [3; 16]);
        }
    }

    #[test]
    fn grouped_quality_filter_checks_cancellation_throughout() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entries = quality_filter_pair();
        let buckets = [&entries[..1], &entries[1..]];
        let checks = AtomicUsize::new(0);
        c30_quality_entries_in_buckets(buckets, &[0; 32], &|| {
            checks.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();
        for limit in 0..checks.load(Ordering::Relaxed) {
            let checks = AtomicUsize::new(0);
            let error = c30_quality_entries_in_buckets(buckets, &[0; 32], &|| {
                if checks.fetch_add(1, Ordering::Relaxed) >= limit {
                    Err(Error::new(ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert_eq!(error.kind(), ErrorKind::Interrupted);
        }
    }

    #[test]
    fn quality_filter_orders_children_and_preserves_distinct_duplicates() {
        let mut entries = quality_filter_pair();
        entries.extend(entries.clone());
        let filtered = c30_quality_entries(&entries, &[0; 32], &|| Ok(())).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].xs, [3; 16]);
        entries.push(C30Table5Entry {
            f_value: K_BC as u64,
            metadata: [0; 16],
            xs: [5; 16],
        });
        let filtered = c30_quality_entries(&entries, &[0; 32], &|| Ok(())).unwrap();
        assert_eq!(
            filtered.iter().map(|entry| entry.xs[0]).collect::<Vec<_>>(),
            [3, 5]
        );
        let mut challenge = [0; 32];
        challenge[31] = 8;
        let filtered = c30_quality_entries(&entries, &challenge, &|| Ok(())).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].xs, [9; 16]);
    }

    #[test]
    fn quality_filter_requires_matching_neighbors_across_input_slices() {
        let mut entries = quality_filter_pair();
        assert!(
            c30_quality_entries(&entries[..1], &[0; 32], &|| Ok(()))
                .unwrap()
                .is_empty()
        );
        assert!(
            c30_quality_entries(&entries[1..], &[0; 32], &|| Ok(()))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            c30_quality_entries(entries[..1].iter().chain(&entries[1..]), &[0; 32], &|| Ok(
                ()
            ))
            .unwrap()
            .len(),
            1
        );
        entries[1].f_value = 2 * K_BC as u64;
        assert!(
            c30_quality_entries(&entries, &[0; 32], &|| Ok(()))
                .unwrap()
                .is_empty()
        );
        entries[1].f_value = K_BC as u64 + 126;
        assert!(
            c30_quality_entries(&entries, &[0; 32], &|| Ok(()))
                .unwrap()
                .is_empty()
        );
        assert!(
            c30_quality_entries(std::iter::empty(), &[0; 32], &|| Ok(()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn quality_filter_rejects_out_of_range_f5_values() {
        for f_value in [1_u64 << 38, u64::MAX] {
            let entry = C30Table5Entry {
                f_value,
                metadata: [0; 16],
                xs: [0; 16],
            };
            let error = c30_quality_entries([&entry], &[0; 32], &|| Ok(()))
                .err()
                .unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
        }
        let entry = C30Table5Entry {
            f_value: (1_u64 << 38) - 1,
            metadata: [0; 16],
            xs: [0; 16],
        };
        assert!(
            c30_quality_entries([&entry], &[0; 32], &|| Ok(()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn quality_filter_checks_cancellation_throughout() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let entries = quality_filter_pair();
        let checks = AtomicUsize::new(0);
        c30_quality_entries(&entries, &[0; 32], &|| {
            checks.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();
        for limit in 0..checks.load(Ordering::Relaxed) {
            let checks = AtomicUsize::new(0);
            let error = c30_quality_entries(&entries, &[0; 32], &|| {
                if checks.fetch_add(1, Ordering::Relaxed) >= limit {
                    Err(Error::new(ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert_eq!(error.kind(), ErrorKind::Interrupted);
        }
    }

    #[test]
    fn cancellation_interrupts_a_dense_duplicate_chain() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountedEntries {
            metadata_reads: AtomicUsize,
        }

        impl Entries for CountedEntries {
            fn len(&self) -> usize {
                16385
            }
            fn f_value(&self, index: usize) -> u64 {
                if index == 0 { 0 } else { K_BC as u64 }
            }
            fn metadata(&self, _index: usize) -> [u8; 16] {
                self.metadata_reads.fetch_add(1, Ordering::Relaxed);
                [0; 16]
            }
            fn child(&self, index: usize) -> u32 {
                index as u32
            }
        }

        let entries = CountedEntries {
            metadata_reads: AtomicUsize::new(0),
        };
        let error = next_table::<CompactNode>(&entries, 6, &|| {
            if entries.metadata_reads.load(Ordering::Relaxed) != 0 {
                Err(Error::new(ErrorKind::Interrupted, "cancelled"))
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        assert!((2..=8192).contains(&entries.metadata_reads.load(Ordering::Relaxed)));
    }

    #[test]
    fn compact_second_table_preserves_metadata() {
        assert_eq!(std::mem::size_of::<CompactNode>(), 16);
        let entries = vec![CompactNode {
            f_value: 123,
            children: [42, u32::MAX],
        }];
        let metadata = entries.metadata(0);
        assert_eq!(&metadata[..4], &42_u32.to_be_bytes());
        assert_eq!(&metadata[4..8], &u32::MAX.to_be_bytes());
        assert_eq!(&metadata[8..], &[0; 8]);
        assert_eq!(entries.child(0), 0);
    }

    #[test]
    fn cancellation_stops_before_allocating() {
        let error = reconstruct_c30_table5_checked(&[0; 32], &[0; 8192], &|| {
            Err(Error::new(ErrorKind::Interrupted, "cancelled"))
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
    }

    #[test]
    fn cancellation_interrupts_an_active_scan() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let checks = AtomicUsize::new(0);
        let mut bitmap = [0; 8192];
        bitmap[0] = 1;
        let workers = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let result = workers.install(|| {
            reconstruct_c30_table5_checked(&[0; 32], &bitmap, &|| {
                if checks.fetch_add(1, Ordering::Relaxed) >= 4 {
                    Err(Error::new(ErrorKind::Interrupted, "cancelled during scan"))
                } else {
                    Ok(())
                }
            })
        });
        assert_eq!(result.err().unwrap().kind(), ErrorKind::Interrupted);
    }

    #[test]
    fn rejects_invalid_or_oversized_bitmaps() {
        assert!(reconstruct_c30_table5(&[0; 32], &[]).is_err());
        assert!(reconstruct_c30_table5(&[0; 32], &[u64::MAX; 8192]).is_err());
        assert!(
            reconstruct_c30_table5(&[0; 32], &[0; 8192])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn handles_empty_proof_candidates() {
        assert!(finish_c30_proofs(Vec::new(), &[0; 32]).is_empty());
    }
}
