use crate::constants::{K_BC, K_EXTRA_BITS_POW, L_TARGETS};
use crate::encoding::{line_point_to_square, line_point_to_square64, square_to_line_point128};
use crate::plots::compression::{
    get_compression_info_for_level, get_entries_per_bucket_for_compression_level,
    get_max_table_pairs_for_compression_level,
};
use crate::plots::fx_generator::{
    generate_fx_for_pairs_table2, generate_fx_for_pairs_table3, generate_fx_for_pairs_table4,
    generate_fx_for_pairs_table5, generate_fx_for_pairs_table6, F1Generator,
};
use crate::plots::{
    ForwardPropResult, Group, K32Meta2, K32Meta4, Pair, ProofTable, BB_PLOT_VERSION,
    MIN_TABLE_PAIRS, POST_PROOF_CMP_X_COUNT, POST_PROOF_X_COUNT, PROOF_X_COUNT,
};
use crate::utils::radix_sort::RadixSorter;
use crate::utils::span::Span;
use crate::utils::{calc_thread_vars, ThreadVars};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::plots::PlotTable;
use log::{debug, error};
use num_traits::{One, Zero};
use rayon::prelude::*;
use std::cmp::{max, min};
use std::collections::VecDeque;
use std::hint::spin_loop;
use std::io::{Error, ErrorKind};
use std::mem::swap;
use std::num::NonZeroUsize;
use std::ops::{Add, AddAssign, Div, Mul, Sub};
use std::ptr::copy_nonoverlapping;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::available_parallelism;
use std::time::Instant;

#[derive(Debug)]
pub struct ProofContext {
    left_length: u32,
    right_length: u32,
    y_left: Span<u64>,          // L table Y
    y_right: Span<u64>,         // R table Y
    meta_left: Span<K32Meta4>,  // L table metadata
    meta_right: Span<K32Meta4>, // R table metadata
    proof: [u64; PROOF_X_COUNT],
}

#[derive(Debug)]
pub struct GroupScanJob {
    id: usize,
    thread_vars: ThreadVars<usize>,
    group_thread_vars: ThreadVars<usize>,
    group_indices: *mut u32,
    group_count: usize,
    end: usize,
    copy_offset: isize,
    pub final_group_entries: *mut u32,
}
unsafe impl<
        T: Div<Output = T>
            + Mul<Output = T>
            + Eq
            + PartialEq
            + Sub<Output = T>
            + Add<Output = T>
            + AddAssign
            + Copy
            + One,
    > Send for ThreadVars<T>
{
}
unsafe impl<
        T: Div<Output = T>
            + Mul<Output = T>
            + Eq
            + PartialEq
            + Sub<Output = T>
            + Add<Output = T>
            + AddAssign
            + Copy
            + One,
    > Sync for ThreadVars<T>
{
}
unsafe impl Send for GroupScanJob {}
unsafe impl Sync for GroupScanJob {}

#[derive(Debug, Copy, Clone)]
pub struct LinePoint {
    pub hi: u64, // High-order bytes
    pub lo: u64, // Low-order bytes
}
unsafe impl Send for LinePoint {}
unsafe impl Sync for LinePoint {}

#[derive(Debug)]
pub struct CompressedQualitiesRequest<'a> {
    pub plot_id: Bytes32,
    pub compression_level: u8,
    pub challenge: &'a [u8],
    pub line_points: [LinePoint; 2],
    pub f1_generator: Option<Arc<F1Generator>>,
}
unsafe impl Send for CompressedQualitiesRequest<'_> {}
unsafe impl Sync for CompressedQualitiesRequest<'_> {}

#[derive(Debug)]
pub struct TableContext<'a> {
    context: &'a mut Decompressor,
    entries_per_bucket: isize,
    out_y: Span<u64>,
    out_meta: Span<K32Meta2>,
    out_pairs: Span<Pair>,
    pub f1_generator: Arc<F1Generator>,
}
unsafe impl Send for TableContext<'_> {}
unsafe impl Sync for TableContext<'_> {}

#[derive(Debug)]
pub struct ProofRequest {
    compressed_proof: Vec<u64>, //[u64; POST_PROOF_CMP_X_COUNT],
    full_proof: Vec<u64>,       //[u64; POST_PROOF_X_COUNT],
    c_level: u8,
    plot_id: Bytes32,
    f1_generator: Option<Arc<F1Generator>>,
}

#[derive(Debug)]
pub enum DecompressorMode {
    CPU,
    ANY,
    GPU(usize),
}

#[derive(Debug)]
pub struct DecompressorConfig {
    pub api_version: u32,
    pub thread_count: u8,
    pub mode: DecompressorMode,
}
impl Default for DecompressorConfig {
    #[allow(clippy::cast_possible_truncation)]
    fn default() -> Self {
        Self {
            api_version: BB_PLOT_VERSION,
            thread_count: max(
                4,
                available_parallelism()
                    .unwrap_or_else(|_| {
                        NonZeroUsize::new(8).expect("Safe Value Expected for Non Zero Usize")
                    })
                    .get() as u8,
            ),
            mode: DecompressorMode::CPU,
        }
    }
}

#[derive(Debug)]
pub enum DecompressorState {
    None = 0,
    Qualities = 1,
}

#[derive(Debug)]
pub struct GpuContext {}

#[derive(Debug)]
pub struct DecompressorPool {
    depth: u8,
    pool: Arc<tokio::sync::Mutex<VecDeque<Decompressor>>>,
}
impl DecompressorPool {
    #[inline]
    #[must_use]
    pub fn new(depth: u8, thread_count: u8) -> DecompressorPool {
        let mut pool = VecDeque::new();
        for _ in 0..depth {
            let mut d = Decompressor::new(DecompressorConfig {
                thread_count,
                mode: DecompressorMode::CPU,
                ..Default::default()
            });
            d.prealloc_for_clevel(32, 3);
            pool.push_back(d);
        }
        DecompressorPool {
            depth,
            pool: Arc::new(tokio::sync::Mutex::new(pool)),
        }
    }
    #[inline]
    pub async fn init(&mut self, depth: u8) {
        match self.depth.cmp(&depth) {
            std::cmp::Ordering::Greater => {
                while self.depth < depth {
                    self.pool.lock().await.push_back(Decompressor::default());
                    self.depth += 1;
                }
            }
            std::cmp::Ordering::Less => {
                self.pool.lock().await.truncate(depth as usize);
                self.depth = depth;
            }
            std::cmp::Ordering::Equal => {}
        }
    }
    #[inline]
    pub async fn len(&self) -> usize {
        self.pool.lock().await.len()
    }
    #[inline]
    pub async fn is_empty(&self) -> bool {
        self.pool.lock().await.is_empty()
    }
    #[inline]
    pub async fn pull_wait(&self, timeout_millis: usize) -> Result<Decompressor, Error> {
        let start = Instant::now();
        loop {
            if let Some(v) = self.pool.lock().await.pop_back() {
                return Ok(v);
            } else if Instant::now().duration_since(start).as_millis() > timeout_millis as u128 {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "Timed out waiting for Decompressor",
                ));
            }
            spin_loop();
        }
    }
    #[inline]
    pub async fn push(&self, t: Decompressor) {
        self.pool.lock().await.push_back(t);
    }
}

