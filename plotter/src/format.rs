use dg_xch_pos2::plotting::NativePlot;
use std::io::{Error, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) fn write_plot(
    output: &mut (impl Write + Seek),
    plot: &NativePlot,
    index: u16,
    meta_group: u8,
    memo: &[u8],
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let params = plot.params();
    let fragments = plot.witnesses();
    let last = fragments
        .last()
        .ok_or_else(|| Error::other("native plot contains no fragments"))?;
    output.write_all(b"pos2\x01")?;
    output.write_all(params.plot_id().as_ref())?;
    output.write_all(&[params.k(), params.strength()])?;
    output.write_all(&index.to_le_bytes())?;
    output.write_all(&[meta_group, memo.len() as u8])?;
    output.write_all(memo)?;
    let span_bits = u32::from(params.k()) + 16;
    let chunks = (last.fragment >> span_bits) + 1;
    output.write_all(&chunks.to_le_bytes())?;
    let directory = output.stream_position()?;
    for _ in 0..chunks {
        output.write_all(&0u64.to_le_bytes())?;
    }
    let stub_bits = u32::from(params.k()) - 2;
    let mut position = 0usize;
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
        let start = position;
        while position < fragments.len() && fragments[position].fragment >> span_bits == chunk {
            position += 1;
        }
        let values = &fragments[start..position];
        if values.is_empty() {
            output.write_all(&12u64.to_le_bytes())?;
            output.write_all(&[0; 12])?;
            continue;
        }
        if values.len() <= 2 {
            return Err(Error::other(
                "native FSE writer does not yet support one/two-entry chunks",
            ));
        }
        let mut deltas = Vec::with_capacity(values.len());
        let mut stubs = Vec::with_capacity((values.len() * stub_bits as usize).div_ceil(8));
        let mut previous = chunk << span_bits;
        let mut buffer = 0u64;
        let mut pending = 0u32;
        for witness in values {
            let delta = witness.fragment - previous;
            previous = witness.fragment;
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
        let compressed = dg_xch_pos_common::finite_state_entropy::encode::compress(&deltas)?;
        output.write_all(&(12 + compressed.len() as u64 + stubs.len() as u64).to_le_bytes())?;
        output.write_all(&(values.len() as u32).to_le_bytes())?;
        output.write_all(&(compressed.len() as u32).to_le_bytes())?;
        output.write_all(&(stubs.len() as u32).to_le_bytes())?;
        output.write_all(&compressed)?;
        output.write_all(&stubs)?;
    }
    Ok(())
}
