use crate::{PlotInfo, inspect_reader, invalid, read_u64};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_pos_common::finite_state_entropy::{
    decompress::{build_dtable, decompress_using_dtable},
    read_ncount,
};
use dg_xch_pos2::{
    ProofCore,
    chainer::{Chain, Chainer, SearchLimits},
    compute::{HashEngine, allocate, check_cancelled},
    params::{ProofParams, Range},
    plotting::PlotLimits,
    solver,
};
use std::fs::File;
use std::io::{Error, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};

pub struct PlotReader<R = File> {
    pub info: PlotInfo,
    input: R,
    offsets: Vec<u64>,
    params: ProofParams,
    memory_bytes: u64,
}

impl PlotReader<File> {
    pub fn open(path: &Path, testnet: bool, memory_bytes: u64) -> Result<Self, Error> {
        Self::from_reader(File::open(path)?, testnet, memory_bytes)
    }
}

impl<R: Read + Seek> PlotReader<R> {
    pub fn from_reader(mut input: R, testnet: bool, memory_bytes: u64) -> Result<Self, Error> {
        if memory_bytes < 1024 * 1024 {
            return Err(invalid("plot reader needs at least 1 MiB"));
        }
        let info = inspect_reader(&mut input)?;
        let params = ProofParams::new(info.plot_id.into(), info.k, info.strength, testnet)?;
        input.seek(SeekFrom::Start(
            43 + if info.portable { 112 } else { 128 } + 8,
        ))?;
        let mut offsets = allocate(info.chunks as usize)?;
        for _ in 0..info.chunks {
            offsets.push(read_u64(&mut input)?);
        }
        Ok(Self {
            info,
            input,
            offsets,
            params,
            memory_bytes,
        })
    }

    pub fn params(&self) -> &ProofParams {
        &self.params
    }

    fn chunk(&mut self, index: usize, cancelled: &AtomicBool) -> Result<Vec<u64>, Error> {
        check_cancelled(cancelled)?;
        let Some(offset) = self.offsets.get(index).copied() else {
            return Ok(Vec::new());
        };
        self.input.seek(SeekFrom::Start(offset))?;
        let size = read_u64(&mut self.input)?;
        let end = self
            .offsets
            .get(index + 1)
            .copied()
            .unwrap_or(self.info.file_bytes);
        if !(12..=64 * 1024 * 1024).contains(&size)
            || offset
                .checked_add(8)
                .and_then(|offset| offset.checked_add(size))
                != Some(end)
        {
            return Err(invalid("invalid chunk size"));
        }
        let mut header = [0u8; 12];
        self.input.read_exact(&mut header)?;
        let count = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let compressed_size =
            u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let stub_size = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        let stub_bits = usize::from(self.info.k - 2);
        if count > 1_048_576
            || stub_size != (count * stub_bits).div_ceil(8)
            || size != 12 + compressed_size as u64 + stub_size as u64
            || size + count as u64 * 9 + self.offsets.len() as u64 * 8 + 1024 * 1024
                > self.memory_bytes
        {
            return Err(invalid(
                "invalid chunk layout or reader memory budget exceeded",
            ));
        }
        if count == 0 {
            if compressed_size != 0 || stub_size != 0 {
                return Err(invalid("invalid empty chunk"));
            }
            return Ok(Vec::new());
        }
        if count < 3 || compressed_size < 2 {
            return Err(invalid("invalid FSE chunk payload"));
        }
        let mut compressed = allocate(compressed_size.max(512))?;
        compressed.resize(compressed_size.max(512), 0);
        self.input.read_exact(&mut compressed[..compressed_size])?;
        let mut normalized = [0i16; 256];
        let mut maximum = 255;
        let mut log = 0;
        let consumed = read_ncount(&mut normalized, &mut maximum, &mut log, &compressed)?;
        if consumed >= compressed_size
            || !(5..=14).contains(&log)
            || normalized
                .iter()
                .map(|count| i32::from(*count).unsigned_abs())
                .sum::<u32>()
                != 1u32 << log
        {
            return Err(invalid("invalid FSE probability table"));
        }
        let table = Arc::new(build_dtable(&normalized, maximum, log)?);
        let mut deltas = allocate(count)?;
        deltas.resize(count, 0u8);
        if decompress_using_dtable(
            &mut deltas,
            count,
            &compressed[consumed..compressed_size],
            compressed_size - consumed,
            table,
        )? != count
        {
            return Err(invalid("FSE chunk length mismatch"));
        }
        drop(compressed);
        let mut stubs = allocate(stub_size)?;
        stubs.resize(stub_size, 0u8);
        self.input.read_exact(&mut stubs)?;
        let mut fragments = allocate(count)?;
        let span_bits = u32::from(self.info.k) + 16;
        let mut previous = (index as u64) << span_bits;
        let mut buffer = 0u64;
        let mut pending = 0usize;
        let mut position = 0usize;
        for delta in deltas {
            while pending < stub_bits {
                let byte = stubs
                    .get(position)
                    .ok_or_else(|| invalid("truncated chunk stubs"))?;
                buffer |= u64::from(*byte) << pending;
                pending += 8;
                position += 1;
            }
            let stub = buffer & ((1u64 << stub_bits) - 1);
            buffer >>= stub_bits;
            pending -= stub_bits;
            previous = previous
                .checked_add((u64::from(delta) << stub_bits) | stub)
                .ok_or_else(|| invalid("fragment delta overflow"))?;
            if previous >> span_bits != index as u64 {
                return Err(invalid("fragment outside chunk range"));
            }
            fragments.push(previous);
        }
        if buffer != 0 {
            return Err(invalid("nonzero stub padding"));
        }
        check_cancelled(cancelled)?;
        Ok(fragments)
    }