#[derive(Debug)]
pub struct Decompressor {
    pub config: DecompressorConfig,
    pub state: DecompressorState,
    max_entries_per_bucket: u64,
    pub sort_key: Vec<u32>,
    pub pairs: Vec<Pair>,
    pub pairs_tmp: Vec<Pair>,
    pub groups_boundaries: Vec<u32>,
    pub x_buffer: Vec<u32>,
    pub x_buffer_tmp: Vec<u32>,
    pub y_buffer: Vec<u64>,
    pub y_buffer_f1: Vec<u64>,
    pub y_buffer_tmp: Vec<u64>,
    pub meta_buffer: Vec<K32Meta4>,
    pub meta_buffer_tmp: Vec<K32Meta4>,
    pub tables: [ProofTable; 7],
    pub proof_context: Option<ProofContext>,
    pub gpu_context: Option<GpuContext>,
    alloc_count: usize,
    max_pairs_per_table: usize,
}
impl Default for Decompressor {
    fn default() -> Self {
        Self::new(DecompressorConfig::default())
    }
}
impl Decompressor {
    #[must_use]
    pub fn new(config: DecompressorConfig) -> Self {
        Decompressor {
            config,
            state: DecompressorState::None,
            max_entries_per_bucket: 0,
            alloc_count: 0,
            max_pairs_per_table: 0,
            sort_key: vec![],
            pairs: vec![],
            pairs_tmp: vec![],
            groups_boundaries: vec![],
            x_buffer: vec![],
            x_buffer_tmp: vec![],
            y_buffer: vec![],
            y_buffer_f1: vec![],
            y_buffer_tmp: vec![],
            meta_buffer: vec![],
            meta_buffer_tmp: vec![],
            tables: [
                ProofTable::default(),
                ProofTable::default(),
                ProofTable::default(),
                ProofTable::default(),
                ProofTable::default(),
                ProofTable::default(),
                ProofTable::default(),
            ],
            proof_context: None,
            gpu_context: None,
        }
    }
    #[must_use]
    pub fn has_gpu(&self) -> bool {
        self.gpu_context.is_some()
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_precision_loss)]
    #[allow(clippy::cast_sign_loss)]
    pub fn prealloc_for_clevel(&mut self, k: u8, c_level: u8) {
        assert_eq!(k, 32);
        let entries_per_bucket = get_entries_per_bucket_for_compression_level(k, c_level);
        if self.max_entries_per_bucket < entries_per_bucket {
            let alloc_count = entries_per_bucket as usize * 2;
            // The pair requirements ought to be much less as the number of matches we get per group is not as high.
            let mut max_pairs_per_table = max(
                MIN_TABLE_PAIRS as usize,
                get_max_table_pairs_for_compression_level(k, c_level),
            );
            self.alloc_count = alloc_count;
            self.max_pairs_per_table = max_pairs_per_table;
            self.sort_key = vec![0u32; max_pairs_per_table];
            self.x_buffer = vec![0u32; alloc_count];
            self.x_buffer_tmp = vec![0u32; alloc_count];
            self.y_buffer = vec![0u64; alloc_count];
            self.y_buffer_f1 = vec![0u64; alloc_count];
            self.y_buffer_tmp = vec![0u64; alloc_count];
            self.groups_boundaries = vec![0u32; alloc_count];
            self.pairs = vec![Pair::zero(); max_pairs_per_table];
            self.pairs_tmp = vec![Pair::zero(); max_pairs_per_table];
            self.meta_buffer = vec![K32Meta4::zero(); max_pairs_per_table];
            self.meta_buffer_tmp = vec![K32Meta4::zero(); max_pairs_per_table];
            // Allocate proof tables
            // Table 1 needs no groups, as we write to the R table's merged group, always
            for i in 1..7 {
                self.tables[i] = ProofTable {
                    pairs: vec![],
                    capacity: 0,
                    length: 0,
                    groups: [Group {
                        count: 0,
                        offset: 0,
                    }; 16],
                };
                self.tables[i].pairs = vec![Pair::zero(); max_pairs_per_table];
                self.tables[i].capacity = max_pairs_per_table as u32;
                // Reduce the match count for each subsequent table by nearly half
                max_pairs_per_table = max(
                    (max_pairs_per_table as f64 * 0.6) as usize,
                    MIN_TABLE_PAIRS as usize,
                );
            }
            self.max_entries_per_bucket = entries_per_bucket;
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::too_many_lines)]
    pub fn fetch_full_proof(&mut self, k: u8, req: &mut ProofRequest) -> Result<(), Error> {
        self.request_setup(k, req.c_level)?;
        let num_groups = POST_PROOF_CMP_X_COUNT;
        let entries_per_bucket = get_entries_per_bucket_for_compression_level(k, req.c_level);
        assert!(entries_per_bucket <= 0xFFFF_FFFF);
        let mut tables = Span::new(self.tables.as_mut_ptr(), self.tables.len());
        let mut x_groups = [0u32; POST_PROOF_X_COUNT];
        if req.c_level < 9 {
            let mut j = 0usize;
            for xs in req
                .compressed_proof
                .iter()
                .take(num_groups)
                .copied()
                .map(line_point_to_square64)
            {
                x_groups[j] = xs.1 as u32;
                x_groups[j + 1] = xs.0 as u32;
                j += 2;
            }
        } else {
            let entry_bits = get_compression_info_for_level(req.c_level).entry_size_bits;
            let mask = (1 << entry_bits) - 1;
            let mut j = 0usize;
            for xs in req
                .compressed_proof
                .iter()
                .take(num_groups / 2)
                .copied()
                .map(line_point_to_square64)
            {
                x_groups[j] = (xs.1 as u32) & mask;
                x_groups[j + 1] = (xs.1 as u32) >> entry_bits;
                x_groups[j + 2] = (xs.0 as u32) & mask;
                x_groups[j + 3] = (xs.0 as u32) >> entry_bits;
                j += 4;
            }
        }
        let out_y = Span::new(self.y_buffer_tmp.as_mut_ptr(), self.y_buffer_tmp.len());
        let out_meta = Span::new(
            self.meta_buffer_tmp.as_mut_ptr(),
            self.meta_buffer_tmp.len(),
        )
        .cast::<K32Meta2>();
        let out_pairs = Span::new(self.pairs.as_mut_ptr(), self.pairs.len());
        let thread_count = self.config.thread_count;
        let mut table_context = TableContext {
            context: self,
            f1_generator: if let Some(f1) = &req.f1_generator {
                f1.clone()
            } else {
                Arc::new(F1Generator::new(k, thread_count, req.plot_id.as_ref()))
            },
            entries_per_bucket: entries_per_bucket as isize,
            out_y,
            out_meta,
            out_pairs,
        };
        for (i, j) in (0..num_groups).zip((0..x_groups.len()).step_by(2)) {
            let x1 = u64::from(x_groups[j]);
            let x2 = u64::from(x_groups[j + 1]);
            let group_index = i / 2;
            let table: &mut ProofTable = &mut tables[1usize];
            if i % 2 == 0 {
                table.begin_group(group_index);
            }
            if let Err(e) =
                Self::process_table1bucket(k, req.c_level, &mut table_context, x1, x2, group_index)
            {
                error!("Error Processing Table1 Bucket: {e:?}");
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Error Processing Table1 Bucket: {e:?}"),
                ));
            }
        }

        // #NOTE: Sanity check, but should never happen w/ our starting compression levels.
        if self.tables[1].length <= 2 {
            error!("Unexpected proof match on first table.");
            Err(Error::new(
                ErrorKind::InvalidData,
                "Unexpected proof match on first table.",
            ))
        } else {
            // Continue forward propagation to the next table
            self.proof_context = Some(ProofContext {
                left_length: self.tables[1].length,
                right_length: 0,
                y_left: Span::new(self.y_buffer.as_mut_ptr(), self.y_buffer.len()),
                meta_left: Span::new(self.meta_buffer.as_mut_ptr(), self.meta_buffer.len()),
                y_right: Span::new(self.y_buffer_tmp.as_mut_ptr(), self.y_buffer_tmp.len()),
                meta_right: Span::new(
                    self.meta_buffer_tmp.as_mut_ptr(),
                    self.meta_buffer_tmp.len(),
                ),
                proof: [0; PROOF_X_COUNT],
            });
            debug!("Sorting Table 2 and Flipping Buffers");
            Self::sort_table2_and_flip_buffers(
                self.config.thread_count,
                &mut self.proof_context,
                Span::new(self.pairs.as_mut_ptr(), self.pairs.len()),
                &mut self.tables,
                Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
                Span::new(self.x_buffer.as_mut_ptr(), self.x_buffer.len()),
                32 >> PlotTable::Table2 as u8,
            )?;
            debug!("Forwarding Prop Tables:");
            if let Err(e) = self.forward_prop_tables(k, req.c_level) {
                Err(e)
            } else {
                req.full_proof.copy_from_slice(
                    &self
                        .proof_context
                        .as_ref()
                        .expect("Expected Context to Copy From")
                        .proof,
                );
                Ok(())
            }
        }
    }

    pub fn request_setup(&mut self, k: u8, c_level: u8) -> Result<(), Error> {
        if (1..=9).contains(&c_level) {
            self.prealloc_for_clevel(k, c_level);
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid Compression level: {c_level}"),
            ))
        }
    }

    pub fn forward_prop_tables(&mut self, k: u8, c_level: u8) -> Result<(), Error> {
        let thread_count = self.config.thread_count;
        for r_table in [
            PlotTable::Table3,
            PlotTable::Table4,
            PlotTable::Table5,
            PlotTable::Table6,
        ] {
            debug!("\tStarting {:?}", r_table);
            let group_count = 32 >> (r_table as u8 - 1);
            let res: ForwardPropResult = match r_table {
                PlotTable::Table3 => {
                    self.forward_prop_table3(thread_count, group_count, false, k, c_level)?
                }
                PlotTable::Table4 => {
                    self.forward_prop_table4(thread_count, group_count, false, k, c_level)?
                }
                PlotTable::Table5 => {
                    self.forward_prop_table5(thread_count, group_count, false, k, c_level)?
                }
                PlotTable::Table6 => {
                    self.forward_prop_table6(thread_count, group_count, true, k, c_level)?
                }
                _ => {
                    //Should never occur
                    return Err(Error::new(
                        ErrorKind::Other,
                        "Unexpected Table Value in Forward Prop Tables",
                    ));
                }
            };
            match res {
                ForwardPropResult::Failed(e) => {
                    return Err(e);
                }
                ForwardPropResult::Success => {
                    return self.backtrace_proof(min(r_table, PlotTable::Table6));
                }
                ForwardPropResult::Continue => {}
            }
        }
        Err(Error::new(
            ErrorKind::Other,
            "Forward Prop Failed to complete",
        ))
    }
    #[allow(clippy::cast_sign_loss)]
    pub fn backtrace_proof(&mut self, table: PlotTable) -> Result<(), Error> {
        let mut backtrace: [[Pair; 64]; 2] = [[Pair::zero(); 64]; 2];
        let back_trace_in = &mut Span::new(backtrace[0].as_mut_ptr(), backtrace[0].len());
        let back_trace_out = &mut Span::new(backtrace[1].as_mut_ptr(), backtrace[1].len());
        let pair_src;
        {
            let table = &mut self.tables[table as usize];
            if self.gpu_context.is_some() {
                todo!()
            } else {
                pair_src = Span::new(table.pairs.as_mut_ptr(), table.length);
            }
        }
        unsafe {
            back_trace_in
                .ptr()
                .copy_from(pair_src.ptr(), pair_src.len() as usize);
        }
        let mut table_id = table as usize;
        while table_id > PlotTable::Table2 as usize {
            let entry_count = (32isize >> table_id) * 2;
            let l_table = &mut self.tables[table_id - 1];
            for i in 0..entry_count {
                let p = &back_trace_in[i];
                let idx = i * 2;
                back_trace_out[idx] = l_table.pairs[p.left as usize];
                back_trace_out[idx + 1] = l_table.pairs[p.right as usize];
            }
            swap(back_trace_in, back_trace_out);
            table_id -= 1;
        }
        let proof_context = self.proof_context.as_mut().expect("Expected Proof Context");
        for i in 0..POST_PROOF_CMP_X_COUNT {
            let idx = i * 2;
            let p = &back_trace_in[i];
            proof_context.proof[idx] = u64::from(p.left);
            proof_context.proof[idx + 1] = u64::from(p.right);
        }
        Ok(())
    }
    pub fn sort_table2_and_flip_buffers(
        thread_count: u8,
        proof_context: &mut Option<ProofContext>,
        pairs: Span<Pair>,
        tables: &mut [ProofTable],
        x_buffer_tmp: Span<u32>,
        x_buffer: Span<u32>,
        group_count: usize,
    ) -> Result<(), Error> {
        if let Some(proof_context) = proof_context {
            let table = &mut tables[PlotTable::Table2 as usize];
            let table_length = table.length;
            // At this point the yRight/metaRight hold the unsorted fx output
            // from the left table pairs/y/meta and sort onto the right buffers
            let mut table_yunsorted = proof_context.y_right.slice_size(table_length);
            let mut table_ysorted = proof_context.y_left.slice_size(table_length);

            //MAP TO U64
            let mut table_meta_unsorted = proof_context
                .meta_right
                .cast::<K32Meta2>()
                .slice_size(table_length);
            let mut table_meta_sorted = proof_context
                .meta_left
                .cast::<K32Meta2>()
                .slice_size(table_length);

            let mut table_pairs_unsorted = pairs.slice_size(table_length);
            let mut table_pairs_sorted = Span::new(table.pairs.as_mut_ptr(), table_length);
            let key_unsorted = x_buffer_tmp.slice_size(table_length);
            let key_sorted = x_buffer.slice_size(table_length);
            for i in 0..group_count {
                let group_length = table.groups[i].count as usize;
                let group_threads = min(thread_count as usize, group_length);
                let mut sorter = RadixSorter::new(group_threads, group_length);
                let mut y_unsorted = table_yunsorted.slice_size(group_length);
                let mut y_sorted = table_ysorted.slice_size(group_length);
                let meta_unsorted = table_meta_unsorted.slice_size(group_length);
                let mut meta_sorted = table_meta_sorted.slice_size(group_length);
                let pairs_unsorted = table_pairs_unsorted.slice_size(group_length);
                let mut pairs_sorted = table_pairs_sorted.slice_size(group_length);
                let mut k_unsorted = key_unsorted.slice_size(group_length);
                let mut k_sorted = key_sorted.slice_size(group_length);
                if i != group_count - 1 {
                    table_yunsorted = table_yunsorted.slice(group_length);
                    table_ysorted = table_ysorted.slice(group_length);
                    table_meta_unsorted = table_meta_unsorted.slice(group_length);
                    table_meta_sorted = table_meta_sorted.slice(group_length);
                    table_pairs_unsorted = table_pairs_unsorted.slice(group_length);
                    table_pairs_sorted = table_pairs_sorted.slice(group_length);
                }
                sorter.generate_key(k_unsorted.as_mut());
                sorter.sort_keyed(
                    5,
                    y_unsorted.as_mut(),
                    y_sorted.as_mut(),
                    k_unsorted.as_mut(),
                    k_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    meta_unsorted.as_ref(),
                    meta_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    pairs_unsorted.as_ref(),
                    pairs_sorted.as_mut(),
                );
            }
            proof_context.left_length = table_length;
            proof_context.right_length = tables[PlotTable::Table3 as usize].capacity;
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "Invalid Proof Context"))
        }
    }
    pub fn sort_table3_and_flip_buffers(
        thread_count: u8,
        proof_context: &mut Option<ProofContext>,
        pairs: Span<Pair>,
        tables: &mut [ProofTable],
        x_buffer_tmp: Span<u32>,
        x_buffer: Span<u32>,
        group_count: usize,
    ) -> Result<(), Error> {
        if let Some(proof_context) = proof_context {
            let table = &mut tables[PlotTable::Table3 as usize];
            let table_length = table.length;
            // At this point the yRight/metaRight hold the unsorted fx output
            // from the left table pairs/y/meta and sort onto the right buffers
            let mut table_yunsorted = proof_context.y_right.slice_size(table_length);
            let mut table_ysorted = proof_context.y_left.slice_size(table_length);

            //MAP TO U64
            let mut table_meta_unsorted = proof_context.meta_right.slice_size(table_length);
            let mut table_meta_sorted = proof_context.meta_left.slice_size(table_length);

            let mut table_pairs_unsorted = pairs.slice_size(table_length);
            let mut table_pairs_sorted = Span::new(table.pairs.as_mut_ptr(), table_length);
            let key_unsorted = x_buffer_tmp.slice_size(table_length);
            let key_sorted = x_buffer.slice_size(table_length);
            for i in 0..group_count {
                let group_length = table.groups[i].count as usize;
                let group_threads = min(thread_count as usize, group_length);
                let mut sorter = RadixSorter::new(group_threads, group_length);
                let mut y_unsorted = table_yunsorted.slice_size(group_length);
                let mut y_sorted = table_ysorted.slice_size(group_length);
                let meta_unsorted = table_meta_unsorted.slice_size(group_length);
                let mut meta_sorted = table_meta_sorted.slice_size(group_length);
                let pairs_unsorted = table_pairs_unsorted.slice_size(group_length);
                let mut pairs_sorted = table_pairs_sorted.slice_size(group_length);
                let mut k_unsorted = key_unsorted.slice_size(group_length);
                let mut k_sorted = key_sorted.slice_size(group_length);
                if i != group_count - 1 {
                    table_yunsorted = table_yunsorted.slice(group_length);
                    table_ysorted = table_ysorted.slice(group_length);
                    table_meta_unsorted = table_meta_unsorted.slice(group_length);
                    table_meta_sorted = table_meta_sorted.slice(group_length);
                    table_pairs_unsorted = table_pairs_unsorted.slice(group_length);
                    table_pairs_sorted = table_pairs_sorted.slice(group_length);
                }
                sorter.generate_key(k_unsorted.as_mut());
                sorter.sort_keyed(
                    5,
                    y_unsorted.as_mut(),
                    y_sorted.as_mut(),
                    k_unsorted.as_mut(),
                    k_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    meta_unsorted.as_ref(),
                    meta_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    pairs_unsorted.as_ref(),
                    pairs_sorted.as_mut(),
                );
            }
            proof_context.left_length = table_length;
            proof_context.right_length = tables[PlotTable::Table4 as usize].capacity;
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "Invalid Proof Context"))
        }
    }
    pub fn sort_table4_and_flip_buffers(
        thread_count: u8,
        proof_context: &mut Option<ProofContext>,
        pairs: Span<Pair>,
        tables: &mut [ProofTable],
        x_buffer_tmp: Span<u32>,
        x_buffer: Span<u32>,
        group_count: usize,
    ) -> Result<(), Error> {
        if let Some(proof_context) = proof_context {
            let table = &mut tables[PlotTable::Table4 as usize];
            let table_length = table.length;
            // At this point the yRight/metaRight hold the unsorted fx output
            // from the left table pairs/y/meta and sort onto the right buffers
            let mut table_yunsorted = proof_context.y_right.slice_size(table_length);
            let mut table_ysorted = proof_context.y_left.slice_size(table_length);

            //MAP TO U64
            let mut table_meta_unsorted = proof_context.meta_right.slice_size(table_length);
            let mut table_meta_sorted = proof_context.meta_left.slice_size(table_length);

            let mut table_pairs_unsorted = pairs.slice_size(table_length);
            let mut table_pairs_sorted = Span::new(table.pairs.as_mut_ptr(), table_length);
            let key_unsorted = x_buffer_tmp.slice_size(table_length);
            let key_sorted = x_buffer.slice_size(table_length);
            for i in 0..group_count {
                let group_length = table.groups[i].count as usize;
                let group_threads = min(thread_count as usize, group_length);
                let mut sorter = RadixSorter::new(group_threads, group_length);
                let mut y_unsorted = table_yunsorted.slice_size(group_length);
                let mut y_sorted = table_ysorted.slice_size(group_length);
                let meta_unsorted = table_meta_unsorted.slice_size(group_length);
                let mut meta_sorted = table_meta_sorted.slice_size(group_length);
                let pairs_unsorted = table_pairs_unsorted.slice_size(group_length);
                let mut pairs_sorted = table_pairs_sorted.slice_size(group_length);
                let mut k_unsorted = key_unsorted.slice_size(group_length);
                let mut k_sorted = key_sorted.slice_size(group_length);
                if i != group_count - 1 {
                    table_yunsorted = table_yunsorted.slice(group_length);
                    table_ysorted = table_ysorted.slice(group_length);
                    table_meta_unsorted = table_meta_unsorted.slice(group_length);
                    table_meta_sorted = table_meta_sorted.slice(group_length);
                    table_pairs_unsorted = table_pairs_unsorted.slice(group_length);
                    table_pairs_sorted = table_pairs_sorted.slice(group_length);
                }
                sorter.generate_key(k_unsorted.as_mut());
                sorter.sort_keyed(
                    5,
                    y_unsorted.as_mut(),
                    y_sorted.as_mut(),
                    k_unsorted.as_mut(),
                    k_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    meta_unsorted.as_ref(),
                    meta_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    pairs_unsorted.as_ref(),
                    pairs_sorted.as_mut(),
                );
            }
            proof_context.left_length = table_length;
            proof_context.right_length = tables[PlotTable::Table5 as usize].capacity;
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "Invalid Proof Context"))
        }
    }

    pub fn sort_table5_and_flip_buffers(
        thread_count: u8,
        proof_context: &mut Option<ProofContext>,
        pairs: Span<Pair>,
        tables: &mut [ProofTable],
        x_buffer_tmp: Span<u32>,
        x_buffer: Span<u32>,
        group_count: usize,
    ) -> Result<(), Error> {
        if let Some(proof_context) = proof_context {
            let table = &mut tables[PlotTable::Table5 as usize];
            let table_length = table.length;
            // At this point the yRight/metaRight hold the unsorted fx output
            // from the left table pairs/y/meta and sort onto the right buffers
            let mut table_yunsorted = proof_context.y_right.slice_size(table_length);
            let mut table_ysorted = proof_context.y_left.slice_size(table_length);
            let mut table_meta_unsorted = proof_context.meta_right.slice_size(table_length);
            let mut table_meta_sorted = proof_context.meta_left.slice_size(table_length);
            let mut table_pairs_unsorted = pairs.slice_size(table_length);
            let mut table_pairs_sorted = Span::new(table.pairs.as_mut_ptr(), table_length);
            let key_unsorted = x_buffer_tmp.slice_size(table_length);
            let key_sorted = x_buffer.slice_size(table_length);
            for i in 0..group_count {
                let group_length = table.groups[i].count as usize;
                let group_threads = min(thread_count as usize, group_length);
                let mut sorter = RadixSorter::new(group_threads, group_length);
                let mut y_unsorted = table_yunsorted.slice_size(group_length);
                let mut y_sorted = table_ysorted.slice_size(group_length);
                let meta_unsorted = table_meta_unsorted.slice_size(group_length);
                let mut meta_sorted = table_meta_sorted.slice_size(group_length);
                let pairs_unsorted = table_pairs_unsorted.slice_size(group_length);
                let mut pairs_sorted = table_pairs_sorted.slice_size(group_length);
                let mut k_unsorted = key_unsorted.slice_size(group_length);
                let mut k_sorted = key_sorted.slice_size(group_length);
                if i != group_count - 1 {
                    table_yunsorted = table_yunsorted.slice(group_length);
                    table_ysorted = table_ysorted.slice(group_length);
                    table_meta_unsorted = table_meta_unsorted.slice(group_length);
                    table_meta_sorted = table_meta_sorted.slice(group_length);
                    table_pairs_unsorted = table_pairs_unsorted.slice(group_length);
                    table_pairs_sorted = table_pairs_sorted.slice(group_length);
                }
                sorter.generate_key(k_unsorted.as_mut());
                sorter.sort_keyed(
                    5,
                    y_unsorted.as_mut(),
                    y_sorted.as_mut(),
                    k_unsorted.as_mut(),
                    k_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    meta_unsorted.as_ref(),
                    meta_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    pairs_unsorted.as_ref(),
                    pairs_sorted.as_mut(),
                );
            }
            proof_context.left_length = table_length;
            proof_context.right_length = tables[PlotTable::Table6 as usize].capacity;
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "Invalid Proof Context"))
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn sort_table6_and_flip_buffers(
        thread_count: u8,
        proof_context: &mut Option<ProofContext>,
        pairs: Span<Pair>,
        tables: &mut [ProofTable],
        x_buffer_tmp: Span<u32>,
        x_buffer: Span<u32>,
        group_count: usize,
    ) -> Result<(), Error> {
        if let Some(proof_context) = proof_context {
            let table = &mut tables[PlotTable::Table6 as usize];
            let table_length = table.length;
            // At this point the yRight/metaRight hold the unsorted fx output
            // from the left table pairs/y/meta and sort onto the right buffers
            let mut table_yunsorted = proof_context.y_right.slice_size(table_length);
            let mut table_ysorted = proof_context.y_left.slice_size(table_length);
            //MAP TO U64
            let mut table_meta_unsorted = proof_context
                .meta_right
                .cast::<K32Meta2>()
                .slice_size(table_length);
            let mut table_meta_sorted = proof_context
                .meta_left
                .cast::<K32Meta2>()
                .slice_size(table_length);
            let mut table_pairs_unsorted = pairs.slice_size(table_length);
            let mut table_pairs_sorted = Span::new(table.pairs.as_mut_ptr(), table_length);
            let key_unsorted = x_buffer_tmp.slice_size(table_length);
            let key_sorted = x_buffer.slice_size(table_length);
            for i in 0..group_count {
                let group_length = table.groups[i].count as usize;
                let group_threads = min(thread_count as usize, group_length);
                let mut sorter = RadixSorter::new(group_threads, group_length);
                let mut y_unsorted = table_yunsorted.slice_size(group_length);
                let mut y_sorted = table_ysorted.slice_size(group_length);
                let meta_unsorted = table_meta_unsorted.slice_size(group_length);
                let mut meta_sorted = table_meta_sorted.slice_size(group_length);
                let pairs_unsorted = table_pairs_unsorted.slice_size(group_length);
                let mut pairs_sorted = table_pairs_sorted.slice_size(group_length);
                let mut k_unsorted = key_unsorted.slice_size(group_length);
                let mut k_sorted = key_sorted.slice_size(group_length);
                if i != group_count - 1 {
                    table_yunsorted = table_yunsorted.slice(group_length);
                    table_ysorted = table_ysorted.slice(group_length);
                    table_meta_unsorted = table_meta_unsorted.slice(group_length);
                    table_meta_sorted = table_meta_sorted.slice(group_length);
                    table_pairs_unsorted = table_pairs_unsorted.slice(group_length);
                    table_pairs_sorted = table_pairs_sorted.slice(group_length);
                }
                sorter.generate_key(k_unsorted.as_mut());
                sorter.sort_keyed(
                    5,
                    y_unsorted.as_mut(),
                    y_sorted.as_mut(),
                    k_unsorted.as_mut(),
                    k_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    meta_unsorted.as_ref(),
                    meta_sorted.as_mut(),
                );
                sorter.sort_on_key(
                    k_sorted.as_ref(),
                    pairs_unsorted.as_ref(),
                    pairs_sorted.as_mut(),
                );
            }
            proof_context.left_length = table_length;
            proof_context.right_length = tables[PlotTable::Table7 as usize].capacity;
            Ok(())
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "Invalid Proof Context"))
        }
    }

    pub fn process_table1bucket(
        k: u8,
        c_level: u8,
        table_ctx: &mut TableContext,
        x1: u64,
        x2: u64,
        group_index: usize,
    ) -> Result<usize, Error> {
        if table_ctx.context.gpu_context.is_some() {
            todo!()
        } else {
            Self::process_table1bucket_cpu(k, c_level, table_ctx, x1, x2, group_index)
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub fn process_table1bucket_cpu(
        k: u8,
        c_level: u8,
        table_ctx: &mut TableContext,
        x1: u64,
        x2: u64,
        group_index: usize,
    ) -> Result<usize, Error> {
        let might_be_dropped = x1 == 0 || x2 == 0;
        let config_thread_count = table_ctx.context.config.thread_count;
        debug!("\tGenerating F1:");
        {
            table_ctx.f1_generator.generate_f1(
                table_ctx.entries_per_bucket as usize,
                [x1 as u32, x2 as u32],
                &mut table_ctx.context.x_buffer_tmp,
                &mut table_ctx.context.y_buffer_f1,
                &mut table_ctx.context.x_buffer,
                &mut table_ctx.context.y_buffer,
            )?;
        }
        debug!("\tMatching Pairs");
        let y_entries = Span::new(
            table_ctx.context.y_buffer.as_mut_ptr(),
            table_ctx.context.y_buffer.len(),
        );
        let x_entries = Span::new(
            table_ctx.context.x_buffer.as_mut_ptr(),
            table_ctx.context.x_buffer.len(),
        );
        let pairs = Self::match_pairs(
            table_ctx.context,
            y_entries,
            table_ctx.out_pairs,
            0,
            k,
            c_level,
        );
        if pairs.is_empty() {
            return if might_be_dropped {
                Err(Error::new(ErrorKind::Other, "Proof Dropped"))
            } else {
                Err(Error::new(ErrorKind::Other, "Failed to load Proof"))
            };
        }
        {
            let table = &mut table_ctx.context.tables[1];
            table.add_group_pairs(group_index, pairs.len() as u32);
        }
        debug!("\tGenerating FX");
        generate_fx_for_pairs_table2(
            config_thread_count as usize,
            k,
            pairs,
            y_entries,
            x_entries,
            table_ctx.out_y,
            table_ctx.out_meta,
        );
        let total_count = pairs.len() as usize;
        let thread_count = min(config_thread_count as usize, total_count);
        let x_buffer = Span::new(
            table_ctx.context.x_buffer.as_mut_ptr(),
            table_ctx.context.x_buffer.len(),
        );
        debug!("\tWriting Table Pairs");
        (0..thread_count).into_par_iter().for_each(|i| {
            let thread_vars = calc_thread_vars(i, thread_count, total_count);
            let mut pairs = pairs.range(thread_vars.offset, thread_vars.count);
            for pair in &mut pairs[0..thread_vars.count] {
                pair.left = x_buffer[pair.left as usize];
                pair.right = x_buffer[pair.right as usize];
            }
        });
        let pairs_length = pairs.len() as usize;
        table_ctx.out_pairs = table_ctx.out_pairs.slice(pairs_length);
        table_ctx.out_y = table_ctx.out_y.slice(pairs_length);
        table_ctx.out_meta = table_ctx.out_meta.slice(pairs_length);
        Ok(pairs_length)
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_sign_loss)]
    pub fn forward_prop_table3(
        &mut self,
        thread_count: u8,
        group_count: usize,
        return_success_on_single_match: bool,
        k: u8,
        c_level: u8,
    ) -> Result<ForwardPropResult, Error> {
        let table_capacity = self.tables[PlotTable::Table3 as usize].capacity;
        let mut y_right = self
            .proof_context
            .as_mut()
            .expect("Expected Proof Context")
            .y_right
            .slice_size(table_capacity);
        let mut meta_right = self
            .proof_context
            .as_mut()
            .expect("Expected Proof Context")
            .meta_right
            .slice_size(table_capacity);
        let mut out_pairs = Span::new(self.pairs.as_mut_ptr(), table_capacity);
        let mut table_match_count = 0;
        for l_group in 0..group_count {
            let match_count = {
                let y_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.y_left.ptr().offset(
                            self.tables[PlotTable::Table2 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table2 as usize].groups[l_group].count,
                    )
                };
                let _meta_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.meta_left.cast::<K32Meta2>().ptr().offset(
                            self.tables[PlotTable::Table2 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table2 as usize].groups[l_group].count,
                    )
                };
                let r_group = l_group / 2;
                if l_group & 1 == 0 {
                    self.tables[PlotTable::Table3 as usize].begin_group(r_group);
                }
                if let Some(_c) = &self.gpu_context {
                    todo!()
                } else {
                    let pairs = Self::match_pairs(
                        self,
                        y_left,
                        out_pairs,
                        self.tables[PlotTable::Table2 as usize].groups[l_group].offset,
                        k,
                        c_level,
                    );
                    if pairs.len() as u32 > table_capacity || pairs.is_empty() {
                        0
                    } else {
                        // Since pairs have the global L table offset applied to them,
                        // we need to turn the left values back to global table y and meta, instead
                        // of group-local y and meta
                        let thread_count = min(thread_count as usize, pairs.len() as usize);
                        let proof_context =
                            self.proof_context.as_mut().expect("Expected Proof Context");
                        generate_fx_for_pairs_table3(
                            k,
                            thread_count,
                            pairs,
                            proof_context
                                .y_left
                                .slice_size(proof_context.left_length as usize),
                            proof_context
                                .meta_left
                                .slice_size(proof_context.left_length as usize)
                                .cast(),
                            y_right,
                            meta_right,
                        );
                        self.tables[PlotTable::Table3 as usize]
                            .add_group_pairs(r_group, pairs.len() as u32);
                        pairs.len() as usize
                    }
                }
            };
            if match_count == 0 {
                return Ok(ForwardPropResult::Failed(Error::new(
                    ErrorKind::Other,
                    "No Matches",
                )));
            }
            table_match_count += match_count;
            out_pairs = out_pairs.slice(match_count);
            meta_right = meta_right.slice(match_count);
            y_right = y_right.slice(match_count);
        }
        let groups_to_flip = max(1, group_count / 2);
        Self::sort_table3_and_flip_buffers(
            self.config.thread_count,
            &mut self.proof_context,
            Span::new(self.pairs.as_mut_ptr(), self.pairs.len()),
            &mut self.tables,
            Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
            Span::new(self.x_buffer.as_mut_ptr(), self.x_buffer.len()),
            groups_to_flip,
        )?;
        if (return_success_on_single_match && table_match_count == 1) || table_match_count == 2 {
            Ok(ForwardPropResult::Success)
        } else {
            Ok(ForwardPropResult::Continue)
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    pub fn forward_prop_table4(
        &mut self,
        thread_count: u8,
        group_count: usize,
        return_success_on_single_match: bool,
        k: u8,
        c_level: u8,
    ) -> Result<ForwardPropResult, Error> {
        let mut table_match_count = 0;
        let table_capacity = self.tables[PlotTable::Table4 as usize].capacity;
        let mut y_right;
        let mut meta_right;
        {
            let proof_context = self.proof_context.as_mut().expect("Expected Proof Context");
            y_right = proof_context.y_right.slice_size(table_capacity);
            meta_right = proof_context.meta_right.slice_size(table_capacity);
        }
        let mut out_pairs = Span::new(self.pairs.as_mut_ptr(), table_capacity);
        for l_group in 0..group_count {
            let match_count = {
                let y_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.y_left.ptr().offset(
                            self.tables[PlotTable::Table3 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table3 as usize].groups[l_group].count,
                    )
                };
                let _meta_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.meta_left.ptr().offset(
                            self.tables[PlotTable::Table3 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table3 as usize].groups[l_group].count,
                    )
                };
                let r_group = l_group / 2;
                if l_group & 1 == 0 {
                    self.tables[PlotTable::Table4 as usize].begin_group(r_group);
                }
                if let Some(_c) = &self.gpu_context {
                    todo!()
                } else {
                    let pairs = Self::match_pairs(
                        self,
                        y_left,
                        out_pairs,
                        self.tables[PlotTable::Table3 as usize].groups[l_group].offset,
                        k,
                        c_level,
                    );
                    if pairs.len() as u32 > table_capacity || pairs.is_empty() {
                        0
                    } else {
                        // Since pairs have the global L table offset applied to them,
                        // we need to turn the left values back to global table y and meta, instead
                        // of group-local y and meta
                        let thread_count = min(thread_count as usize, pairs.len() as usize);
                        let proof_context =
                            self.proof_context.as_mut().expect("Expected Proof Context");
                        generate_fx_for_pairs_table4(
                            k,
                            thread_count,
                            pairs,
                            proof_context
                                .y_left
                                .slice_size(proof_context.left_length as usize),
                            proof_context
                                .meta_left
                                .slice_size(proof_context.left_length as usize)
                                .cast(),
                            y_right,
                            meta_right,
                        );
                        self.tables[PlotTable::Table4 as usize]
                            .add_group_pairs(r_group, pairs.len() as u32);
                        pairs.len() as usize
                    }
                }
            };
            if match_count == 0 {
                return Ok(ForwardPropResult::Failed(Error::new(
                    ErrorKind::Other,
                    "No Matches",
                )));
            }
            out_pairs = out_pairs.slice(match_count);
            meta_right = meta_right.slice(match_count);
            y_right = y_right.slice(match_count);
            table_match_count += match_count;
        }
        let groups_to_flip = max(1, group_count / 2);
        Self::sort_table4_and_flip_buffers(
            self.config.thread_count,
            &mut self.proof_context,
            Span::new(self.pairs.as_mut_ptr(), self.pairs.len()),
            &mut self.tables,
            Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
            Span::new(self.x_buffer.as_mut_ptr(), self.x_buffer.len()),
            groups_to_flip,
        )?;
        if (return_success_on_single_match && table_match_count == 1) || table_match_count == 2 {
            Ok(ForwardPropResult::Success)
        } else {
            Ok(ForwardPropResult::Continue)
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    pub fn forward_prop_table5(
        &mut self,
        thread_count: u8,
        group_count: usize,
        return_success_on_single_match: bool,
        k: u8,
        c_level: u8,
    ) -> Result<ForwardPropResult, Error> {
        let mut table_match_count = 0;
        let table_capacity = self.tables[PlotTable::Table5 as usize].capacity;
        let mut y_right;
        let mut meta_right;
        {
            let proof_context = self.proof_context.as_mut().expect("Expected Proof Context");
            y_right = proof_context.y_right.slice_size(table_capacity);
            meta_right = proof_context.meta_right.slice_size(table_capacity);
        }
        let mut out_pairs = Span::new(self.pairs.as_mut_ptr(), table_capacity);
        for l_group in 0..group_count {
            let match_count = {
                let y_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.y_left.ptr().offset(
                            self.tables[PlotTable::Table4 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table4 as usize].groups[l_group].count,
                    )
                };
                let _meta_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.meta_left.ptr().offset(
                            self.tables[PlotTable::Table4 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table4 as usize].groups[l_group].count,
                    )
                };
                let r_group = l_group / 2;
                if l_group & 1 == 0 {
                    self.tables[PlotTable::Table5 as usize].begin_group(r_group);
                }
                if let Some(_c) = &self.gpu_context {
                    todo!()
                } else {
                    let pairs = Self::match_pairs(
                        self,
                        y_left,
                        out_pairs,
                        self.tables[PlotTable::Table4 as usize].groups[l_group].offset,
                        k,
                        c_level,
                    );
                    if pairs.len() as u32 > table_capacity || pairs.is_empty() {
                        0
                    } else {
                        let thread_count = min(thread_count as usize, pairs.len() as usize);
                        let proof_context =
                            self.proof_context.as_mut().expect("Expected Proof Context");
                        generate_fx_for_pairs_table5(
                            k,
                            thread_count,
                            pairs,
                            proof_context
                                .y_left
                                .slice_size(proof_context.left_length as usize),
                            proof_context
                                .meta_left
                                .slice_size(proof_context.left_length as usize),
                            y_right,
                            meta_right,
                        );
                        self.tables[PlotTable::Table5 as usize]
                            .add_group_pairs(r_group, pairs.len() as u32);
                        pairs.len() as usize
                    }
                }
            };
            if match_count == 0 {
                return Ok(ForwardPropResult::Failed(Error::new(
                    ErrorKind::Other,
                    "No Matches",
                )));
            }
            out_pairs = out_pairs.slice(match_count);
            meta_right = meta_right.slice(match_count);
            y_right = y_right.slice(match_count);
            table_match_count += match_count;
        }
        let groups_to_flip = max(1, group_count / 2);
        Self::sort_table5_and_flip_buffers(
            self.config.thread_count,
            &mut self.proof_context,
            Span::new(self.pairs.as_mut_ptr(), self.pairs.len()),
            &mut self.tables,
            Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
            Span::new(self.x_buffer.as_mut_ptr(), self.x_buffer.len()),
            groups_to_flip,
        )?;
        if (return_success_on_single_match && table_match_count == 1) || table_match_count == 2 {
            Ok(ForwardPropResult::Success)
        } else {
            Ok(ForwardPropResult::Continue)
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    pub fn forward_prop_table6(
        &mut self,
        thread_count: u8,
        group_count: usize,
        return_success_on_single_match: bool,
        k: u8,
        c_level: u8,
    ) -> Result<ForwardPropResult, Error> {
        let mut table_match_count = 0;
        let table_capacity = self.tables[PlotTable::Table6 as usize].capacity;
        let mut y_right;
        let mut meta_right;
        {
            let proof_context = self.proof_context.as_mut().expect("Expected Proof Context");
            y_right = proof_context.y_right.slice_size(table_capacity);
            meta_right = proof_context
                .meta_right
                .cast::<K32Meta2>()
                .slice_size(table_capacity);
        }
        let mut out_pairs = Span::new(self.pairs.as_mut_ptr(), table_capacity);
        for l_group in 0..group_count {
            let match_count = {
                let y_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.y_left.ptr().offset(
                            self.tables[PlotTable::Table5 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table5 as usize].groups[l_group].count,
                    )
                };
                let _meta_left = unsafe {
                    let proof_context =
                        self.proof_context.as_mut().expect("Expected Proof Context");
                    Span::new(
                        proof_context.meta_left.ptr().offset(
                            self.tables[PlotTable::Table5 as usize].groups[l_group].offset as isize,
                        ),
                        self.tables[PlotTable::Table5 as usize].groups[l_group].count,
                    )
                };
                //let _meta_left = &proof_context.meta_left[prev_table.groups[l_group].offset..prev_table.groups[l_group].count];
                let r_group = l_group / 2;
                if l_group & 1 == 0 {
                    self.tables[PlotTable::Table6 as usize].begin_group(r_group);
                }
                if let Some(_c) = &self.gpu_context {
                    todo!()
                } else {
                    let pairs = self.match_pairs(
                        y_left,
                        out_pairs,
                        self.tables[PlotTable::Table5 as usize].groups[l_group].offset,
                        k,
                        c_level,
                    );
                    if pairs.len() as u32 > table_capacity || pairs.is_empty() {
                        0
                    } else {
                        // Since pairs have the global L table offset applied to them,
                        // we need to turn the left values back to global table y and meta, instead
                        // of group-local y and meta
                        let thread_count = min(thread_count as usize, pairs.len() as usize);
                        let proof_context =
                            self.proof_context.as_mut().expect("Expected Proof Context");
                        generate_fx_for_pairs_table6(
                            k,
                            thread_count,
                            pairs,
                            proof_context
                                .y_left
                                .slice_size(proof_context.left_length as usize),
                            proof_context
                                .meta_left
                                .slice_size(proof_context.left_length as usize),
                            y_right,
                            meta_right,
                        );
                        self.tables[PlotTable::Table6 as usize]
                            .add_group_pairs(r_group, pairs.len() as u32);
                        pairs.len() as usize
                    }
                }
            };
            if match_count == 0 {
                return Ok(ForwardPropResult::Failed(Error::new(
                    ErrorKind::Other,
                    "No Matches",
                )));
            }
            table_match_count += match_count;
            out_pairs = out_pairs.slice(match_count);
            meta_right = meta_right.slice(match_count);
            y_right = y_right.slice(match_count);
        }
        let groups_to_flip = max(1, group_count / 2);
        Self::sort_table6_and_flip_buffers(
            self.config.thread_count,
            &mut self.proof_context,
            Span::new(self.pairs.as_mut_ptr(), self.pairs.len()),
            &mut self.tables,
            Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
            Span::new(self.x_buffer.as_mut_ptr(), self.x_buffer.len()),
            groups_to_flip,
        )?;
        if (return_success_on_single_match && table_match_count == 1) || table_match_count == 2 {
            Ok(ForwardPropResult::Success)
        } else {
            Ok(ForwardPropResult::Continue)
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    #[allow(clippy::too_many_lines)]
    pub fn match_pairs(
        &mut self,
        y_entries: Span<u64>,
        out_pairs: Span<Pair>,
        pair_offset: u32,
        k: u8,
        c_level: u8,
    ) -> Span<Pair> {
        assert!(y_entries.len() <= 0xFFFF_FFFF);
        debug!("\t\tGroup Scan");
        let group_count = Self::scan_groups(
            self.config.thread_count,
            y_entries,
            Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
            Span::new(
                self.groups_boundaries.as_mut_ptr(),
                self.groups_boundaries.len(),
            ),
            self.groups_boundaries.len(),
        );
        let groups_boundaries = Span::new(
            self.groups_boundaries.as_mut_ptr(),
            self.groups_boundaries.len(),
        );
        let match_thread_count = min(self.config.thread_count as usize, group_count as usize);
        let max_matches = max(
            out_pairs.len() as usize,
            get_max_table_pairs_for_compression_level(k, c_level),
        );
        let tmp_pairs = Span::new(self.pairs_tmp.as_mut_ptr(), self.pairs_tmp.len());
        debug!("\t\tMatching - Threads: {match_thread_count}, Group Count: {group_count}");
        let mut jobs = (0..match_thread_count)
            .into_par_iter()
            .map(|id| {
                let copy_offset = 0;
                let t_vars = calc_thread_vars(id, match_thread_count, group_count as usize);
                let (group_count, offset) = (t_vars.count, t_vars.offset);
                let t_vars = calc_thread_vars(id, match_thread_count, max_matches);
                let (max_matches, match_offset) = (t_vars.count, t_vars.offset);
                let tmp_pairs = tmp_pairs.range(match_offset, max_matches);
                let groups_boundaries = groups_boundaries.slice(offset);
                let mut pairs = tmp_pairs;
                let max_pairs = pairs.len() as usize;
                let mut pair_count = 0usize;
                let mut r_map_counts: [u8; K_BC] = [0; K_BC];
                let mut r_map_indices: [u16; K_BC] = [0; K_BC];
                let mut r_map_counts: Span<u8> = Span::new(r_map_counts.as_mut_ptr(), K_BC);
                let mut r_map_indices: Span<u16> = Span::new(r_map_indices.as_mut_ptr(), K_BC);
                let mut r_map_to_clear = vec![];
                let mut group_l_start = groups_boundaries[0isize];
                let mut group_l = y_entries[group_l_start as usize] / K_BC as u64;
                'outer: for i in 1..=group_count {
                    let group_rstart = groups_boundaries[i];
                    let group_r = y_entries[group_rstart as usize] / K_BC as u64;
                    if group_r.checked_sub(group_l) == Some(1) {
                        let parity = (group_l & 1) as u16;
                        let group_rend = groups_boundaries[i + 1];
                        let group_lrange_start = group_l * K_BC as u64;
                        let group_rrange_start = group_r * K_BC as u64;
                        debug_assert!(group_rend - group_rstart <= 350);
                        debug_assert_eq!(group_lrange_start, group_rrange_start - K_BC as u64);
                        for i in &r_map_to_clear {
                            r_map_counts[*i] = 0;
                        }
                        r_map_to_clear.clear();
                        for (i_r, y_entry) in y_entries[group_rstart as usize..group_rend as usize]
                            .iter()
                            .enumerate()
                        {
                            let local_ry = (y_entry - group_rrange_start) as usize;
                            debug_assert_eq!(y_entry / K_BC as u64, group_r);
                            let count_ref = &mut r_map_counts[local_ry];
                            if *count_ref == 0 {
                                r_map_indices[local_ry] = i_r as u16;
                                r_map_to_clear.push(local_ry);
                            }
                            *count_ref += 1;
                        }
                        // For each group L entry
                        for (i_l, y_l) in y_entries[group_l_start as usize..group_rstart as usize]
                            .iter()
                            .enumerate()
                        {
                            let i_l = i_l as u32 + group_l_start;
                            let local_l = *y_l - group_lrange_start;
                            let mut r_count;
                            for target_r in &L_TARGETS[parity as usize][local_l as usize]
                                [0..K_EXTRA_BITS_POW as usize]
                            {
                                r_count = u32::from(r_map_counts[*target_r as usize]);
                                let prefix =
                                    group_rstart + u32::from(r_map_indices[*target_r as usize]);
                                for i_r in prefix..(prefix + r_count) {
                                    debug_assert!(i_l < i_r);
                                    // Add a new pair
                                    if pair_count >= max_pairs {
                                        error!("Pair count is > Map Pairs, Invalid Proof");
                                        break 'outer;
                                    }
                                    let pair = &mut pairs[pair_count];
                                    pair.left = i_l + pair_offset;
                                    pair.right = i_r + pair_offset;
                                    pair_count += 1;
                                }
                            }
                        }
                    }
                    group_l = group_r;
                    group_l_start = group_rstart;
                }
                (copy_offset, pair_count, tmp_pairs)
            })
            .collect::<Vec<(isize, usize, Span<Pair>)>>();
        debug!("\t\tCopying Data");
        let mut copy_offset = 0;
        let mut index = 0;
        while index < jobs.len() {
            jobs[index].0 = copy_offset as isize * isize::from(index > 0);
            copy_offset += jobs[index].1;
            index += 1;
        }
        jobs.into_par_iter().for_each(|job| unsafe {
            copy_nonoverlapping(job.2.ptr(), out_pairs.ptr().offset(job.0), job.1);
        });
        out_pairs.slice_size(copy_offset)
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn scan_groups(
        thread_count: u8,
        y_entries: Span<u64>,
        tmp_group_entries: Span<u32>,
        out_group_entries: Span<u32>,
        max_groups: usize,
    ) -> u64 {
        let min_entries_per_threads = 10000;
        let entry_count = y_entries.len() as usize;
        let mut thread_count = min(thread_count as usize, entry_count);
        while thread_count > 1 && entry_count / thread_count < min_entries_per_threads {
            thread_count -= 1;
        }
        if max_groups < thread_count || max_groups < 3 {
            0
        } else {
            let group_count = Arc::new(AtomicU64::new(0));
            assert!(entry_count <= 0xFFFF_FFFF);
            let all_thread_vars = (0..thread_count)
                .collect::<Vec<usize>>()
                .into_iter()
                .map(|id| {
                    let mut thread_vars = calc_thread_vars(id, thread_count, entry_count);
                    let mut offset = thread_vars.offset;
                    // Find the start of our current group
                    let cur_group = y_entries[offset] / K_BC as u64;
                    while offset > 0 {
                        let group = y_entries[offset - 1] / K_BC as u64;
                        if group != cur_group {
                            break;
                        }
                        offset -= 1;
                    }
                    thread_vars.offset = offset;
                    thread_vars
                })
                .collect::<Vec<ThreadVars<usize>>>();
            let mut thread_results = all_thread_vars
                .iter()
                .enumerate()
                .map(|(id, thread_vars)| {
                    let end = if id == thread_count - 1 {
                        entry_count
                    } else {
                        all_thread_vars[id + 1].offset
                    };
                    let group_thread_vars = calc_thread_vars(id, thread_count, max_groups);
                    GroupScanJob {
                        id,
                        thread_vars: *thread_vars,
                        group_thread_vars,
                        group_indices: tmp_group_entries.ptr(),
                        final_group_entries: out_group_entries.ptr(),
                        group_count: 0,
                        end,
                        copy_offset: 0,
                    }
                })
                .collect::<Vec<GroupScanJob>>()
                .into_par_iter()
                .map(|mut job| {
                    let offset = job.thread_vars.offset;
                    let mut max_groups = job.group_thread_vars.count;
                    let group_indices;
                    unsafe {
                        group_indices = job.group_indices.add(job.group_thread_vars.offset);
                        *group_indices = offset as u32;
                    }
                    max_groups -= 1;
                    let group_count = 1 + scan_bc_group(
                        job.id,
                        &y_entries,
                        offset,
                        job.end,
                        unsafe { group_indices.offset(1) },
                        max_groups,
                    );
                    job.group_count = group_count;
                    job
                })
                .collect::<Vec<GroupScanJob>>();
            let mut copy_offset = 0;
            let mut index = 0;
            while index < thread_results.len() {
                if index > 0 {
                    thread_results[index].copy_offset = copy_offset as isize;
                } else {
                    thread_results[index].copy_offset = 0;
                }
                copy_offset += thread_results[index].group_count;
                index += 1;
            }
            (0..thread_count)
                .zip(thread_results)
                .collect::<Vec<(usize, GroupScanJob)>>()
                .into_par_iter()
                .for_each(|(index, mut job)| {
                    unsafe {
                        copy_nonoverlapping(
                            job.group_indices.add(job.group_thread_vars.offset),
                            job.final_group_entries.offset(job.copy_offset),
                            job.group_count,
                        );
                    }
                    if index == thread_count - 1 {
                        if max_groups > 0 {
                            unsafe {
                                *job.final_group_entries
                                    .offset(job.copy_offset + job.group_count as isize) =
                                    entry_count as u32;
                            }
                        } else {
                            job.group_count -= 1;
                        }
                        job.group_count -= 1;
                    }
                    group_count.fetch_add(job.group_count as u64, Ordering::Relaxed);
                });
            group_count.load(Ordering::Relaxed)
        }
    }

    pub fn decompress_proof(
        &mut self,
        plot_id: &Bytes32,
        k: u8,
        c_level: u8,
        compressed_proof: &[u64],
        f1_generator: Option<Arc<F1Generator>>,
    ) -> Result<Vec<u64>, Error> {
        let mut req = ProofRequest {
            compressed_proof: vec![0u64; k as usize],
            full_proof: vec![0u64; k as usize * 2],
            c_level,
            plot_id: *plot_id,
            f1_generator,
        };
        let compressed_proof_count = if c_level < 9 {
            PROOF_X_COUNT / 2
        } else {
            PROOF_X_COUNT / 4
        };
        req.compressed_proof[..compressed_proof_count]
            .copy_from_slice(&compressed_proof[..compressed_proof_count]);
        self.fetch_full_proof(k, &mut req)?;
        Ok(req.full_proof)
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::too_many_lines)]
    pub fn get_fetch_qualties_x_pair(
        &mut self,
        k: u8,
        req: CompressedQualitiesRequest,
    ) -> Result<(u64, u64), Error> {
        let entries_per_bucket =
            get_entries_per_bucket_for_compression_level(k, req.compression_level);
        assert!(entries_per_bucket <= 0xFFFF_FFFF);
        let mut x_groups = [0u32; 16];
        let mut num_xgroups;
        let lp_index = (req.line_points[0].hi as u128) << 64 | req.line_points[0].lo as u128;
        let p = line_point_to_square(lp_index);
        let (x1, x2) = line_point_to_square64(p.0);
        let (x3, x4) = line_point_to_square64(p.1);
        let mut proof_might_be_dropped = (x1 == 0 || x2 == 0) || (x3 == 0 || x4 == 0);
        if req.compression_level < 9 {
            num_xgroups = 2;
            x_groups[0] = x1 as u32;
            x_groups[1] = x2 as u32;
            x_groups[2] = x3 as u32;
            x_groups[3] = x4 as u32;
        } else {
            // Level 9 and above have 8 packed entries
            let entrybits = get_compression_info_for_level(req.compression_level).entry_size_bits;
            let mask = (1 << entrybits) - 1;
            num_xgroups = 4;
            x_groups[0] = (x1 as u32) & mask;
            x_groups[1] = (x1 as u32) >> entrybits;
            x_groups[2] = (x2 as u32) & mask;
            x_groups[3] = (x2 as u32) >> entrybits;
            x_groups[4] = (x3 as u32) & mask;
            x_groups[5] = (x3 as u32) >> entrybits;
            x_groups[6] = (x4 as u32) & mask;
            x_groups[7] = (x4 as u32) >> entrybits;
        }
        if req.compression_level >= 6 && req.compression_level < 9 {
            let lp_index = (req.line_points[1].hi as u128) << 64 | req.line_points[1].lo as u128;
            let p = line_point_to_square(lp_index);
            let (x1, x2) = line_point_to_square64(p.0);
            let (x3, x4) = line_point_to_square64(p.1);
            proof_might_be_dropped =
                proof_might_be_dropped || (x1 == 0 || x2 == 0) || (x3 == 0 || x4 == 0);
            if req.compression_level < 9 {
                num_xgroups = 4;
                x_groups[4] = x1 as u32;
                x_groups[5] = x2 as u32;
                x_groups[6] = x3 as u32;
                x_groups[7] = x4 as u32;
            } else {
                let entrybits =
                    get_compression_info_for_level(req.compression_level).entry_size_bits;
                let mask = (1 << entrybits) - 1;
                num_xgroups = 8;
                x_groups[8] = (x1 as u32) & mask;
                x_groups[9] = (x1 as u32) >> entrybits;
                x_groups[10] = (x2 as u32) & mask;
                x_groups[11] = (x2 as u32) >> entrybits;
                x_groups[12] = (x3 as u32) & mask;
                x_groups[13] = (x3 as u32) >> entrybits;
                x_groups[14] = (x4 as u32) & mask;
                x_groups[15] = (x4 as u32) >> entrybits;
            }
        }
        // Begin decompression
        {
            let out_y = Span::new(self.y_buffer_tmp.as_mut_ptr(), self.y_buffer_tmp.len());
            let out_meta = Span::new(
                self.meta_buffer_tmp.as_mut_ptr(),
                self.meta_buffer_tmp.len(),
            )
            .cast::<K32Meta2>();
            let out_pairs = Span::new(self.pairs.as_mut_ptr(), self.pairs.len());
            let mut tables = Span::new(self.tables.as_mut_ptr(), self.tables.len());
            let thread_count = self.config.thread_count;
            let mut table_context = TableContext {
                context: self,
                f1_generator: if let Some(f1) = req.f1_generator {
                    f1.clone()
                } else {
                    Arc::new(F1Generator::new(k, thread_count, req.plot_id.as_ref()))
                },
                entries_per_bucket: entries_per_bucket as isize,
                out_y,
                out_meta,
                out_pairs,
            };
            let table: &mut ProofTable = &mut tables[1isize];
            for (i, j) in (0..num_xgroups).zip((0..x_groups.len()).step_by(2)) {
                let x1 = u64::from(x_groups[j]);
                let x2 = u64::from(x_groups[j + 1]);
                let group_index = i / 2;
                if i % 2 == 0 {
                    table.begin_group(group_index);
                }
                if let Err(e) = Self::process_table1bucket(
                    k,
                    req.compression_level,
                    &mut table_context,
                    x1,
                    x2,
                    group_index,
                ) {
                    error!("Error Processing Table1 Bucket: {e:?}");
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Error Processing Table1 Bucket: {e:?}"),
                    ));
                }
            }
        }
        // #NOTE: Sanity check, but should never happen w/ our starting compression levels.
        if self.tables[1].length <= 2 {
            error!("Unexpected proof match on first table.");
            Err(Error::new(
                ErrorKind::InvalidData,
                "Unexpected proof match on first table.",
            ))
        } else {
            // Continue forward propagation to the next table
            let table2length = self.tables[1].length;
            self.proof_context = Some(ProofContext {
                left_length: table2length,
                right_length: 0,
                y_left: Span::new(self.y_buffer.as_mut_ptr(), self.y_buffer.len()),
                meta_left: Span::new(self.meta_buffer.as_mut_ptr(), self.meta_buffer.len()),
                y_right: Span::new(self.y_buffer_tmp.as_mut_ptr(), self.y_buffer_tmp.len()),
                meta_right: Span::new(
                    self.meta_buffer_tmp.as_mut_ptr(),
                    self.meta_buffer_tmp.len(),
                ),
                proof: [0; PROOF_X_COUNT],
            });
            debug!("Sorting Table 2 and Flipping Buffers");
            Self::sort_table2_and_flip_buffers(
                self.config.thread_count,
                &mut self.proof_context,
                Span::new(self.pairs.as_mut_ptr(), self.pairs.len()),
                &mut self.tables,
                Span::new(self.x_buffer_tmp.as_mut_ptr(), self.x_buffer_tmp.len()),
                Span::new(self.x_buffer.as_mut_ptr(), self.x_buffer.len()),
                num_xgroups / 2,
            )?;
            debug!("Forwarding Prop Tables:");
            num_xgroups /= 2;
            let mut match_table = PlotTable::Table3;
            let mut res: ForwardPropResult = self.forward_prop_table3(
                self.config.thread_count,
                num_xgroups,
                true,
                k,
                req.compression_level,
            )?;
            if res == ForwardPropResult::Continue {
                num_xgroups /= 2;
                match_table = PlotTable::Table4;
                res = self.forward_prop_table4(
                    self.config.thread_count,
                    num_xgroups,
                    true,
                    k,
                    req.compression_level,
                )?;
            }
            if res == ForwardPropResult::Continue {
                num_xgroups /= 2;
                match_table = PlotTable::Table5;
                res = self.forward_prop_table5(
                    self.config.thread_count,
                    num_xgroups,
                    true,
                    k,
                    req.compression_level,
                )?;
            }
            if res == ForwardPropResult::Continue {
                num_xgroups /= 2;
                match_table = PlotTable::Table6;
                res = self.forward_prop_table6(
                    self.config.thread_count,
                    num_xgroups,
                    true,
                    k,
                    req.compression_level,
                )?;
            }
            if res == ForwardPropResult::Success {
                let mut quality_xs: [u64; 8] = [0; 8];
                let mut pairs = [[Pair::zero(); 4]; 2];
                let mut r_table = match_table as usize;
                pairs[0][0..self.tables[r_table].length as usize].copy_from_slice(
                    &self.tables[r_table].pairs[0..self.tables[r_table].length as usize],
                );
                let mut pairs_in = Span::new(pairs[0].as_mut_ptr(), pairs[0].len());
                let mut pairs_out = Span::new(pairs[1].as_mut_ptr(), pairs[1].len());
                let mut tables = Span::new(self.tables.as_mut_ptr(), self.tables.len());
                while r_table > PlotTable::Table3 as usize {
                    let out_table_pairs = Span::new(
                        tables[r_table - 1].pairs.as_mut_ptr(),
                        tables[r_table - 1].pairs.len(),
                    );
                    for (i, pair) in tables[r_table].pairs[0..tables[r_table].length as usize]
                        .iter()
                        .enumerate()
                    {
                        pairs_out[i * 2] = out_table_pairs[pair.left as usize];
                        pairs_out[i * 2 + 1] = out_table_pairs[pair.right as usize];
                    }
                    swap(&mut pairs_out, &mut pairs_in);
                    r_table -= 1;
                }
                // From table 3, only take the pair that points to the first group
                let p0: Pair = pairs_in[0isize];
                let t3pair: Pair = if p0.left < self.tables[1].groups[0].count {
                    pairs_in[0isize]
                } else {
                    pairs_in[1isize]
                };
                // Grab the x's from the first group only
                let x_pair0 = self.tables[1].pairs[t3pair.left as usize];
                let x_pair1 = self.tables[1].pairs[t3pair.right as usize];
                quality_xs[0] = u64::from(x_pair0.left);
                quality_xs[1] = u64::from(x_pair0.right);
                quality_xs[2] = u64::from(x_pair1.left);
                quality_xs[3] = u64::from(x_pair1.right);
                // We need to now sort the X's on y, in order to chose the right path
                let mut quality_xs = Span::new(quality_xs.as_mut_ptr(), quality_xs.len());
                sort_quality_xs(&mut quality_xs, 4);
                // Follow the last path, based on the challenge in our x's
                let last5bits = req.challenge[31] & 0x1f;
                if (last5bits & 1) == 1 {
                    Ok((quality_xs[2isize], quality_xs[3isize]))
                } else {
                    Ok((quality_xs[0isize], quality_xs[1isize]))
                }
            } else if proof_might_be_dropped {
                Err(Error::new(ErrorKind::Other, "Proof Dropped"))
            } else {
                Err(Error::new(
                    ErrorKind::NotFound,
                    format!("Failed to look up quality, {res:?}"),
                ))
            }
        }
    }
}

