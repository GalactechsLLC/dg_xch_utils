use dg_xch_core::plots::PlotTable;
use num_traits::Zero;
use std::io::Error;
use std::mem::size_of;
use std::ops::Add;

pub mod compression;
pub mod d_tables;
pub mod decompressor;
pub mod disk_plot;
pub mod fx_generator;
pub mod plot_reader;

pub const PROOF_X_COUNT: usize = 64;
const BB_PLOT_VERSION: u32 = 1;
const MAX_MATCHES_MULTIPLIER: f64 = 0.005;
const MAX_MATCHES_MULTIPLIER_2T_DROP: f64 = 0.018; // For C9+
const MAX_BUCKETS: u32 = 32;
const MIN_TABLE_PAIRS: u64 = 1024;
const POST_PROOF_X_COUNT: usize = 64;
const POST_PROOF_CMP_X_COUNT: usize = POST_PROOF_X_COUNT / 2;

pub type K32Meta1 = u32;
pub type K32Meta2 = u64;
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct K32Meta3 {
    m0: u64,
    m1: u64,
}
trait FromMeta4 {
    fn from_meta4(s: K32Meta4) -> Vec<Self>
    where
        Self: Sized;
}
impl FromMeta4 for K32Meta4 {
    fn from_meta4(s: K32Meta4) -> Vec<Self> {
        vec![s]
    }
}
impl Add<Self> for K32Meta3 {
    type Output = Self;
    fn add(self, _: Self) -> Self::Output {
        todo!()
    }
}
impl Zero for K32Meta3 {
    fn zero() -> Self {
        K32Meta3 { m0: 0, m1: 0 }
    }
    fn is_zero(&self) -> bool {
        self.m0 == 0 && self.m1 == 0
    }
}

pub type K32Meta4 = K32Meta3;

struct _K32NoMeta {}

pub struct MetaIn {
    pub size_a: usize,
    pub size_b: usize,
    pub multiplier: usize,
}

pub struct MetaOut {
    pub size_a: usize,
    pub size_b: usize,
    pub multiplier: usize,
}

pub const fn get_meta_in(table: PlotTable) -> MetaIn {
    match table {
        PlotTable::Table1 => MetaIn {
            size_a: 0,
            size_b: 0,
            multiplier: 0,
        },
        PlotTable::Table2 => {
            let size_a = size_of::<u32>();
            let size_b = 0;
            MetaIn {
                size_a,
                size_b,
                multiplier: (size_a + size_b) / 4,
            }
        }
        PlotTable::Table3 => {
            let size_a = size_of::<u64>();
            let size_b = 0;
            MetaIn {
                size_a,
                size_b,
                multiplier: (size_a + size_b) / 4,
            }
        }
        PlotTable::Table4 => {
            let size_a = size_of::<u64>();
            let size_b = size_of::<u64>();
            MetaIn {
                size_a,
                size_b,
                multiplier: (size_a + size_b) / 4,
            }
        }
        PlotTable::Table5 => {
            let size_a = size_of::<u64>();
            let size_b = size_of::<u64>();
            MetaIn {
                size_a,
                size_b,
                multiplier: (size_a + size_b) / 4,
            }
        }
        PlotTable::Table6 => {
            let size_a = size_of::<u64>();
            let size_b = size_of::<u32>();
            MetaIn {
                size_a,
                size_b,
                multiplier: (size_a + size_b) / 4,
            }
        }
        PlotTable::Table7 => {
            let size_a = size_of::<u64>();
            let size_b = 0;
            MetaIn {
                size_a,
                size_b,
                multiplier: (size_a + size_b) / 4,
            }
        }
        _ => {
            panic!("Illegal Table MetaIn Lookup")
        }
    }
}

