use crate::encoding;
use crate::encoding::create_normalized_count;
use crate::finite_state_entropy::compress::CTable;
use crate::finite_state_entropy::decompress::DTable;
use crate::finite_state_entropy::fse_ctable_size;
use crate::plots::{MAX_BUCKETS, MAX_MATCHES_MULTIPLIER, MAX_MATCHES_MULTIPLIER_2T_DROP};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;
use std::io::{Error, ErrorKind};
use std::sync::Arc;

#[derive(Copy, Clone)]
pub struct CompressionInfo {
    pub entry_size_bits: u32,
    pub stub_size_bits: u32,
    pub table_park_size: usize,
    pub ans_rvalue: f64,
}
#[derive(Default)]
pub struct CompressionTableCache {
    c_tables: FxHashMap<u8, Arc<CTable>>,
    d_tables: FxHashMap<u8, Arc<DTable>>,
}
impl CompressionTableCache {
    pub fn new() -> Self {
        Default::default()
    }
    pub fn ct_exists(&self, c_level: u8) -> bool {
        self.c_tables.contains_key(&c_level)
    }
    pub fn dt_exists(&self, c_level: u8) -> bool {
        self.d_tables.contains_key(&c_level)
    }
    pub fn ct_assign(&mut self, c_level: u8, ct: Arc<CTable>) {
        self.c_tables.insert(c_level, ct);
    }
    pub fn dt_assign(&mut self, c_level: u8, dt: DTable) {
        self.d_tables.insert(c_level, Arc::new(dt));
    }
    pub fn ct_get(&self, c_level: u8) -> Option<Arc<CTable>> {
        self.c_tables.get(&c_level).cloned()
    }
    pub fn dt_get(&self, c_level: u8) -> Option<Arc<DTable>> {
        self.d_tables.get(&c_level).cloned()
    }
}
static C_LEVEL_CACHE: Lazy<Arc<Mutex<CompressionTableCache>>> =
    Lazy::new(|| Arc::new(Mutex::new(CompressionTableCache::new())));

pub static COMPRESSION_LEVEL_INFO: [CompressionInfo; 9] = [
    CompressionInfo {
        //1
        entry_size_bits: 16,
        stub_size_bits: 29,
        table_park_size: 8336,
        ans_rvalue: 2.51,
    },
    CompressionInfo {
        //2
        entry_size_bits: 15,
        stub_size_bits: 25,
        table_park_size: 7360,
        ans_rvalue: 3.44,
    },
    CompressionInfo {
        //3
        entry_size_bits: 14,
        stub_size_bits: 21,
        table_park_size: 6352,
        ans_rvalue: 4.36,
    },
    CompressionInfo {
        //4
        entry_size_bits: 13,
        stub_size_bits: 16,
        table_park_size: 5325, //5312;
        ans_rvalue: 9.30,
    },
    CompressionInfo {
        //5
        entry_size_bits: 12,
        stub_size_bits: 12,
        table_park_size: 4300, //4290;
        ans_rvalue: 9.30,
    },
    CompressionInfo {
        //6
        entry_size_bits: 11,
        stub_size_bits: 8,
        table_park_size: 3273, //3263;
        ans_rvalue: 9.10,
    },
    CompressionInfo {
        //7
        entry_size_bits: 10,
        stub_size_bits: 4,
        table_park_size: 2250, //2240;
        ans_rvalue: 8.60,
    },
    //https://github.com/Chia-Network/bladebit/blob/cuda-compression/src/plotting/Compression.h
    // #TODO: These are dummy values in bladebit, Update with real values later from upstream
    CompressionInfo {
        //8
        entry_size_bits: 9,
        stub_size_bits: 4,
        table_park_size: 6350, //2240;
        ans_rvalue: 8.60,
    },
    CompressionInfo {
        //9
        entry_size_bits: 8,
        stub_size_bits: 30,
        table_park_size: 8808,
        ans_rvalue: 4.54,
    },
];

pub fn get_compression_info_for_level(compression_level: u8) -> &'static CompressionInfo {
    assert!(compression_level > 0);
    assert!(compression_level < 10);
    match compression_level {
        1 => &COMPRESSION_LEVEL_INFO[0],
        2 => &COMPRESSION_LEVEL_INFO[1],
        3 => &COMPRESSION_LEVEL_INFO[2],
        4 => &COMPRESSION_LEVEL_INFO[3],
        5 => &COMPRESSION_LEVEL_INFO[4],
        6 => &COMPRESSION_LEVEL_INFO[5],
        7 => &COMPRESSION_LEVEL_INFO[6],
        8 => &COMPRESSION_LEVEL_INFO[7],
        _ => &COMPRESSION_LEVEL_INFO[8],
    }
}

pub fn create_compression_dtable(c_level: u8) -> Result<Arc<DTable>, Error> {
    match c_level {
        1..=9 => create_compression_dtable_for_clevel(c_level),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid Compression Level: {c_level}"),
        )),
    }
}

pub fn create_compression_ctable(c_level: u8, out_size: &mut usize) -> Result<Arc<CTable>, Error> {
    match c_level {
        1..=9 => create_compression_ctable_for_clevel(c_level, out_size),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("Invalid Compression Level: {c_level}"),
        )),
    }
}
pub fn create_compression_ctable_for_clevel(
    c_level: u8,
    out_size: &mut usize,
) -> Result<Arc<CTable>, Error> {
    let mut cache = C_LEVEL_CACHE.as_ref().lock();
    match cache.c_tables.entry(c_level) {
        Entry::Occupied(e) => Ok(e.get().clone()),
        Entry::Vacant(e) => {
            let r_value = get_compression_info_for_level(c_level).ans_rvalue;
            let ct = Arc::new(gen_compression_table(r_value, out_size)?);
            e.insert(ct.clone());
            Ok(ct)
        }
    }
}
pub fn create_compression_dtable_for_clevel(c_level: u8) -> Result<Arc<DTable>, Error> {
    encoding::get_d_table(get_compression_info_for_level(c_level).ans_rvalue)
}

fn gen_compression_table(r_value: f64, out_size: &mut usize) -> Result<CTable, Error> {
    let normalized_count = create_normalized_count(r_value)?;
    let max_symbol_value = normalized_count.len() - 1;
    let table_log = 14;
    // let ct  = build_ctable( normalized_count, max_symbol_value, table_log );
    *out_size = fse_ctable_size(table_log, max_symbol_value as u32) as usize;
    // ct
    todo!()
}

pub fn get_entries_per_bucket_for_compression_level(k: u8, c_level: u8) -> u64 {
    1u64 << (k - (17 - c_level))
}

pub fn get_max_table_pairs_for_compression_level(k: u8, c_level: u8) -> usize {
    let factor = if c_level >= 9 {
        MAX_MATCHES_MULTIPLIER_2T_DROP
    } else {
        MAX_MATCHES_MULTIPLIER
    };
    (get_entries_per_bucket_for_compression_level(k, c_level) as f64 * factor) as usize
        * MAX_BUCKETS as usize
}
