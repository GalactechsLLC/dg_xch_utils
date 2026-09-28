use crate::compute::{allocate, check_cancelled};
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;

const BUCKETS: usize = 256;
const SMALL_SORT_LIMIT: usize = 16_384;
const CANCEL_INTERVAL: usize = 4096;

pub(crate) fn sort<Entry, Key>(
    values: &mut Vec<Entry>,
    scratch: &mut Vec<Entry>,
    bits: u32,
    key: Key,
    cancelled: &AtomicBool,
) -> Result<(), Error>
where
    Entry: Copy + Default + Send + Sync,
    Key: Fn(&Entry) -> u64 + Sync,
{
    check_cancelled(cancelled)?;
    if bits == 0 || bits > 64 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid radix key width",
        ));
    }
    if values.len() <= SMALL_SORT_LIMIT {
        for (position, entry) in values.iter().enumerate() {
            if position.is_multiple_of(CANCEL_INTERVAL) {
                check_cancelled(cancelled)?;
            }
            if bits < 64 && key(entry) >> bits != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "radix key exceeds its declared width",
                ));
            }
        }
        values.par_sort_unstable_by_key(key);
        return check_cancelled(cancelled);
    }
    scratch
        .try_reserve_exact(values.len().saturating_sub(scratch.len()))
        .map_err(|_| Error::other("PoS2 radix scratch allocation failed"))?;
    scratch.truncate(values.len());
    while scratch.len() < values.len() {
        check_cancelled(cancelled)?;
        scratch.resize(
            scratch
                .len()
                .saturating_add(CANCEL_INTERVAL)
                .min(values.len()),
            Entry::default(),
        );
    }
    let workers = rayon::current_num_threads()
        .min(values.len().div_ceil(SMALL_SORT_LIMIT))
        .max(1);
    let chunk_size = values.len().div_ceil(workers);
    let chunk_count = values.len().div_ceil(chunk_size);
    let mut histograms = allocate(chunk_count)?;
    histograms.resize(chunk_count, [0usize; BUCKETS]);
    for shift in (0..bits).step_by(8) {
        check_cancelled(cancelled)?;
        histograms.fill([0; BUCKETS]);
        values
            .par_chunks(chunk_size)
            .zip(histograms.par_iter_mut())
            .try_for_each(|(entries, histogram)| -> Result<(), Error> {
                for (position, entry) in entries.iter().enumerate() {
                    if position.is_multiple_of(CANCEL_INTERVAL) {
                        check_cancelled(cancelled)?;
                    }
                    let entry_key = key(entry);
                    if shift == 0 && bits < 64 && entry_key >> bits != 0 {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "radix key exceeds its declared width",
                        ));
                    }
                    histogram[((entry_key >> shift) & 255) as usize] += 1;
                }
                Ok(())
            })?;
        let mut destinations = allocate(chunk_count)?;
        for _ in 0..chunk_count {
            destinations.push(allocate::<&mut [Entry]>(BUCKETS)?);
        }
        let mut remaining = scratch.as_mut_slice();
        for bucket in 0..BUCKETS {
            for (histogram, destination) in histograms.iter().zip(&mut destinations) {
                let (output, remainder) = remaining.split_at_mut(histogram[bucket]);
                destination.push(output);
                remaining = remainder;
            }
        }
        values
            .par_chunks(chunk_size)
            .zip(destinations.into_par_iter())
            .try_for_each(|(entries, mut destinations)| -> Result<(), Error> {
                let mut positions = [0usize; BUCKETS];
                for (position, entry) in entries.iter().enumerate() {
                    if position.is_multiple_of(CANCEL_INTERVAL) {
                        check_cancelled(cancelled)?;
                    }
                    let bucket = ((key(entry) >> shift) & 255) as usize;
                    let destination = destinations[bucket]
                        .get_mut(positions[bucket])
                        .ok_or_else(|| Error::other("PoS2 radix key changed during sorting"))?;
                    *destination = *entry;
                    positions[bucket] += 1;
                }
                Ok(())
            })?;
        std::mem::swap(values, scratch);
    }
    check_cancelled(cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct Entry {
        key: u64,
        original: u32,
    }

    #[test]
    fn k28_info_and_fragment_keys_match_standard_sort() {
        let cancelled = AtomicBool::new(false);
        let mut scratch = Vec::new();
        for bits in [28, 56, 64] {
            let mask = u64::MAX >> (64 - bits);
            let mut values: Vec<Entry> = (0..65_537u32)
                .map(|index| Entry {
                    key: match index % 7 {
                        0 => 0,
                        1 => mask,
                        2 => u64::from(index % 19),
                        _ => {
                            (u64::from(index).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                                ^ u64::from(index).rotate_left(33))
                                & mask
                        }
                    },
                    original: index,
                })
                .collect();
            let mut expected = values.clone();
            expected.sort_by_key(|entry| entry.key);
            sort(
                &mut values,
                &mut scratch,
                bits,
                |entry| entry.key,
                &cancelled,
            )
            .unwrap();
            assert_eq!(values, expected, "key width {bits}");
            assert_eq!(scratch.len(), values.len());
        }
    }

    #[test]
    fn small_radix_inputs_and_key_widths_are_checked() {
        let cancelled = AtomicBool::new(false);
        let mut scratch = Vec::new();
        for length in [0, 1, 7, 256, SMALL_SORT_LIMIT] {
            let mut values: Vec<u64> = (0..length as u64).rev().collect();
            sort(&mut values, &mut scratch, 28, |entry| *entry, &cancelled).unwrap();
            assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        for bits in [0, 65] {
            assert!(
                sort(
                    &mut vec![0u64],
                    &mut scratch,
                    bits,
                    |entry| *entry,
                    &cancelled
                )
                .is_err()
            );
        }
        for length in [1, SMALL_SORT_LIMIT + 1] {
            assert!(
                sort(
                    &mut vec![1u64 << 28; length],
                    &mut scratch,
                    28,
                    |entry| *entry,
                    &cancelled
                )
                .is_err()
            );
        }
    }

    #[test]
    fn radix_sort_observes_cancellation() {
        let mut values = vec![1u64, 0];
        assert!(
            sort(
                &mut values,
                &mut Vec::new(),
                28,
                |entry| *entry,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(values, [1, 0]);
    }
}
