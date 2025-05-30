use crate::constants::{
    ucdiv, ucdiv64, ucdiv_t, HEADER_MAGIC, HEADER_V2_MAGIC, K_C3R, K_CHECKPOINT1INTERVAL,
    K_CHECKPOINT2INTERVAL, K_ENTRIES_PER_PARK, K_RVALUES, K_STUB_MINUS_BITS,
};
use crate::encoding;
use crate::encoding::{ans_decode_deltas, line_point_to_square, line_point_to_square64};
use crate::entry_sizes::EntrySizes;
use crate::finite_state_entropy::decompress::{decompress_using_dtable, DTable};
use crate::plots::compression::{create_compression_dtable, get_compression_info_for_level};
use crate::plots::decompressor::{
    CompressedQualitiesRequest, Decompressor, DecompressorPool, LinePoint,
};
use crate::plots::fx_generator::F1Generator;
use crate::plots::PROOF_X_COUNT;
use crate::utils::bit_reader::BitReader;
use crate::utils::{bytes_to_u64, open_read_only, open_read_only_async, slice_u128from_bytes};
use crate::verifier::get_f7_from_proof_and_reorder;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::plots::{PlotFile, PlotHeader, PlotHeaderV1, PlotHeaderV2, PlotMemo, PlotTable};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use hex::encode;
use log::{debug, error, warn};
use rustc_hash::FxHashSet;
use std::cmp::{max, min};
use std::ffi::OsStr;
use std::fmt::Display;
use std::io::{Error, ErrorKind, Seek, SeekFrom};
use std::marker::PhantomData;
use std::mem::{size_of, swap};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::available_parallelism;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};
use tokio::sync::Mutex;

const CHIA_QUALITY_SIZE: usize = 32;
const HASH_SIZE_MAX: usize = CHIA_QUALITY_SIZE + ucdiv_t(2 * 50, 8);

pub struct LinePointParkComponents {
    base_line_point: u128,
    stubs: Vec<u8>,
    deltas: Vec<u8>,
}
#[derive(Debug)]
pub struct PlotReader<
    F: AsyncSeek + AsyncRead + AsyncSeekExt + AsyncReadExt + Unpin,
    T: for<'a> PlotFile<'a, F> + Display,
