use crate::finite_state_entropy::compress::CTable;
use crate::finite_state_entropy::decompress::{build_dtable, decompress_using_dtable, DTable};
use crate::utils::span::Span;
use lazy_static::lazy_static;
use num_traits::real::Real;
use num_traits::Pow;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::io::{Error, ErrorKind};
use std::sync::Arc;

#[derive(Default)]
pub struct TMemoCache {
    ct_memo: FxHashMap<[u8; 8], Vec<CTable>>,
    dt_memo: FxHashMap<[u8; 8], Arc<DTable>>,
}
impl TMemoCache {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn ct_exists(&self, r: f64) -> bool {
        self.ct_memo.contains_key(&r.to_be_bytes())
    }

    pub fn dt_exists(&self, r: f64) -> bool {
        self.dt_memo.contains_key(&r.to_be_bytes())
    }

    pub fn ct_assign(&mut self, r: f64, ct: Vec<CTable>) {
        self.ct_memo.insert(r.to_be_bytes(), ct);
    }

    pub fn dt_assign(&mut self, r: f64, dt: DTable) {
        self.dt_memo.insert(r.to_be_bytes(), Arc::new(dt));
    }

    pub fn ct_get(&self, r: f64) -> Option<&Vec<CTable>> {
        return self.ct_memo.get(&r.to_be_bytes());
    }

    pub fn dt_get(&self, r: f64) -> Option<Arc<DTable>> {
        return self.dt_memo.get(&r.to_be_bytes()).cloned();
    }
}
lazy_static! {
    static ref MEMO_CACHE: Arc<Mutex<TMemoCache>> = Arc::new(Mutex::new(TMemoCache::new()));
}

pub fn get_x_enc(x: &u64) -> u64 {
    if x % 2 == 0 {
        (x >> 1).wrapping_mul(x.wrapping_sub(1))
    } else {
        x.wrapping_mul(x.wrapping_sub(1) >> 1)
    }
}

pub fn get_x_enc128(x: &u64) -> u128 {
    if x & 1 == 0 {
        (x >> 1) as u128 * x.wrapping_sub(1) as u128
    } else {
        *x as u128 * (x.wrapping_sub(1) >> 1) as u128
    }
}

pub fn square_to_line_point(x: u64, y: u64) -> u64 {
    // Always makes y < x, which maps the random x, y  points from a square into a
    // triangle. This means less data is needed to represent y, since we know it's less
    // than x.
    if y > x {
        get_x_enc(&y) + x
    } else {
        get_x_enc(&x) + y
    }
}

pub fn square_to_line_point128(x: u64, y: u64) -> u128 {
    // Always makes y < x, which maps the random x, y  points from a square into a
    // triangle. This means less data is needed to represent y, since we know it's less
    // than x.
    if y > x {
        get_x_enc128(&y) + x as u128
    } else {
        get_x_enc128(&x) + y as u128
    }
}

// Does the opposite as the above function, deterministically mapping a one dimensional
// line point into a 2d pair. However, we do not recover the original ordering here.
pub fn line_point_to_square(index: u128) -> (u64, u64) {
    // Performs a square root, without the use of doubles, to use the precision of the u128.
    let mut x = 0;
    for i in (0..=63).rev() {
        let new_x = x + (1u64 << i);
        if get_x_enc128(&new_x) <= index {
            x = new_x;
        }
    }
    (x, (index - get_x_enc128(&x)) as u64)
}

pub fn line_point_to_square64(index: u64) -> (u64, u64) {
    let mut x = 0;
    let mut i = 63;
    while i >= 0 {
        let new_x = x + (1u64 << i);
        if get_x_enc(&new_x) <= index {
            x = new_x;
        }
        i -= 1;
    }
    (x, (index - get_x_enc(&x)))
}

pub const MIN_PRB_THRESHOLD: f64 = 1e-50;
pub const TOTAL_QUANTITY: usize = 1 << 14;

pub fn create_normalized_count(r: f64) -> Result<Vec<i16>, Error> {
    let mut dpdf: Vec<f64> = Vec::with_capacity(256);
    let mut n = 0;
    let mut p = 1.0f64 - ((std::f64::consts::E - 1.0f64) / std::f64::consts::E).pow(1.0f64 / r);
    let alt_p = (std::f64::consts::E.pow(1.0 / r) - 1.0) * (std::f64::consts::E - 1.0).pow(1.0 / r);
    while p > MIN_PRB_THRESHOLD && n < 255 {
        dpdf.push(p);
        n += 1;
        p = alt_p / (std::f64::consts::E.pow((n + 1) as f64 / r));
    }
    let mut ans = vec![(1i16, 2.0.log2() - 1.0.log2()); n];
    let ans_span = Span::new(ans.as_mut_ptr(), n);
    let sort_fn = |i: &usize, j: &usize| {
        let left = dpdf[*i] * ans_span[*i].1;
        let right = dpdf[*j] * ans_span[*j].1;
        left.partial_cmp(&right)
            .expect("Failed to compare invalid values, NAN in float values")
    };
    let mut pq = vec![];
    pq.extend(0..n);
    pq.sort_unstable_by(sort_fn);
    for _ in 0..TOTAL_QUANTITY - n {
        {
            let v = &mut ans[*pq
                .last()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "pq Array was Empty"))?];
            v.0 += 1;
            v.1 = (v.0 as f64 + 1.0).log2() - (v.0 as f64).log2();
        }
        pq.sort_unstable_by(sort_fn);
    }
    Ok(ans
        .into_iter()
        .map(|(i, _)| {
            -((i == 1) as i16) // Set to -1 if it is 1
            + i * ((i != 1) as i16) //Dont change otherwise
        })
        .collect())
}

pub fn get_d_table(r: f64) -> Result<Arc<DTable>, Error> {
    let mut cache = MEMO_CACHE.as_ref().lock();
    if !cache.dt_exists(r) {
        let normalized_count = create_normalized_count(r)?;
        let max_symbol_value = normalized_count.len() - 1;
        let table_log = 14;
        cache.dt_assign(
            r,
            build_dtable(&normalized_count, max_symbol_value as u32, table_log)?,
        );
    }
    Ok(cache.dt_get(r).expect("Cache miss on expected value"))
}

pub fn ans_decode_deltas(
    input: &[u8],
    input_size: usize,
    num_deltas: usize,
    r: f64,
) -> Result<(usize, Vec<u8>), Error> {
    let dt = get_d_table(r)?;
    let mut dst = vec![0u8; num_deltas];
    match decompress_using_dtable(&mut dst, num_deltas, input, input_size, dt) {
        Ok(c) => {
            if dst.iter().any(|d| *d == 0xff) {
                Err(Error::new(ErrorKind::InvalidInput, "Bad delta detected"))
            } else {
                Ok((c, dst))
            }
        }
        Err(e) => Err(e),
    }
}
