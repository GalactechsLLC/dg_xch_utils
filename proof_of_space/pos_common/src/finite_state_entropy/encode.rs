use crate::finite_state_entropy::compress::{build_ctable, compress_using_ctable};
use std::io::Error;

pub(crate) fn header(counts: &[i16], log: u32) -> Vec<u8> {
    let mut bits = Vec::new();
    let mut append = |value: u32, width: u32| {
        for bit in 0..width {
            bits.push(((value >> bit) & 1) as u8);
        }
    };
    append(log - 5, 4);
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut width = log + 1;
    let mut symbol = 0;
    let mut previous_zero = false;
    while remaining > 1 && symbol < counts.len() {
        if previous_zero {
            let start = symbol;
            while symbol < counts.len() && counts[symbol] == 0 {
                symbol += 1;
            }
            let mut zeros = symbol - start;
            while zeros >= 24 {
                append(65535, 16);
                zeros -= 24;
            }
            while zeros >= 3 {
                append(3, 2);
                zeros -= 3;
            }
            append(zeros as u32, 2);
        }
        let count = i32::from(counts[symbol]);
        let cutoff = 2 * threshold - 1 - remaining;
        remaining -= count.abs();
        let mut encoded = count + 1;
        if encoded >= threshold {
            encoded += cutoff;
        }
        append(encoded as u32, width - u32::from(encoded < cutoff));
        previous_zero = count == 0;
        while remaining < threshold {
            width -= 1;
            threshold >>= 1;
        }
        symbol += 1;
    }
    let mut output = vec![0; bits.len().div_ceil(8)];
    for (index, bit) in bits.into_iter().enumerate() {
        output[index / 8] |= bit << (index % 8);
    }
    output
}

fn normalize_secondary(counts: &[u32], mut total: u64, log: u32) -> Result<Vec<i16>, Error> {
    let mut normalized = vec![-2i16; counts.len()];
    let low = total >> log;
    let mut low_one = (total * 3) >> (log + 1);
    let mut distributed = 0u64;
    for (index, count) in counts.iter().copied().enumerate() {
        if count == 0 {
            normalized[index] = 0;
        } else if u64::from(count) <= low {
            normalized[index] = -1;
            distributed += 1;
            total -= u64::from(count);
        } else if u64::from(count) <= low_one {
            normalized[index] = 1;
            distributed += 1;
            total -= u64::from(count);
        }
    }
    let mut remaining = (1u64 << log) - distributed;
    if remaining == 0 {
        return Ok(normalized);
    }
    if total / remaining > low_one {
        low_one = total * 3 / (remaining * 2);
        for (index, count) in counts.iter().copied().enumerate() {
            if normalized[index] == -2 && u64::from(count) <= low_one {
                normalized[index] = 1;
                distributed += 1;
                total -= u64::from(count);
            }
        }
        remaining = (1u64 << log) - distributed;
    }
    if distributed == counts.len() as u64 {
        let largest = counts
            .iter()
            .enumerate()
            .max_by_key(|(index, count)| (**count, std::cmp::Reverse(*index)))
            .unwrap()
            .0;
        normalized[largest] += remaining as i16;
        return Ok(normalized);
    }
    if total == 0 {
        while remaining > 0 {
            for probability in &mut normalized {
                if *probability > 0 && remaining > 0 {
                    *probability += 1;
                    remaining -= 1;
                }
            }
        }
        return Ok(normalized);
    }
    let scale = 62 - log;
    let midpoint = (1u64 << (scale - 1)) - 1;
    let step = (((1u64 << scale) * remaining) + midpoint) / total;
    let mut accumulator = midpoint;
    for (index, count) in counts.iter().copied().enumerate() {
        if normalized[index] == -2 {
            let end = accumulator + u64::from(count) * step;
            let weight = (end >> scale) - (accumulator >> scale);
            if weight == 0 {
                return Err(Error::other("FSE normalization produced zero weight"));
            }
            normalized[index] = weight as i16;
            accumulator = end;
        }
    }
    Ok(normalized)
}

fn normalize(counts: &[u32], total: u64, log: u32) -> Result<Vec<i16>, Error> {
    let rounding = [0u64, 473195, 504333, 520860, 550000, 700000, 750000, 830000];
    let scale = 62 - log;
    let step = (1u64 << 62) / total;
    let low = total >> log;
    let mut remaining = 1i32 << log;
    let mut normalized = vec![0i16; counts.len()];
    let mut largest = 0;
    for (index, count) in counts.iter().copied().enumerate() {
        if count == 0 {
            continue;
        }
        if u64::from(count) <= low {
            normalized[index] = -1;
            remaining -= 1;
            continue;
        }
        let scaled = u64::from(count) * step;
        let mut probability = (scaled >> scale) as i16;
        if probability < 8
            && scaled - ((probability as u64) << scale)
                > (1u64 << (scale - 20)) * rounding[probability as usize]
        {
            probability += 1;
        }
        if probability > normalized[largest] {
            largest = index;
        }
        normalized[index] = probability;
        remaining -= i32::from(probability);
    }
    if -remaining >= i32::from(normalized[largest] >> 1) {
        return normalize_secondary(counts, total, log);
    }
    normalized[largest] += remaining as i16;
    Ok(normalized)
}