> {
    proof_decompressor: Option<Arc<DecompressorPool>>,
    quality_decompressor: Option<Arc<DecompressorPool>>,
    c2_entries: Vec<u64>,
    file: T,
    _phantom_data: PhantomData<F>,
    last_park: Mutex<usize>,
    pub p7_entries: Mutex<Vec<u64>>,
    fx: Mutex<Vec<u64>>,
    meta: Mutex<Vec<BitReader>>,
    f1_generator: Arc<F1Generator>,
}
impl<
        F: AsyncSeek + AsyncRead + AsyncSeekExt + AsyncReadExt + Unpin,
        T: for<'a> PlotFile<'a, F> + Display,
    > PlotReader<F, T>
{
    #[allow(clippy::cast_possible_truncation)]
    pub async fn new(
        t: T,
        proof_decompressor: Option<Arc<DecompressorPool>>,
        quality_decompressor: Option<Arc<DecompressorPool>>,
    ) -> Result<Self, Error> {
        let f1_generator = Arc::new(F1Generator::new(
            t.k(),
            available_parallelism()
                .unwrap_or_else(|_| {
                    NonZeroUsize::new(8).expect("Safe Value Expected for Non Zero Usize")
                })
                .get() as u8,
            &t.plot_id().bytes(),
        ));
        let mut reader = Self {
            proof_decompressor,
            quality_decompressor,
            c2_entries: vec![],
            file: t,
            _phantom_data: PhantomData,
            last_park: Mutex::new(usize::MAX),
            p7_entries: Mutex::new(vec![0u64; K_ENTRIES_PER_PARK as usize]),
            fx: Mutex::new(vec![0u64; PROOF_X_COUNT]),
            meta: Mutex::new(Vec::with_capacity(PROOF_X_COUNT)),
            f1_generator,
        };
        reader.load_c2entries().await?;
        Ok(reader)
    }

    pub fn get_c3_park_count(&self) -> u64 {
        // We know how many C3 parks there are by how many
        // entries we have in the C1 table - 1 (extra 0 entry added)
        // However, to make sure this is the case, we'll have to
        // read-in all C1 entries and ensure we hit an empty one,
        // to ensure we don't run into dead/alignment-space
        self.get_maximum_c1_entries()
        // Or just do this:
        //  Same thing, but we use it
        //  because we want to validate the plot for farming,
        //  and farming goes to C1 tables before it goes to C3
        // let c3_park_size  = calculate_c3size();
        // let c3_table_size = self.file.table_size(PlotTable::C3);
        // let c3_park_count = c3_table_size / c3_park_size;
    }

    pub fn get_max_f7entry_count(&self) -> u64 {
        self.get_c3_park_count() * u64::from(K_CHECKPOINT1INTERVAL)
    }

    pub fn get_lowest_stored_table(&self) -> PlotTable {
        match self.header() {
            PlotHeader::V1(_) | PlotHeader::GHv2_5(_) => PlotTable::Table1,
            PlotHeader::V2(h) => {
                if h.compression_level == 0 {
                    PlotTable::Table1
                } else if h.compression_level >= 9 {
                    PlotTable::Table3
                } else {
                    PlotTable::Table2
                }
            }
        }
    }

    pub fn is_compressed_table(&self, table: PlotTable) -> bool {
        match self.header() {
            PlotHeader::V1(_) | PlotHeader::GHv2_5(_) => false,
            PlotHeader::V2(h) => {
                if h.compression_level == 0 {
                    false
                } else {
                    table == self.get_lowest_stored_table()
                }
            }
        }
    }

    pub fn get_park_size_for_table(&self, table: PlotTable) -> u64 {
        if self.is_compressed_table(table) {
            get_compression_info_for_level(self.plot_file().compression_level()).table_park_size
                as u64
        } else if (table as u8) < self.get_lowest_stored_table() as u8 {
            0
        } else {
            u64::from(EntrySizes::calculate_park_size(
                table,
                u32::from(self.plot_file().k()),
            ))
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn get_table_park_count(&self, table: PlotTable) -> usize {
        match table {
            PlotTable::C1 | PlotTable::C2 => 0,
            PlotTable::C3 => self.get_c3_park_count() as usize,
            PlotTable::Table1
            | PlotTable::Table2
            | PlotTable::Table3
            | PlotTable::Table4
            | PlotTable::Table5
            | PlotTable::Table6 => {
                self.file.table_size(table) as usize
                    / EntrySizes::calculate_park_size(table, u32::from(self.file.k())) as usize
            }
            PlotTable::Table7 => {
                ucdiv64(self.get_max_f7entry_count(), u64::from(K_ENTRIES_PER_PARK)) as usize
            }
        }
    }

    pub fn get_maximum_c1_entries(&self) -> u64 {
        let c1table_size = self.file.table_size(PlotTable::C1);
        let f7size = ucdiv64(u64::from(self.file.k()), 8);
        let c3park_count = max(c1table_size / f7size, 1);
        // -1 because an extra 0 entry is added at the end
        c3park_count - 1
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn get_actual_c1_entry_count(&self) -> Result<u64, Error> {
        let max_c1entries = self.get_max_f7entry_count();
        if max_c1entries < 1 {
            return Ok(0);
        }
        let f7size_bytes = ucdiv64(u64::from(self.file.k()), 8);
        let c1address = self.file.table_address(PlotTable::C1);
        let c1table_size = self.file.table_size(PlotTable::C1);
        let mut c1read_address = c1address + c1table_size - f7size_bytes;
        // Read entries from the end of the table until the start, until we find an entry that is
        // not zero/higher than the previous one
        {
            let file = self.file.file().clone();
            let mut file_lock = file.lock().await;
            file_lock.seek(SeekFrom::Start(c1read_address)).await?;
            let c1 = 0;
            let mut c1_entry_bytes = vec![0; f7size_bytes as usize];
            let mut u64_buffer = [0u8; 8];
            while c1read_address >= c1address {
                file_lock.read_exact(&mut c1_entry_bytes).await?;
                for (i, b) in c1_entry_bytes.iter().take(size_of::<u64>()).enumerate() {
                    if (f7size_bytes as usize) < size_of::<u64>() {
                        u64_buffer[i + size_of::<u64>() - f7size_bytes as usize] = *b;
                    } else {
                        u64_buffer[i] = *b;
                    }
                }
                let c1_entry = u64::from_be_bytes(u64_buffer);
                if c1_entry > c1 {
                    break;
                }
                if c1read_address <= c1address {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Failed to read c1 entry",
                    ));
                }
                c1read_address -= f7size_bytes;
            }
        }
        Ok((c1read_address - c1address) / f7size_bytes)
    }
    #[allow(clippy::cast_possible_truncation)]
    pub async fn read_c3park(&self, park_index: u64) -> Result<Vec<u64>, Error> {
        let f7size_bytes: u64 = ucdiv64(u64::from(self.file.k()), 8);
        let c3park_size: u64 = u64::from(EntrySizes::calculate_c3size(u32::from(self.file.k())));
        let c1address: u64 = self.file.table_address(PlotTable::C1);
        let c3address: u64 = self.file.table_address(PlotTable::C3);
        let c1table_size: u64 = self.file.table_size(PlotTable::C1);
        let c3table_size: u64 = self.file.table_size(PlotTable::C3);
        let c1entry_address: u64 = c1address + park_index * f7size_bytes;
        let park_address: u64 = c3address + park_index * c3park_size;
        // Ensure the C1 address is within the C1 table bounds.
        if c1entry_address >= c1address + c1table_size - f7size_bytes
        // - f7size_bytes because the last C1 entry is an empty/dummy one
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid c1 address: {c1entry_address}"),
            ));
        }

        // First we need to read the root F7 entry for the park,  which is in the C1 table.
        let mut c1_entry_bytes = vec![0; f7size_bytes as usize];
        {
            let file = self.file.file().clone();
            let mut file_lock = file.lock().await;
            file_lock.seek(SeekFrom::Start(c1entry_address)).await?;
            file_lock.read_exact(&mut c1_entry_bytes).await?;
        }
        let mut f7_reader = BitReader::from_bytes_be(&c1_entry_bytes, f7size_bytes as usize * 8);
        let c1 = f7_reader.read_u64(self.plot_file().k() as usize)?;

        // Ensure we can read this park. If it's not present, it means
        // the C1 entry is the only entry in the park, so just return it.
        // Read the park into our buffer
        if park_address >= c3address + c3table_size {
            return Ok(vec![c1]);
        }
        // Read the size of the compressed C3 deltas
        let (count, deltas) = {
            let mut compressed_size_bytes: [u8; 2] = [0; 2];
            let file = self.file.file().clone();
            let mut file_lock = file.lock().await;
            file_lock.seek(SeekFrom::Start(park_address)).await?;
            file_lock.read_exact(&mut compressed_size_bytes).await?;
            let compressed_size = u16::from_be_bytes(compressed_size_bytes);
            if compressed_size > c3park_size as u16 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid size for c3 deltas: {compressed_size}"),
                ));
            }
            let mut park_buffer = vec![0; c3park_size as usize - size_of::<u16>()];
            file_lock.read_exact(&mut park_buffer).await?;
            ans_decode_deltas(
                &park_buffer,
                compressed_size as usize,
                K_CHECKPOINT1INTERVAL as usize,
                K_C3R,
            )?
        };
        let mut f7buffer = vec![0u64; count];
        let mut previous = c1;
        f7buffer[0] = c1;
        // Unpack deltas into absolute values
        for (delta, f7) in deltas.iter().zip(f7buffer[1..].iter_mut()).take(count) {
            let val = previous + u64::from(*delta);
            *f7 = val;
            previous = val;
        }
        Ok(f7buffer)
    }

    pub async fn read_p7entries(&self, park_index: usize) -> Result<(), Error> {
        self.read_p7park(park_index).await
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn read_p7park(&self, park_index: usize) -> Result<(), Error> {
        let entry_size = 1 + self.plot_file().k() as usize;
        let table_address = self.file.table_address(PlotTable::Table7);
        let max_table_size = self.file.table_size(PlotTable::Table7);
        let park_size = u64::from(EntrySizes::calculate_park7_size(u32::from(
            self.plot_file().k(),
        )));
        let max_parks = max_table_size / park_size;
        if park_index >= max_parks as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid park_index for p7: {park_index} >= {max_parks}"),
            ));
        }
        let park_address = table_address + park_index as u64 * park_size;
        let mut buffer = vec![0; park_size as usize];
        {
            let file = self.file.file().clone();
            let mut file_lock = file.lock().await;
            file_lock.seek(SeekFrom::Start(park_address)).await?;
            file_lock.read_exact(&mut buffer).await?;
        }
        let mut reader = BitReader::from_bytes_be(&buffer, park_size as usize * 8);
        for entry in self
            .p7_entries
            .lock()
            .await
            .iter_mut()
            .take(K_ENTRIES_PER_PARK as usize)
        {
            *entry = reader.read_u64(entry_size)?;
        }
        *self.last_park.lock().await = park_index;
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::too_many_lines)]
    pub async fn fetch_proof(&self, index: u64) -> Result<Vec<u64>, Error> {
        let mut first = vec![0u64; PROOF_X_COUNT];
        let mut second = vec![0u64; PROOF_X_COUNT];
        let lp_idx_src: &mut Vec<u64> = &mut first;
        lp_idx_src[0] = index;
        let lp_idx_dst: &mut Vec<u64> = &mut second;
        // Fetch line points to back pointers going through all our tables
        // from 6 to 1, grabbing all of the x's that make up a proof.
        let mut lookup_count = 1;
        let compression_level = match self.header() {
            PlotHeader::V1(_) => 0,
            PlotHeader::V2(h) => h.compression_level,
            PlotHeader::GHv2_5(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Gigahorse Plots are Not Supported",
                ))
            }
        };
        let tables = if compression_level == 0 {
            vec![
                PlotTable::Table6,
                PlotTable::Table5,
                PlotTable::Table4,
                PlotTable::Table3,
                PlotTable::Table2,
                PlotTable::Table1,
            ]
        } else if compression_level < 9 {
            vec![
                PlotTable::Table6,
                PlotTable::Table5,
                PlotTable::Table4,
                PlotTable::Table3,
                PlotTable::Table2,
            ]
        } else {
            vec![
                PlotTable::Table6,
                PlotTable::Table5,
                PlotTable::Table4,
                PlotTable::Table3,
            ]
        };
        for table in tables {
            let (mut i, mut dst) = (0, 0);
            while i < lookup_count {
                let idx = lp_idx_src[i];
                let lp = self.read_line_point(table, idx).await?;
                let (x, y) = if self.file.k() <= 32 && table != PlotTable::Table6 {
                    line_point_to_square64(lp as u64)
                } else {
                    line_point_to_square(lp)
                };
                lp_idx_dst[dst] = y;
                lp_idx_dst[dst + 1] = x;
                i += 1;
                dst += 2;
            }
            lookup_count <<= 1;
            swap(lp_idx_src, lp_idx_dst);
        }
        if compression_level > 0 {
            let plot_id = self.plot_id();
            let k = self.file.k();
            let c = self.compression_level();
            if let Some(pool) = self.proof_decompressor.as_ref() {
                match pool.pull_wait(10000).await {
                    Ok(mut rede) => {
                        debug!("Search for proof at index {index} in plot {}", self.file);
                        rede.prealloc_for_clevel(k, c);
                        match rede.decompress_proof(
                            &plot_id,
                            k,
                            c,
                            lp_idx_src,
                            Some(self.f1_generator.clone()),
                        ) {
                            Ok(p) => {
                                pool.push(rede).await;
                                Ok(p)
                            }
                            Err(e) => {
                                pool.push(rede).await;
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(Error::new(
                        ErrorKind::TimedOut,
                        format!("Failed to get Decompressor in Time: {e:?}"),
                    )),
                }
            } else {
                warn!("Using Decompressor not in pool!");
                debug!("Search for proof at index {index} in plot {}", self.file);
                let mut d = Decompressor::default();
                d.prealloc_for_clevel(k, c);
                d.decompress_proof(
                    &plot_id,
                    k,
                    compression_level,
                    lp_idx_src,
                    Some(self.f1_generator.clone()),
                )
            }
        } else {
            Ok(first)
        }
    }

    pub fn read_line_point_park(
        &self,
        _table: PlotTable,
        _index: u64,
        _line_points: &[u128],
        _out_entry_count: u64,
    ) -> bool {
        todo!()
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn read_line_point(&self, table: PlotTable, index: u64) -> Result<u128, Error> {
        let park_index = index / u64::from(K_ENTRIES_PER_PARK);
        let components = self.read_lp_park_components(table, park_index).await?;
        let lp_local_idx = index as usize - park_index as usize * K_ENTRIES_PER_PARK as usize;
        if lp_local_idx > 0 {
            if lp_local_idx > components.deltas.len() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "Invalid Liner Point Index: {} >= {}",
                        lp_local_idx - 1,
                        components.deltas.len()
                    ),
                ));
            }
            let (mut sum_stubs, mut sum_deltas, mut start_bit) = (0, 0, 0usize);
            let stub_size = self.calculate_lp_stubs_bits_size(table);
            for delta in components
                .deltas
                .iter()
                .take(min(lp_local_idx, components.deltas.len()))
            {
                sum_stubs += (bytes_to_u64(&components.stubs[(start_bit / 8)..])
                    << (start_bit % 8))
                    >> (64 - stub_size);
                start_bit += stub_size as usize;
                sum_deltas += u64::from(*delta);
            }
            let delta = ((sum_deltas as u128) << stub_size) + sum_stubs as u128;
            Ok(components.base_line_point + delta)
        } else {
            Ok(components.base_line_point)
        }
    }

    pub fn fetch_proof_from_p7entry(&self, _p7_entry: u64, _proof: &[u64]) -> bool {
        todo!()
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::too_many_lines)]
    pub async fn fetch_quality_xs_for_p7entry(
        &self,
        index: u64,
        challenge: &[u8],
    ) -> Result<(u64, u64), Error> {
        let compression_level = self.file.compression_level();
        let last5bits = challenge[31] & 0x1f;
        let mut lp_index = index;
        let mut alt_index = 0;
        let (tables, end_table) = if compression_level == 0 {
            (
                vec![
                    PlotTable::Table6,
                    PlotTable::Table5,
                    PlotTable::Table4,
                    PlotTable::Table3,
                    PlotTable::Table2,
                ],
                PlotTable::Table1,
            )
        } else if compression_level < 9 {
            (
                vec![
                    PlotTable::Table6,
                    PlotTable::Table5,
                    PlotTable::Table4,
                    PlotTable::Table3,
                ],
                PlotTable::Table2,
            )
        } else {
            (
                vec![PlotTable::Table6, PlotTable::Table5, PlotTable::Table4],
                PlotTable::Table3,
            )
        };
        for table in tables {
            let lp = self.read_line_point(table, lp_index).await?;
            let (x, y) = if self.file.k() <= 32 {
                line_point_to_square64(lp as u64)
            } else {
                line_point_to_square(lp)
            };
            if (last5bits >> (table as usize - 1)) & 1 == 1 {
                lp_index = x;
                alt_index = y;
            } else {
                lp_index = y;
                alt_index = x;
            }
        }
        if compression_level > 0 {
            let need_both_leaves = compression_level >= 6;
            let x_lp0 = self.read_line_point(end_table, lp_index).await?;
            let mut x_lp1: LinePoint = LinePoint { hi: 0, lo: 0 };
            if need_both_leaves {
                let tmp = self.read_line_point(end_table, alt_index).await?;
                x_lp1 = LinePoint {
                    hi: (tmp >> 64) as u64,
                    lo: (tmp) as u64,
                };
            }
            let req = CompressedQualitiesRequest {
                plot_id: self.plot_id(),
                compression_level,
                challenge,
                line_points: [
                    LinePoint {
                        hi: (x_lp0 >> 64) as u64,
                        lo: (x_lp0) as u64,
                    },
                    x_lp1,
                ],
                f1_generator: Some(self.f1_generator.clone()),
            };
            let k = self.file.k();
            let c = self.file.compression_level();
            if let Some(pool) = &self.quality_decompressor {
                match pool.pull_wait(10000).await {
                    Ok(mut rede) => {
                        debug!(
                            "Search for Challenge {} in plot {}",
                            encode(challenge),
                            self.file
                        );
                        rede.prealloc_for_clevel(k, c);
                        match rede.get_fetch_qualties_x_pair(k, req) {
                            Ok(p) => {
                                pool.push(rede).await;
                                Ok(p)
                            }
                            Err(e) => {
                                pool.push(rede).await;
                                Err(e)
                            }
                        }
                    }
                    Err(e) => Err(Error::new(
                        ErrorKind::TimedOut,
                        format!("Failed to get Decompressor in Time: {e:?}"),
                    )),
                }
            } else {
                let mut d = Decompressor::default();
                d.prealloc_for_clevel(k, c);
                d.get_fetch_qualties_x_pair(self.file.k(), req)
            }
        } else {
            let lp = self.read_line_point(end_table, lp_index).await?;
            Ok(if self.file.k() <= 32 {
                line_point_to_square64(lp as u64)
            } else {
                line_point_to_square(lp)
            })
        }
    }

    pub async fn fetch_quality_for_p7entry(
        &self,
        p7_entry: u64,
        challenge: &[u8],
    ) -> Result<Bytes32, Error> {
        let (x1, x2) = self
            .fetch_quality_xs_for_p7entry(p7_entry, challenge)
            .await?;
        let mut hash_input = Vec::with_capacity(HASH_SIZE_MAX);
        hash_input.extend(challenge);
        let mut bits = BitReader::default();
        bits.append_value(x2, self.file.k() as usize);
        bits.append_value(x1, self.file.k() as usize);
        hash_input.extend(bits.to_bytes());
        Ok(Bytes32::new(hash_256(hash_input)))
    }

    pub async fn fetch_qualities_for_challenge(
        &self,
        challenge: &[u8],
    ) -> Result<Vec<(u64, Bytes32)>, Error> {
        let k = self.plot_file().k() as usize;
        let mut challenge_reader = BitReader::from_bytes_be(&challenge[0..8], 64);
        let f7 = challenge_reader.read_u64(k)?;
        let (match_count, p7base_index) = self.get_p7indices_for_f7(f7).await?;
        if match_count == 0 {
            Err(Error::new(
                ErrorKind::NotFound,
                format!("Could not find f7({}) {} in plot.", f7, encode(challenge)),
            ))
        } else {
            let mut qualities = vec![];
            for i in 0..match_count {
                let p7index = p7base_index + i;
                let p7park = p7index / K_ENTRIES_PER_PARK as usize;
                if p7park != *self.last_park.lock().await {
                    self.read_p7entries(p7park).await?;
                }
                let local_p7index = p7index - p7park * K_ENTRIES_PER_PARK as usize;
                let t6index = self.p7_entries.lock().await[local_p7index];
                let quality = self.fetch_quality_for_p7entry(t6index, challenge).await?;
                qualities.push((t6index, quality));
            }
            Ok(qualities)
        }
    }

    pub async fn fetch_ordered_proof(&self, index: u64) -> Result<Vec<u64>, Error> {
        let proof = match self.fetch_proof(index).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to get Proof at index {index}: {:?}", e);
                return Err(e);
            }
        };
        //Reorder
        self.reorder_proof(&proof).await
    }

    pub async fn reorder_proof(&self, proof: &[u64]) -> Result<Vec<u64>, Error> {
        let mut fx = self.fx.lock().await;
        let mut meta = self.meta.lock().await;
        meta.clear();
        let k = self.plot_file().k();
        let bytes = self.plot_file().plot_id();
        reorder_proof(k, &bytes.bytes(), proof, &mut fx, &mut meta)
    }

    pub async fn fetch_proofs_for_challenge(
        &self,
        challenge: &[u8],
    ) -> Result<Vec<Vec<u64>>, Error> {
        let mut challenge_reader = BitReader::from_bytes_be(&challenge[0..8], 64);
        let f7 = challenge_reader.read_u64(self.plot_file().k() as usize)?;
        let (match_count, p7base_index) = self.get_p7indices_for_f7(f7).await?;
        if match_count == 0 {
            Err(Error::new(
                ErrorKind::NotFound,
                format!("Could not find f7 {f7} in plot."),
            ))
        } else {
            let mut proofs = FxHashSet::default();
            for i in 0..match_count {
                let p7index = p7base_index + i;
                let p7park = p7index / K_ENTRIES_PER_PARK as usize;
                if p7park != *self.last_park.lock().await {
                    self.read_p7entries(p7park).await?;
                }
                let local_p7index = p7index - p7park * K_ENTRIES_PER_PARK as usize;
                let t6index = self.p7_entries.lock().await[local_p7index];
                match self.fetch_ordered_proof(t6index).await {
                    Ok(p) => {
                        proofs.insert(p);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get Proof for f7: {f7}, index: {}/{match_count} : {:?}",
                            i + 1,
                            e
                        );
                    }
                }
            }
            Ok(proofs.into_iter().collect::<Vec<Vec<u64>>>())
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn get_p7indices_for_f7(&self, f7: u64) -> Result<(usize, usize), Error> {
        let mut c2index: u32 = 0;
        let mut broke = false;
        for c2 in &self.c2_entries {
            if *c2 > f7 {
                c2index = c2index.saturating_sub(1);
                broke = true;
                break;
            }
            c2index += 1;
        }
        if !broke {
            c2index -= 1;
        }
        let c1start_index = u64::from(c2index) * u64::from(K_CHECKPOINT2INTERVAL);
        let k = self.file.k() as usize;
        let f7size_bytes = ucdiv_t(k, 8);
        let f7bit_count = f7size_bytes * 8;
        let c1table_address = self.file.table_address(PlotTable::C1);
        let c1table_size = self.file.table_size(PlotTable::C1);
        let c1table_end = c1table_address + c1table_size;
        let c1entry_address = c1table_address + c1start_index * f7size_bytes as u64;
        let c1end_address = min(
            c1entry_address + (u64::from(K_CHECKPOINT1INTERVAL) * f7size_bytes as u64),
            c1table_end,
        );
        if c1entry_address > c1end_address {
            return Ok((0, 0));
        }
        let read_size = (c1end_address - c1entry_address) as usize;
        let c1entry_count = read_size / f7size_bytes;
        if c1entry_count == 0 {
            return Ok((0, 0));
        }
        let mut c1_reader;
        {
            let file = self.file.file().clone();
            let mut file_lock = file.lock().await;
            file_lock.seek(SeekFrom::Start(c1entry_address)).await?;
            // Read C1 entries until we find one equal or larger than the f7 we're looking for
            let mut c1_buffer = vec![0; read_size];
            file_lock.read_exact(&mut c1_buffer[0..read_size]).await?;
            c1_reader = BitReader::from_bytes_be(&c1_buffer, read_size * 8);
        }
        let mut c3park = c1start_index;
        let mut c1;
        let mut i = 0;
        loop {
            c1_reader.seek(SeekFrom::Start((i * f7bit_count) as u64))?;
            c1 = c1_reader.read_u64(k)?;
            i += 1;
            if c1 >= f7 || i >= c1entry_count {
                c3park = c3park.saturating_sub(1);
                break;
            }
            c3park += 1;
        }
        let park_count = if c1 == f7 && c3park > 0 { 2 } else { 1 }; // If we got the same c1 as f7, then the previous
                                                                     // needs to be read as well because we may have duplicate f7s
                                                                     // in the previous park's last entries.
        let mut first_c3_buffer = self.read_c3park(c3park).await?;
        if first_c3_buffer.is_empty() {
            return Ok((0, 0));
        }
        if park_count > 1 {
            debug_assert_eq!(park_count, 2);
            let second_c3_buffer = self.read_c3park(c3park + 1).await?;
            if second_c3_buffer.is_empty() {
                return Ok((0, 0));
            }
            first_c3_buffer.extend(second_c3_buffer);
        }
        // Grab as many matches as we can
        let c3start_index = c3park * u64::from(K_CHECKPOINT1INTERVAL);
        let out_index;
        // let iterator =
        for i in 0..first_c3_buffer.len() {
            if first_c3_buffer[i] == f7 {
                let mut match_count = 1;
                out_index = c3start_index + i as u64;
                let mut i = i + 1;
                while i < first_c3_buffer.len() && first_c3_buffer[i] == f7 {
                    match_count += 1;
                    i += 1;
                }
                return Ok((match_count, out_index as usize));
            }
        }
        Ok((0, 0))
    }
    pub fn plot_file(&self) -> &T {
        &self.file
    }
    pub fn header(&self) -> &PlotHeader {
        self.file.header()
    }
    pub fn plot_id(&self) -> Bytes32 {
        self.file.plot_id()
    }
    pub fn compression_level(&self) -> u8 {
        self.file.compression_level()
    }
    #[allow(clippy::cast_possible_truncation)]
    pub fn calculate_max_deltas_size(&self, table: PlotTable) -> u32 {
        if self.is_compressed_table(table) {
            let info = get_compression_info_for_level(self.file.compression_level());
            let lp_size = ucdiv(u32::from(self.file.k() * 2), 8);
            let stub_byte_size = self.calculate_lp_stubs_size(table);
            info.table_park_size as u32 - (lp_size + stub_byte_size)
        } else {
            EntrySizes::calculate_max_deltas_size(table)
        }
    }
    pub fn calculate_lp_stubs_bits_size(&self, table: PlotTable) -> u32 {
        assert!(
            (table as u8) >= self.get_lowest_stored_table() as u8,
            "Getting stub bit size for invalid table."
        );
        if self.is_compressed_table(table) {
            get_compression_info_for_level(self.file.compression_level()).stub_size_bits
        } else {
            u32::from(self.file.k() - K_STUB_MINUS_BITS)
        }
    }
    pub fn calculate_lp_stubs_size(&self, table: PlotTable) -> u32 {
        ucdiv(
            (K_ENTRIES_PER_PARK - 1) * self.calculate_lp_stubs_bits_size(table),
            8,
        )
    }
    async fn read_lp_park_components(
        &self,
        table: PlotTable,
        park_index: u64,
    ) -> Result<LinePointParkComponents, Error> {
        let park_size = self.get_park_size_for_table(table);
        let k = u32::from(self.plot_file().k());
        let table_max_size = self.plot_file().table_size(table);
        let table_address = self.plot_file().table_address(table);
        let max_parks = table_max_size / park_size;
        if park_index >= max_parks {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Invalid Park Index: {park_index} >= {max_parks}"),
            ));
        }
        let file = self.file.file().clone();
        let mut file_lock = file.lock().await;
        let park_address = table_address + park_index * park_size;
        file_lock.seek(SeekFrom::Start(park_address)).await?;

        // This is the checkpoint at the beginning of the park
        let line_point_size = EntrySizes::line_point_size_bytes(k);
        let mut line_point_bin = vec![0u8; line_point_size as usize];
        file_lock.read_exact(&mut line_point_bin).await?;
        let base_line_point = slice_u128from_bytes(line_point_bin, 0, k * 2);

        // Reads EPP stubs
        let stubs_size_bytes = self.calculate_lp_stubs_size(table) as usize;
        let mut stubs = vec![0u8; stubs_size_bytes];
        file_lock.read_exact(&mut stubs).await?;

        // Reads EPP deltas
        let max_deltas_size = self.calculate_max_deltas_size(table);
        // Reads the size of the encoded deltas object
        let mut encoded_deltas_buf: [u8; 2] = [0u8; 2];
        file_lock.read_exact(&mut encoded_deltas_buf).await?;
        let mut encoded_deltas_size = u16::from_le_bytes(encoded_deltas_buf);
        if !(encoded_deltas_size & 0x8000) > 0 && u32::from(encoded_deltas_size) > max_deltas_size {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid size for deltas: {encoded_deltas_size}"),
            ));
        }
        let mut deltas;
        if 0x8000 & encoded_deltas_size > 0 {
            // Uncompressed
            encoded_deltas_size &= 0x7fff;
            deltas = vec![0u8; encoded_deltas_size as usize];
            file_lock.read_exact(&mut deltas).await?;
        } else {
            // Compressed
            let mut deltas_bin = vec![0u8; (max_deltas_size) as usize];
            file_lock.read_exact(&mut deltas_bin).await?;
            //Decodes the deltas
            let num_deltas = (K_ENTRIES_PER_PARK - 1) as usize;
            let d_table = self.get_dtable_for_table(table)?;
            let mut dst = vec![0u8; num_deltas];
            decompress_using_dtable(
                &mut dst,
                num_deltas,
                &deltas_bin,
                encoded_deltas_size as usize,
                d_table,
            )?;
            deltas = dst;
        }
        Ok(LinePointParkComponents {
            base_line_point,
            stubs,
            deltas,
        })
    }
    fn get_dtable_for_table(&self, table: PlotTable) -> Result<Arc<DTable>, Error> {
        if self.is_compressed_table(table) {
            create_compression_dtable(self.file.compression_level())
        } else {
            let r = K_RVALUES[table as usize];
            encoding::get_d_table(r)
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    async fn load_c2entries(&mut self) -> Result<(), Error> {
        let c2size = self.file.table_size(PlotTable::C2) as usize;
        if c2size != 0 {
            let k = self.file.k() as usize;
            let f7byte_size = ucdiv_t(k, 8);
            let c2max_entries = c2size / f7byte_size;
            if c2max_entries > 0 {
                let address = self.file.table_address(PlotTable::C2);
                let file = self.file.file();
                let mut file_lock = file.lock().await;
                file_lock.seek(SeekFrom::Start(address)).await?;
                let mut buffer = vec![0; c2size];
                file_lock.read_exact(&mut buffer).await?;
                self.c2_entries = Vec::with_capacity(c2max_entries);
                let mut reader = BitReader::from_bytes_be(&buffer, c2size * 8);
                let mut prev_f7 = 0;
                for _ in 0..c2max_entries {
                    let f7 = reader.read_u64(k)?;
                    // Short circuit if we encounter an unsorted/out-of-order c2 entry
                    if f7 < prev_f7 {
                        break;
                    }
                    self.c2_entries.push(f7);
                    prev_f7 = f7;
                }
                Ok(())
            } else {
                Err(Error::new(ErrorKind::UnexpectedEof, "c2max_entries is 0"))
            }
        } else {
            Err(Error::new(
                ErrorKind::UnexpectedEof,
                "Failed to load c2 size",
            ))
        }
    }
}

pub fn reorder_proof(
    k: u8,
    plot_id: &[u8; 32],
    proof: &[u64],
    fx: &mut [u64],
    meta: &mut Vec<BitReader>,
) -> Result<Vec<u64>, Error> {
    Ok(get_f7_from_proof_and_reorder(u32::from(k), plot_id, proof, fx, meta)?.1)
}

pub fn read_plot_header(file: &mut std::fs::File) -> Result<PlotHeader, Error> {
    use std::io::Read;
    let mut full_buffer = [0; 320];
    file.read_exact(&mut full_buffer)?;
    if HEADER_V2_MAGIC == full_buffer[0..4] {
        Ok(PlotHeader::V2(parse_v2(&full_buffer)?))
    } else if HEADER_MAGIC == full_buffer[0..19] {
        //TODO Gigahorse plots also follow this format but cant be actually read, memo is encrypted.
        Ok(PlotHeader::V1(parse_v1(&full_buffer)?))
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid plot header magic",
        ))
    }
}