    pub fn fragments_in_range(
        &mut self,
        range: Range,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u64>, Error> {
        if range.start > range.end {
            return Err(invalid("invalid fragment range"));
        }
        let span_bits = u32::from(self.info.k) + 16;
        let first = range.start >> span_bits;
        let last = (range.end >> span_bits).min(self.info.chunks.saturating_sub(1));
        let mut selected = Vec::new();
        for index in first..=last {
            let chunk = self.chunk(index as usize, cancelled)?;
            let start = chunk.partition_point(|fragment| *fragment < range.start);
            let end = chunk.partition_point(|fragment| *fragment <= range.end);
            if selected.len() + end - start > 4096 {
                return Err(invalid("challenge set exceeds search bound"));
            }
            selected
                .try_reserve_exact(end - start)
                .map_err(Error::other)?;
            selected.extend_from_slice(&chunk[start..end]);
        }
        selected.dedup();
        Ok(selected)
    }

    pub fn qualities(
        &mut self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<Chain>, Error> {
        let core = ProofCore::new(self.params.clone())?;
        let ranges = core.select_challenge_sets(challenge).ranges;
        let mut sets = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for (set, range) in sets.iter_mut().zip(ranges) {
            *set = self.fragments_in_range(range, cancelled)?;
        }
        Chainer::new(&core, challenge)
            .search(
                std::array::from_fn(|index| sets[index].as_slice()),
                limits,
                cancelled,
            )
            .map_err(|error| Error::other(format!("quality search failed: {error:?}")))
    }

    pub fn prove(
        &mut self,
        chain: &Chain,
        challenge: Bytes32,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        let mut engine = dg_xch_pos2::compute::CpuHasher::new(&self.params);
        self.prove_with_engine(chain, challenge, limits, cancelled, &mut engine)
    }

    pub fn prove_with_engine(
        &mut self,
        chain: &Chain,
        challenge: Bytes32,
        limits: PlotLimits,
        cancelled: &AtomicBool,
        engine: &mut impl HashEngine,
    ) -> Result<Vec<u8>, Error> {
        for fragment in chain.fragments {
            if self
                .fragments_in_range(
                    Range {
                        start: fragment,
                        end: fragment,
                    },
                    cancelled,
                )?
                .binary_search(&fragment)
                .is_err()
            {
                return Err(invalid("proof fragment absent from plot"));
            }
        }
        solver::solve_with_engine(&self.params, chain, challenge, limits, cancelled, engine)
    }
}