fn sort_quality_xs(x: &mut Span<u64>, count: usize) {
    debug_assert!(count <= 16);
    let mut lp = [0u128; 8];
    let mut lp = Span::new(lp.as_mut_ptr(), lp.len());
    let lp_count = count / 2;
    for i in 0..lp_count {
        lp[i] = square_to_line_point128(x[i * 2], x[i * 2 + 1]);
    }
    lp[0..lp_count].sort_unstable();
    for i in 0..lp_count {
        let v = line_point_to_square(lp[i]);
        x[i * 2] = v.0;
        x[i * 2 + 1] = v.1;
    }
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_possible_wrap)]
#[allow(clippy::cast_sign_loss)]
fn scan_bc_group(
    _index: usize,
    y_buffer: &Span<u64>,
    scan_start: usize,
    scan_end: usize,
    group_indices: *mut u32,
    max_groups: usize,
) -> usize {
    let max_groups = max_groups as isize;
    if max_groups < 1 {
        return 0;
    }
    let mut group_count = 0;
    let mut prev_group = y_buffer[scan_start] / K_BC as u64;
    for i in scan_start + 1..scan_end {
        let group = y_buffer[i] / K_BC as u64;
        if group == prev_group {
            continue;
        }
        debug_assert!(group > prev_group);
        prev_group = group;
        unsafe {
            *group_indices.offset(group_count) = i as u32;
        }
        group_count += 1;
        if group_count == max_groups {
            break;
        }
    }
    group_count as usize
}