pub const fn get_meta_out(table: PlotTable) -> MetaOut {
    match table {
        PlotTable::Table1 => {
            let next_meta_in = get_meta_in(PlotTable::Table2);
            MetaOut {
                size_a: next_meta_in.size_a,
                size_b: next_meta_in.size_b,
                multiplier: next_meta_in.multiplier,
            }
        }
        PlotTable::Table2 => {
            let next_meta_in = get_meta_in(PlotTable::Table3);
            MetaOut {
                size_a: next_meta_in.size_a,
                size_b: next_meta_in.size_b,
                multiplier: next_meta_in.multiplier,
            }
        }
        PlotTable::Table3 => {
            let next_meta_in = get_meta_in(PlotTable::Table4);
            MetaOut {
                size_a: next_meta_in.size_a,
                size_b: next_meta_in.size_b,
                multiplier: next_meta_in.multiplier,
            }
        }
        PlotTable::Table4 => {
            let next_meta_in = get_meta_in(PlotTable::Table5);
            MetaOut {
                size_a: next_meta_in.size_a,
                size_b: next_meta_in.size_b,
                multiplier: next_meta_in.multiplier,
            }
        }
        PlotTable::Table5 => {
            let next_meta_in = get_meta_in(PlotTable::Table6);
            MetaOut {
                size_a: next_meta_in.size_a,
                size_b: next_meta_in.size_b,
                multiplier: next_meta_in.multiplier,
            }
        }
        PlotTable::Table6 => {
            let next_meta_in = get_meta_in(PlotTable::Table7);
            MetaOut {
                size_a: next_meta_in.size_a,
                size_b: next_meta_in.size_b,
                multiplier: next_meta_in.multiplier,
            }
        }
        PlotTable::Table7 => MetaOut {
            size_a: 0,
            size_b: 0,
            multiplier: 0,
        },
        _ => {
            panic!("Illegal Table MetaOut Lookup")
        }
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq)]
struct Group {
    count: u32,
    offset: u32,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct LPIndex {
    lp: u64,
    index: u32,
}
impl Add<Self> for LPIndex {
    type Output = Self;
    fn add(self, _rhs: Self) -> Self::Output {
        todo!()
    }
}
impl Zero for LPIndex {
    fn zero() -> Self {
        LPIndex { lp: 0, index: 0 }
    }
    fn is_zero(&self) -> bool {
        self.lp == 0 && self.index == 0
    }
}
#[derive(Debug)]
pub enum ForwardPropResult {
    Failed(Error),
    Success,
    Continue,
}
impl ForwardPropResult {
    pub fn as_byte(&self) -> u8 {
        match self {
            ForwardPropResult::Failed(_) => 0,
            ForwardPropResult::Success => 1,
            ForwardPropResult::Continue => 2,
        }
    }
}
impl PartialEq<Self> for ForwardPropResult {
    fn eq(&self, other: &Self) -> bool {
        self.as_byte() == other.as_byte()
    }
}

impl Eq for ForwardPropResult {}

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct ProofTable {
    pairs: Vec<Pair>,
    capacity: u32,
    length: u32,
    groups: [Group; 16],
}
impl<'a> ProofTable {
    pub fn get_group_pairs(&'a self, group_index: usize) -> &'a [Pair] {
        let group = &self.groups[group_index];
        &self.pairs.as_slice()[group.offset as usize..group.count as usize]
    }

    pub fn get_used_table_pairs(&'a self) -> &'a [Pair] {
        &self.pairs.as_slice()[0..self.length as usize]
    }

    pub fn get_free_table_pairs(&'a self) -> &'a [Pair] {
        &self.pairs.as_slice()[self.length as usize..(self.capacity - self.length) as usize]
    }

    pub fn push_group_pair(&mut self, group_idx: usize) -> &Pair {
        self.groups[group_idx].count += 1;
        self.length += 1;
        &self.pairs[self.length as usize]
    }

    pub fn begin_group(&mut self, group_idx: usize) {
        self.groups[group_idx].count = 0;
        if group_idx > 0 {
            self.groups[group_idx].offset =
                self.groups[group_idx - 1].offset + self.groups[group_idx - 1].count;
        } else {
            self.length = 0;
        }
    }

    pub fn add_group_pairs(&mut self, group_idx: usize, pair_count: u32) {
        self.groups[group_idx].count += pair_count;
        self.length += pair_count;
    }
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Pair {
    left: u32,
    right: u32,
}
impl Pair {
    pub fn add_offset(mut self, offset: u32) -> Self {
        self.left += offset;
        self.right += offset;
        self
    }

    pub fn sub_offset(mut self, offset: u32) -> Self {
        self.left -= offset;
        self.right -= offset;
        self
    }
}
impl Add<Self> for Pair {
    type Output = Self;
    fn add(self, _rhs: Self) -> Self::Output {
        todo!()
    }
}
impl Zero for Pair {
    fn zero() -> Self {
        Pair { left: 0, right: 0 }
    }
    fn is_zero(&self) -> bool {
        self.left == 0 && self.right == 0
    }
}
