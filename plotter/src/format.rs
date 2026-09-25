use dg_xch_pos2::plotting::NativePlot;
use std::io::{Error, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

pub use dg_xch_pos2::compact::PackedChunk;

pub(crate) fn write_plot(
    output: &mut (impl Write + Seek),
    plot: &NativePlot,
    index: u16,
    meta_group: u8,
    memo: &[u8],
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let fragments = plot.witnesses();
    write_values(
        output,
        plot.params(),
        fragments.len(),
        |index| fragments[index].fragment,
        index,
        meta_group,
        memo,
        cancelled,
    )
}

pub fn write_compact(
    output: &mut (impl Write + Seek),
    plot: &dg_xch_pos2::compact::CompactPlot,
    index: u16,
    meta_group: u8,
    memo: &[u8],
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    write_values(
        output,
        plot.params(),
        plot.fragments().len(),
        |index| plot.fragments()[index],
        index,
        meta_group,
        memo,
        cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
fn write_values(
    output: &mut (impl Write + Seek),
    params: &dg_xch_pos2::params::ProofParams,
    count: usize,
    fragment: impl Fn(usize) -> u64,
    index: u16,
    meta_group: u8,
    memo: &[u8],
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let last = count
        .checked_sub(1)
        .ok_or_else(|| Error::other("native plot contains no fragments"))?;
    let span_bits = u32::from(params.k()) + 16;
    let chunks = (fragment(last) >> span_bits) + 1;
    let stub_bits = u32::from(params.k()) - 2;
    let mut position = 0usize;
    write_packed_chunks(
        output,
        params,
        chunks,
        index,
        meta_group,
        memo,
        cancelled,
        |chunk| {
            let start = position;
            while position < count && fragment(position) >> span_bits == chunk {
                position += 1;
            }
            let values = position - start;
            let mut deltas = Vec::with_capacity(values);
            let mut stubs = Vec::with_capacity((values * stub_bits as usize).div_ceil(8));
            let mut previous = chunk << span_bits;
            let mut buffer = 0u64;
            let mut pending = 0u32;
            for index in start..position {
                let value = fragment(index);
                let delta = value
                    .checked_sub(previous)
                    .ok_or_else(|| Error::other("unsorted plot fragments"))?;
                previous = value;
                deltas.push(
                    u8::try_from(delta >> stub_bits)
                        .map_err(|_| Error::other("fragment delta exceeds PoS2 chunk format"))?,
                );
                buffer |= (delta & ((1u64 << stub_bits) - 1)) << pending;
                pending += stub_bits;
                while pending >= 8 {
                    stubs.push(buffer as u8);
                    buffer >>= 8;
                    pending -= 8;
                }
            }
            if pending > 0 {
                stubs.push(buffer as u8);
            }
            Ok(PackedChunk {
                count: u32::try_from(values)
                    .map_err(|_| Error::other("plot chunk entry count overflow"))?,
                deltas,
                stubs,
            })
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub fn write_packed_chunks(
    output: &mut (impl Write + Seek),
    params: &dg_xch_pos2::params::ProofParams,
    chunks: u64,
    index: u16,
    meta_group: u8,
    memo: &[u8],
    cancelled: &AtomicBool,
    mut next_chunk: impl FnMut(u64) -> Result<PackedChunk, Error>,
) -> Result<(), Error> {
    if !matches!(memo.len(), 112 | 128) {
        return Err(Error::new(
            std::io::ErrorKind::InvalidInput,
            "plot memo must contain 112 or 128 bytes",
        ));
    }
    if chunks == 0 || chunks > 1u64 << (params.k() - 16) {
        return Err(Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid plot chunk count",
        ));
    }
    dg_xch_pos2::compute::check_cancelled(cancelled)?;
    output.write_all(b"pos2\x01")?;
    output.write_all(params.plot_id().as_ref())?;
    output.write_all(&[params.k(), params.strength()])?;
    output.write_all(&index.to_le_bytes())?;
    output.write_all(&[meta_group, memo.len() as u8])?;
    output.write_all(memo)?;
    output.write_all(&chunks.to_le_bytes())?;
    let directory = output.stream_position()?;
    for _ in 0..chunks {
        output.write_all(&0u64.to_le_bytes())?;
    }
    let stub_bits = u32::from(params.k()) - 2;
    for chunk in 0..chunks {
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::new(
                std::io::ErrorKind::Interrupted,
                "native plot writing cancelled",
            ));
        }
        let offset = output.stream_position()?;
        output.seek(SeekFrom::Start(directory + chunk * 8))?;
        output.write_all(&offset.to_le_bytes())?;
        output.seek(SeekFrom::Start(offset))?;
        let PackedChunk {
            count,
            deltas,
            stubs,
        } = next_chunk(chunk)?;
        dg_xch_pos2::compute::check_cancelled(cancelled)?;
        let values = count as usize;
        if values > 1_048_576
            || deltas.len() != values
            || stubs.len() != (values * stub_bits as usize).div_ceil(8)
        {
            return Err(Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid packed plot chunk layout",
            ));
        }
        if values == 0 {
            output.write_all(&12u64.to_le_bytes())?;
            output.write_all(&[0; 12])?;
            continue;
        }
        if values <= 2 {
            return Err(Error::other(
                "native FSE writer does not yet support one/two-entry chunks",
            ));
        }
        let compressed = dg_xch_pos_common::finite_state_entropy::encode::compress(&deltas)?;
        output.write_all(&(12 + compressed.len() as u64 + stubs.len() as u64).to_le_bytes())?;
        output.write_all(&(values as u32).to_le_bytes())?;
        output.write_all(&(compressed.len() as u32).to_le_bytes())?;
        output.write_all(&(stubs.len() as u32).to_le_bytes())?;
        output.write_all(&compressed)?;
        output.write_all(&stubs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn packed_writer_rejects_invalid_layout_and_cancellation() {
        let params = dg_xch_pos2::params::ProofParams::new([42; 32].into(), 28, 2, false).unwrap();
        for (count, deltas, stubs) in [(3, 2, 10), (3, 3, 9), (0, 1, 0), (1, 1, 4), (2, 2, 7)] {
            let mut output = std::io::Cursor::new(Vec::new());
            assert!(
                super::write_packed_chunks(
                    &mut output,
                    &params,
                    1,
                    0,
                    0,
                    &[0; 112],
                    &std::sync::atomic::AtomicBool::new(false),
                    |_| Ok(super::PackedChunk {
                        count,
                        deltas: vec![0; deltas],
                        stubs: vec![0; stubs],
                    }),
                )
                .is_err()
            );
        }
        for chunks in [0, 4097] {
            let mut output = std::io::Cursor::new(Vec::new());
            let error = super::write_packed_chunks(
                &mut output,
                &params,
                chunks,
                0,
                0,
                &[0; 112],
                &std::sync::atomic::AtomicBool::new(false),
                |_| panic!("invalid chunk count must be rejected before callback"),
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(output.get_ref().is_empty());
        }
        let mut output = std::io::Cursor::new(Vec::new());
        let error = super::write_packed_chunks(
            &mut output,
            &params,
            1,
            0,
            0,
            &[0; 112],
            &std::sync::atomic::AtomicBool::new(true),
            |_| panic!("cancelled writer must not request chunks"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(output.get_ref().is_empty());
    }

    #[test]
    fn k28_writer_rejects_invalid_memo_before_writing() {
        let params = dg_xch_pos2::params::ProofParams::new([42; 32].into(), 28, 2, false).unwrap();
        for length in [0, 111, 113, 127, 129, 256] {
            let mut output = std::io::Cursor::new(Vec::new());
            let error = super::write_values(
                &mut output,
                &params,
                3,
                |position| position as u64,
                0,
                0,
                &vec![0; length],
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(output.get_ref().is_empty());
        }
    }

    use super::write_values;
    use crate::{PlotRequest, PoolBinding, create_identity, reader::PlotReader};
    use blst::min_pk::SecretKey;
    use dg_xch_pos2::params::Range;
    use std::io::Cursor;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn synthetic_k32_format_round_trips_high_fragments_and_chunk_limit() {
        let cancelled = AtomicBool::new(false);
        let request = PlotRequest {
            farmer_public_key: SecretKey::key_gen_v3(&[7; 32], &[])
                .unwrap()
                .sk_to_pk()
                .to_bytes(),
            pool: PoolBinding::Contract([8; 32]),
            k: 32,
            strength: 2,
            index: u16::MAX,
            meta_group: u8::MAX,
            testnet: false,
        };
        let (params, memo) = create_identity(&request).unwrap();
        let span = 1u64 << 48;
        let occupied_chunks = [0u64, 0x7fff, 0x8000, 0xffff];
        let fragments: Vec<_> = occupied_chunks
            .into_iter()
            .flat_map(|chunk| {
                (0..2048u64).map(move |position| (chunk << 48) + (span - 1) * position / 2047)
            })
            .collect();
        assert_eq!(fragments.first(), Some(&0));
        assert_eq!(fragments.last(), Some(&u64::MAX));
        let mut output = Cursor::new(Vec::new());
        write_values(
            &mut output,
            &params,
            fragments.len(),
            |index| fragments[index],
            request.index,
            request.meta_group,
            &memo,
            &cancelled,
        )
        .unwrap();
        assert!(output.get_ref().len() < 4 * 1024 * 1024);
        let mut reader = PlotReader::from_reader(&mut output, false, 8 * 1024 * 1024).unwrap();
        assert_eq!(reader.info.chunks, 65_536);
        for (position, chunk) in occupied_chunks.into_iter().enumerate() {
            let start = chunk << 48;
            let actual = reader
                .fragments_in_range(
                    Range {
                        start,
                        end: start | (span - 1),
                    },
                    &cancelled,
                )
                .unwrap();
            assert_eq!(actual, fragments[position * 2048..(position + 1) * 2048]);
        }
        assert_eq!(
            reader
                .fragments_in_range(
                    Range {
                        start: (1u64 << 63) - 1,
                        end: 1u64 << 63,
                    },
                    &cancelled,
                )
                .unwrap(),
            [(1u64 << 63) - 1, 1u64 << 63]
        );
        assert_eq!(
            reader
                .fragments_in_range(
                    Range {
                        start: u64::MAX,
                        end: u64::MAX,
                    },
                    &cancelled,
                )
                .unwrap(),
            [u64::MAX]
        );
        assert!(
            reader
                .fragments_in_range(
                    Range {
                        start: span,
                        end: 2 * span - 1,
                    },
                    &cancelled,
                )
                .unwrap()
                .is_empty()
        );
        drop(reader);
        let chunk_count_offset = 43 + memo.len();
        output.get_mut()[chunk_count_offset..chunk_count_offset + 8]
            .copy_from_slice(&65_537u64.to_le_bytes());
        assert!(PlotReader::from_reader(output, false, 8 * 1024 * 1024).is_err());
    }
}