pub fn compress(values: &[u8]) -> Result<Vec<u8>, Error> {
    if values.len() <= 2 || values.len() > u32::MAX as usize {
        return Err(Error::other("unsupported FSE input size"));
    }
    let mut counts = [0u32; 256];
    for value in values {
        counts[usize::from(*value)] += 1;
    }
    let maximum = values.iter().copied().max().unwrap();
    let largest = *counts.iter().max().unwrap() as usize;
    if largest == values.len() || largest == 1 || largest < values.len() >> 7 {
        return Err(Error::other(
            "reference PoS2 FSE cannot safely encode this distribution",
        ));
    }
    let highbit = |value: u32| 31 - value.leading_zeros();
    let minimum = (highbit(values.len() as u32) + 1).min(highbit(u32::from(maximum)) + 2);
    let log = 11u32
        .min(highbit(values.len() as u32 - 1).saturating_sub(2))
        .max(minimum)
        .clamp(5, 12);
    let counts = normalize(&counts[..=usize::from(maximum)], values.len() as u64, log)?;
    let mut output = header(&counts, log);
    output.extend(compress_using_ctable(
        values,
        &build_ctable(&counts, u32::from(maximum), log)?,
    )?);
    if output.len() >= values.len() - 1 {
        return Err(Error::other("reference PoS2 FSE input is incompressible"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finite_state_entropy::{
        decompress::{build_dtable, decompress_using_dtable},
        read_ncount,
    };
    use std::sync::Arc;

    #[test]
    fn uniform_header_round_trips() {
        let encoded = header(&[1; 256], 8);
        let mut counts = [0; 256];
        let mut maximum = 255;
        let mut log = 0;
        assert_eq!(
            read_ncount(&mut counts, &mut maximum, &mut log, &encoded).unwrap(),
            encoded.len()
        );
        assert_eq!(counts, [1; 256]);
        assert_eq!((maximum, log), (255, 8));
    }

    #[test]
    fn uniform_payload_round_trips() {
        let counts = [1i16; 256];
        let encoder = build_ctable(&counts, 255, 8).unwrap();
        let decoder = Arc::new(build_dtable(&counts, 255, 8).unwrap());
        for length in [3, 4, 7, 256, 65_537] {
            let values: Vec<u8> = (0..length).map(|index| (index * 73) as u8).collect();
            let encoded = compress_using_ctable(&values, &encoder).unwrap();
            let mut decoded = vec![0; length];
            assert_eq!(
                decompress_using_dtable(
                    &mut decoded,
                    length,
                    &encoded,
                    encoded.len(),
                    decoder.clone()
                )
                .unwrap(),
                length
            );
            assert_eq!(decoded, values);
        }
    }

    #[test]
    fn adaptive_entropy_round_trips_sparse_and_skewed_distributions() {
        for shift in [1, 3, 6, 9] {
            let source: Vec<u8> = (0..65_537u32)
                .map(|index| {
                    let value = index.wrapping_mul(2654435761);
                    if value >> shift & 15 == 0 {
                        (value >> 16) as u8
                    } else {
                        (value & 7) as u8
                    }
                })
                .collect();
            let encoded = compress(&source).unwrap();
            let mut counts = [0; 256];
            let mut maximum = 255;
            let mut log = 0;
            let size = read_ncount(&mut counts, &mut maximum, &mut log, &encoded).unwrap();
            assert_eq!(
                counts
                    .iter()
                    .map(|count| i32::from(*count).abs())
                    .sum::<i32>(),
                1 << log
            );
            let table = Arc::new(build_dtable(&counts, maximum, log).unwrap());
            let mut decoded = vec![0; source.len()];
            assert_eq!(
                decompress_using_dtable(
                    &mut decoded,
                    source.len(),
                    &encoded[size..],
                    encoded.len() - size,
                    table
                )
                .unwrap(),
                source.len()
            );
            assert_eq!(decoded, source);
        }
    }

    #[test]
    fn unsupported_reference_edge_cases_are_explicit_errors() {
        for input in [
            vec![],
            vec![1],
            vec![1, 2],
            vec![7; 100],
            (0..=255).collect(),
        ] {
            assert!(compress(&input).is_err());
        }
    }
}
