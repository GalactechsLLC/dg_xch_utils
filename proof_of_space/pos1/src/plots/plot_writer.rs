use crate::constants::{K_BATCH_SIZES, K_BC, K_EXTRA_BITS, L_TARGETS};
use crate::f_calc::F1Calculator;
use crate::plots::fx_generator::fx_gen;
use crate::utils::bit_reader::BitReader;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::plots::PlotTable;
use dg_xch_core::traits::SizedBytes;
use std::cmp::min;
use std::io::{Error, ErrorKind};

/// One entry of a forward propagation table. `left` and `right` index the previous table; in
/// table 1 there is no previous table and `left` carries the x value itself.
#[derive(Clone)]
pub struct FpEntry {
    pub y: u64,
    pub meta: BitReader,
    pub left: u32,
    pub right: u32,
}

/// Tables 1 through 7 held in memory, each sorted by `y`. Only viable at a small `k`; a
/// production-size plot needs the disk backed bucket sort instead.
pub struct ForwardTables {
    pub k: u8,
    pub plot_id: Bytes32,
    pub tables: Vec<Vec<FpEntry>>,
}

impl ForwardTables {
    /// Table `table` (1 based), sorted by `y`.
    #[must_use]
    pub fn table(&self, table: usize) -> &[FpEntry] {
        &self.tables[table - 1]
    }

    /// The f7 of a table 7 entry.
    #[must_use]
    pub fn f7(&self, index: usize) -> u64 {
        self.tables[6][index].y >> K_EXTRA_BITS
    }
}

/// Compute table 1: `f1(x)` for every x, sorted by y.
fn compute_table1(k: u8, plot_id: &Bytes32) -> Vec<FpEntry> {
    let f1 = F1Calculator::new(k, &plot_id.bytes());
    let max_x = 1u64 << k;
    let batch = 1u64 << K_BATCH_SIZES;
    let mut buffer = vec![0u64; batch as usize];
    let mut entries = Vec::with_capacity(max_x as usize);
    let mut x = 0;
    while x < max_x {
        let n = min(batch, max_x - x);
        f1.calculate_buckets(x, n, &mut buffer[..n as usize]);
        for (i, y) in buffer[..n as usize].iter().enumerate() {
            let value = x + i as u64;
            entries.push(FpEntry {
                y: *y,
                meta: BitReader::new(value, k as usize),
                left: value as u32,
                right: 0,
            });
        }
        x += n;
    }
    entries.sort_unstable_by_key(|e| e.y);
    entries
}

/// Every matching pair in a table, as index pairs into it.
///
/// Two entries match when their y values fall in consecutive `K_BC` groups and the right one lands
/// on one of the 64 targets the left one projects to. Rather than compare every pair, the right
/// group is indexed by its offset within the group so each left entry only visits its own targets.
fn find_pairs(entries: &[FpEntry]) -> Vec<(u32, u32)> {
    let mut pairs = Vec::new();
    let mut head = vec![u32::MAX; K_BC];
    let mut next = vec![u32::MAX; entries.len()];
    let mut touched: Vec<usize> = Vec::new();

    let group_of = |e: &FpEntry| e.y / K_BC as u64;
    let mut i = 0;
    while i < entries.len() {
        let group_l = group_of(&entries[i]);
        let start_l = i;
        while i < entries.len() && group_of(&entries[i]) == group_l {
            i += 1;
        }
        let end_l = i;
        if i >= entries.len() {
            break;
        }
        let group_r = group_of(&entries[i]);
        if group_r != group_l + 1 {
            continue;
        }
        let start_r = i;
        let mut j = i;
        while j < entries.len() && group_of(&entries[j]) == group_r {
            j += 1;
        }
        let end_r = j;

        for r in start_r..end_r {
            let local = (entries[r].y - group_r * K_BC as u64) as usize;
            if head[local] == u32::MAX {
                touched.push(local);
            }
            next[r] = head[local];
            head[local] = r as u32;
        }
        let parity = (group_l & 1) as usize;
        for (offset, entry) in entries[start_l..end_l].iter().enumerate() {
            let l = start_l + offset;
            let local_l = (entry.y - group_l * K_BC as u64) as usize;
            for target in &L_TARGETS[parity][local_l] {
                let mut r = head[*target as usize];
                while r != u32::MAX {
                    pairs.push((l as u32, r));
                    r = next[r as usize];
                }
            }
        }
        for local in touched.drain(..) {
            head[local] = u32::MAX;
        }
    }
    pairs
}

/// Build tables 1 through 7 for `(k, plot_id)`.
pub fn forward_propagate(k: u8, plot_id: Bytes32) -> Result<ForwardTables, Error> {
    let mut tables = Vec::with_capacity(7);
    tables.push(compute_table1(k, &plot_id));

    for table in [
        PlotTable::Table2,
        PlotTable::Table3,
        PlotTable::Table4,
        PlotTable::Table5,
        PlotTable::Table6,
        PlotTable::Table7,
    ] {
        let previous = tables
            .last()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "no previous table"))?;
        let pairs = find_pairs(previous);
        if pairs.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("no matches into {table:?}; k is too small for this plot id"),
            ));
        }
        let mut entries = Vec::with_capacity(pairs.len());
        for (left, right) in pairs {
            let mut y = 0u64;
            let mut meta = BitReader::default();
            fx_gen(
                table,
                u32::from(k),
                previous[left as usize].y,
                &previous[left as usize].meta,
                &previous[right as usize].meta,
                &mut y,
                &mut meta,
            )?;
            entries.push(FpEntry {
                y,
                meta,
                left,
                right,
            });
        }
        entries.sort_unstable_by_key(|e| e.y);
        tables.push(entries);
    }
    Ok(ForwardTables { k, plot_id, tables })
}

/// The 64 x values behind a table 7 entry, in left to right tree order.
#[must_use]
pub fn proof_xs(tables: &ForwardTables, table7_index: usize) -> Vec<u64> {
    fn collect(tables: &[Vec<FpEntry>], table_index: usize, entry: u32, out: &mut Vec<u64>) {
        let e = &tables[table_index][entry as usize];
        if table_index == 0 {
            out.push(u64::from(e.left));
            return;
        }
        collect(tables, table_index - 1, e.left, out);
        collect(tables, table_index - 1, e.right, out);
    }
    let mut out = Vec::with_capacity(64);
    collect(&tables.tables, 6, table7_index as u32, &mut out);
    out
}

/// Pack 64 x values the way a proof of space carries them: `k` bits each, big endian.
#[must_use]
pub fn proof_bytes(k: u8, xs: &[u64]) -> Vec<u8> {
    let mut bits = BitReader::default();
    for x in xs {
        bits.append_value(*x, k as usize);
    }
    bits.to_bytes()
}