fn parse_v1(full_buffer: &[u8]) -> Result<PlotHeaderV1, Error> {
    let mut start = 0;
    let mut plot_header = PlotHeaderV1 {
        magic: full_buffer[start..start + 19].try_into().unwrap(),
        ..Default::default()
    };
    start += 19;
    plot_header.id = Bytes32::parse(&full_buffer[start..start + 32])?;
    start += 32;
    plot_header.k = full_buffer[start];
    start += 1;
    plot_header.format_desc_len =
        u16::from_be_bytes(full_buffer[start..start + 2].try_into().unwrap());
    start += 2;
    plot_header.format_desc =
        full_buffer[start..(start + plot_header.format_desc_len as usize)].to_vec();
    start += plot_header.format_desc_len as usize;
    plot_header.memo_len = u16::from_be_bytes(full_buffer[start..start + 2].try_into().unwrap());
    start += 2;
    plot_header.memo =
        PlotMemo::try_from(&full_buffer[start..(start + plot_header.memo_len as usize)])?;
    start += plot_header.memo_len as usize;
    for pointer in &mut plot_header.table_begin_pointers {
        *pointer = u64::from_be_bytes(full_buffer[start..start + 8].try_into().unwrap());
        start += 8;
    }
    Ok(plot_header)
}

fn parse_v2(full_buffer: &[u8]) -> Result<PlotHeaderV2, Error> {
    let mut start = 0;
    let mut plot_header = PlotHeaderV2 {
        magic: full_buffer[start..start + 4].try_into().unwrap(),
        ..Default::default()
    };
    start += 4;
    plot_header.version = u32::from_le_bytes(full_buffer[start..start + 4].try_into().unwrap());
    start += 4;
    plot_header.id = Bytes32::parse(&full_buffer[start..start + 32])?;
    start += 32;
    plot_header.k = full_buffer[start];
    start += 1;
    plot_header.memo_len = u16::from_be_bytes(full_buffer[start..start + 2].try_into().unwrap());
    start += 2;
    plot_header.memo =
        PlotMemo::try_from(&full_buffer[start..(start + plot_header.memo_len as usize)])?;
    start += plot_header.memo_len as usize;
    plot_header.plot_flags = u32::from_le_bytes(full_buffer[start..start + 4].try_into().unwrap());
    start += 4;
    if plot_header.plot_flags & 1u32 == 1u32 {
        plot_header.compression_level = full_buffer[start];
        start += 1;
    }
    for pointer in &mut plot_header.table_begin_pointers {
        *pointer = u64::from_be_bytes(full_buffer[start..start + 8].try_into().unwrap());
        start += 8;
    }
    for pointer in &mut plot_header.table_sizes {
        *pointer = u64::from_be_bytes(full_buffer[start..start + 8].try_into().unwrap());
        start += 8;
    }
    Ok(plot_header)
}

