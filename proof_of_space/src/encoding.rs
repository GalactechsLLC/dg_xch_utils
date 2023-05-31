use crate::finite_state_entropy::compress::CTable;
use crate::finite_state_entropy::decompress::{build_dtable, decompress_using_dtable, DTable};
use lazy_static::lazy_static;
use num_traits::Pow;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct TMemoCache {
    ct_memo: HashMap<[u8; 8], Vec<CTable>>,
    dt_memo: HashMap<[u8; 8], Arc<DTable>>,
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

pub const fn get_x_enc(x: &u64) -> u128 {
    if *x % 2 == 0 {
        (*x / 2) as u128 * x.wrapping_sub(1) as u128
    } else {
        *x as u128 * (x.wrapping_sub(1) / 2) as u128
    }
}

pub fn square_to_line_point(mut x: u64, mut y: u64) -> u128 {
    // Always makes y < x, which maps the random x, y  points from a square into a
    // triangle. This means less data is needed to represent y, since we know it's less
    // than x.
    if y > x {
        std::mem::swap(&mut x, &mut y);
    }
    get_x_enc(&x) + y as u128
}

// Does the opposite as the above function, deterministically mapping a one dimensional
// line point into a 2d pair. However, we do not recover the original ordering here.
pub const fn line_point_to_square(index: u128) -> (u64, u64) {
    // Performs a square root, without the use of doubles, to use the precision of the u128.
    let mut x = 0;
    let mut i = 63;
    while i >= 0 {
        let new_x = x + (1u64 << i);
        if get_x_enc(&new_x) <= index {
            x = new_x;
        }
        i -= 1;
    }
    (x, (index - get_x_enc(&x)) as u64)
}

pub fn create_normalized_count(r: f64) -> Result<Vec<i16>, Error> {
    let mut dpdf: Vec<f64> = vec![];
    let mut n = 0;
    let min_prb_threshold = 1e-50;
    let total_quanta = 1 << 14;
    let mut p = 1.0f64 - ((std::f64::consts::E - 1.0f64) / std::f64::consts::E).pow(1.0f64 / r);

    while p > min_prb_threshold && n < 255 {
        dpdf.push(p);
        n += 1;
        p = (std::f64::consts::E.pow(1.0 / r) - 1.0) * (std::f64::consts::E - 1.0).pow(1.0 / r);
        p /= std::f64::consts::E.pow((n + 1) as f64 / r);
    }
    let ans = Mutex::new(vec![1i16; n]);
    let sort_fn = |i: &usize, j: &usize| {
        let mutex = ans
            .lock()
            .expect("Failed to acquire lock for sort function");
        let left = dpdf[*i] * (((mutex[*i] + 1) as f64).log2() - (mutex[*i] as f64).log2());
        let right = dpdf[*j] * (((mutex[*j] + 1) as f64).log2() - (mutex[*j] as f64).log2());
        left.partial_cmp(&right)
            .expect("Failed to compare invalid values, NAN in float values")
    };
    let mut pq = vec![];
    pq.extend(0..n);
    pq.sort_by(sort_fn);
    for _ in 0..total_quanta - n {
        let i = pq
            .pop()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Failed to convert atom to int"))?;
        {
            ans.lock().map_err(|e| {
                Error::new(ErrorKind::Other, format!("Failed to lock mutex: {:?}", e))
            })?[i] += 1;
        }
        pq.push(i);
        pq.sort_by(sort_fn);
    }
    {
        let mut mutex = ans
            .lock()
            .map_err(|e| Error::new(ErrorKind::Other, format!("Failed to lock mutex: {:?}", e)))?;
        for i in 0..n {
            if mutex[i] == 1 {
                mutex[i] = -1i16;
            }
        }
    }
    ans.into_inner().map_err(|e| {
        Error::new(
            ErrorKind::Other,
            format!("Failed to remove values from mutex: {}", e),
        )
    })
}

pub fn ans_decode_deltas(
    input: &[u8],
    input_size: usize,
    num_deltas: usize,
    r: f64,
) -> Result<Vec<u8>, Error> {
    let dt;
    {
        let mut cache = MEMO_CACHE.as_ref().lock().map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Failed to lock memo cache: {}", e),
            )
        })?;
        if !cache.dt_exists(r) {
            let normalized_count = create_normalized_count(r)?;
            let max_symbol_value = normalized_count.len() - 1;
            let table_log = 14;
            cache.dt_assign(
                r,
                build_dtable(&normalized_count, max_symbol_value as u32, table_log)?,
            );
        }
        dt = cache.dt_get(r).expect("Cache miss on expected value");
    }
    let mut dst = vec![0u8; num_deltas];
    match decompress_using_dtable(&mut dst, num_deltas, input, input_size, dt) {
        Ok(_) => {
            if dst.iter().any(|d| *d == 0xff) {
                Err(Error::new(ErrorKind::InvalidInput, "Bad delta detected"))
            } else {
                Ok(dst)
            }
        }
        Err(e) => Err(e),
    }
}