pub async fn read_plot_header_async(file: &mut tokio::fs::File) -> Result<PlotHeader, Error> {
    let mut full_buffer = [0; 320];
    file.read_exact(&mut full_buffer).await?;
    if HEADER_V2_MAGIC == full_buffer[0..4] {
        Ok(PlotHeader::V2(parse_v2(&full_buffer)?))
    } else if HEADER_MAGIC == full_buffer[0..19] {
        Ok(PlotHeader::V1(parse_v1(&full_buffer)?))
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid plot header magic",
        ))
    }
}

pub async fn read_plot_file_header_async(
    p: impl AsRef<Path>,
) -> Result<(PathBuf, PlotHeader), Error> {
    if !p.as_ref().is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "Path must be a file"));
    }
    let mut file = open_read_only_async(p.as_ref()).await?;
    Ok((
        p.as_ref().to_path_buf(),
        read_plot_header_async(&mut file).await?,
    ))
}

pub fn read_plot_file_header(p: impl AsRef<Path>) -> Result<(PathBuf, PlotHeader), Error> {
    if !p.as_ref().is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "Path must be a file"));
    }
    let mut file = open_read_only(p.as_ref())?;
    Ok((p.as_ref().to_path_buf(), read_plot_header(&mut file)?))
}

type AllPlotHeaders = (Vec<(PathBuf, PlotHeader)>, Vec<PathBuf>);

pub fn read_all_plot_headers(p: impl AsRef<Path>) -> Result<AllPlotHeaders, Error> {
    if p.as_ref().is_dir() {
        let dir = std::fs::read_dir(p)?;
        let mut valid_rtn = vec![];
        let mut failed_rtn = vec![];
        for c in dir {
            match c {
                Ok(c) => {
                    let path = c.path();
                    if path.extension() == Some(OsStr::new("plot")) {
                        match read_plot_file_header(&path) {
                            Ok(d) => {
                                valid_rtn.push(d);
                            }
                            Err(e) => {
                                error!("Failed to open plot: {:?}", e);
                                failed_rtn.push(path.clone());
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to open directory entry: {:?}", e);
                }
            }
        }
        Ok((valid_rtn, failed_rtn))
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Path must be a directory",
        ))
    }
}

pub async fn read_all_plot_headers_async(
    p: impl AsRef<Path>,
    existing_paths: &[&Path],
) -> Result<AllPlotHeaders, Error> {
    if p.as_ref().is_dir() {
        let mut dir = tokio::fs::read_dir(p).await?;
        let mut valid_rtn = vec![];
        let mut failed_rtn = vec![];
        loop {
            match dir.next_entry().await {
                Ok(Some(c)) => {
                    let path = c.path();
                    if existing_paths.contains(&path.as_path()) {
                        continue;
                    }
                    if path.extension() == Some(OsStr::new("plot")) {
                        match read_plot_file_header_async(&path).await {
                            Ok(d) => {
                                valid_rtn.push(d);
                            }
                            Err(e) => {
                                error!("Failed to open plot: {:?}", e);
                                failed_rtn.push(path.clone());
                            }
                        }
                    }
                }
                Ok(None) => {
                    break;
                }
                Err(e) => {
                    error!("Failed to open directory entry: {:?}", e);
                }
            }
        }
        Ok((valid_rtn, failed_rtn))
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Path must be a directory",
        ))
    }
}
