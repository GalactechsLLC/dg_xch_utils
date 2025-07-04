use crate::constants::{K_CHECKPOINT1INTERVAL, K_ENTRIES_PER_PARK, K_EXTRA_BITS};
use crate::plots::decompressor::DecompressorPool;
use crate::plots::disk_plot::DiskPlot;
use crate::plots::fx_generator::{forward_prop_f1_to_f7, get_proof_f1_and_meta};
use crate::plots::plot_reader::PlotReader;
use crate::plots::PROOF_X_COUNT;
use crate::utils::bit_reader::BitReader;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::plots::{PlotFile, PlotHeader, PlotTable};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use futures_util::future::join_all;
use log::{debug, error, info, warn};
use std::cmp::{max, min};
use std::io::{Error, ErrorKind};
use std::mem::size_of;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncSeek};

pub struct ValidatePlotOptions {
    pub in_ram: bool,
    pub unpacked: bool,
    pub thread_count: usize,
    pub start_offset: f64,
    pub use_cuda: bool,
    pub f7: i64,
}
impl Default for ValidatePlotOptions {
    fn default() -> Self {
        Self {
            in_ram: false,
            unpacked: false,
            thread_count: 0,
            start_offset: 0.0,
            use_cuda: false,
            f7: -1,
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
pub async fn validate_plot(path: &Path, options: ValidatePlotOptions) -> Result<(), Error> {
    let count = thread::available_parallelism()?.get();
    let thread_count = max(min(options.thread_count, count), 1);
    let mut plot_files = vec![];
    for _ in 0..thread_count {
        plot_files.push(DiskPlot::new(path).await?);
    }
    info!("Validating Plot: {path:?}");
    info!("Mode: {}", if options.in_ram { "Ram" } else { "Disk" });
    info!("K Size: {}", plot_files[0].k());
    info!("Unpacked: {}", options.unpacked);
    let plot_c3park_count = plot_files[0].table_size(PlotTable::C1) as usize / size_of::<u32>() - 1;
    info!("Maximum C3 Parks: {plot_c3park_count}");
    if options.unpacked {
        todo!()
    } else {
        let fail_count = Arc::new(AtomicU64::new(0));
        let mut tasks = vec![];
        for (index, plot_file) in plot_files.into_iter().enumerate() {
            let fail_count = fail_count.clone();
            tasks.push(tokio::task::spawn(async move {
                validate_disk(
                    index,
                    thread_count,
                    plot_file,
                    fail_count,
                    options.start_offset,
                )
                .await
            }));
        }
        for results in join_all(&mut tasks).await {
            match results {
                Ok(res) => match res {
                    Ok(()) => {
                        info!("Validator Thread Finished");
                    }
                    Err(e) => {
                        error!("Error in Validator: {e:?}");
                    }
                },
                Err(e) => {
                    error!("Join Error for Plot Read Thread: {e:?}");
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::too_many_lines)]
async fn validate_disk<F: AsyncSeek + AsyncRead + Unpin>(
    index: usize,
    thread_count: usize,
    plot_file: DiskPlot<F>,
    fail_counter: Arc<AtomicU64>,
    start_offset: f64,
) -> Result<(), Error> {
    let plot_c3park_count = plot_file.table_size(PlotTable::C1) as usize / size_of::<u32>() - 1;
    let mut c3park_count = plot_c3park_count / thread_count;
    let mut start_c3park = index * c3park_count;
    let trailing_parks = plot_c3park_count - c3park_count * thread_count;
    if index < trailing_parks {
        c3park_count += 1;
    }
    start_c3park += min(trailing_parks, index);
    let c3park_end = start_c3park + c3park_count;
    if start_offset > 0.0f64 {
        start_c3park += min(c3park_count, (c3park_count as f64 * start_offset) as usize);
        c3park_count = c3park_end - start_c3park;
    }
    info!(
        "Index: {index} Park range: {start_c3park}..{c3park_end}  Park count: {c3park_count}"
    );
    let mut f7_entries;
    let mut fx: [u64; PROOF_X_COUNT] = [0; PROOF_X_COUNT];
    let mut meta: Vec<BitReader> = Vec::with_capacity(PROOF_X_COUNT);
    let mut cur_park7 = 0usize;
    let pool = Arc::new(DecompressorPool::new(1, thread_count as u8));
    let reader = PlotReader::new(plot_file, Some(pool.clone()), Some(pool)).await?;
    if index == 0 {
        reader.read_p7entries(0).await?;
    }
    let mut total_proofs = 0u128;
    let mut total_millis = 0u128;
    for c3_park_index in start_c3park..c3park_end {
        let c3_start = Instant::now();
        f7_entries = reader.read_c3park(c3_park_index as u64).await?;
        assert!(f7_entries.len() <= K_CHECKPOINT1INTERVAL as usize);
        let f7idx_base = c3_park_index * K_CHECKPOINT1INTERVAL as usize;
        let entry_count = f7_entries.len();
        let mut threshold = 0;
        info!("Validating c3 Park: {c3_park_index}");
        for (index, f7) in f7_entries.iter().enumerate() {
            if index / entry_count > threshold {
                info!(
                    "Progress: {}% ({}/{}), Avg Proof Lookup: {} millis",
                    index as f64 / entry_count as f64 * 100.0,
                    index,
                    entry_count,
                    if index > 0 {
                        total_proofs / total_millis
                    } else {
                        0
                    }
                );
                threshold += entry_count / 20;
            }
            let p_start = Instant::now();
            let f7idx = f7idx_base + index;
            let p7park_index = f7idx / K_ENTRIES_PER_PARK as usize;
            if p7park_index != cur_park7 {
                cur_park7 = p7park_index;
                reader.read_p7entries(p7park_index).await?;
            }
            let p7local_idx = f7idx - p7park_index * K_ENTRIES_PER_PARK as usize;
            let t6index = reader.p7_entries.lock().await[p7local_idx];
            match reader.fetch_proof(t6index).await {
                Ok(proof) => {
                    // Now we can validate the proof
                    match get_f7_from_proof(
                        u32::from(reader.plot_file().k()),
                        &reader.plot_id().bytes(),
                        &proof,
                        &mut fx,
                        &mut meta,
                    ) {
                        Ok(v_f7) => {
                            if v_f7 != *f7 {
                                error!("Failed to validate F7 v_f7({v_f7}) != f7({f7})");
                                fail_counter.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        Err(err) => {
                            error!("Error Validating Proof: {err:?}");
                            fail_counter.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                Err(err) => {
                    error!(
                        "Park [{c3_park_index}][{index}] proof fetch failed for f7[{f7idx}] local({index}) = {f7}: {index:?}, {err:?}"
                    );
                    fail_counter.fetch_add(1, Ordering::SeqCst);
                }
            }
            let p_elapsed = Instant::now().duration_since(p_start).as_millis();
            total_proofs += 1;
            total_millis += p_elapsed;
        }
        let c3_elapsed = Instant::now().duration_since(c3_start).as_millis();
        info!(
            "{}..{} ( {} ) C3 Park Validated in {} seconds | Proofs Failed: {}",
            c3_park_index,
            c3park_end - 1,
            (c3_park_index - start_c3park) as f64 / c3park_count as f64 * 100.0,
            c3_elapsed as f64 / 1000.0,
            fail_counter.load(Ordering::Relaxed)
        );
    }
    Ok(())
}

#[must_use]
pub fn uncompress_proof(proof: &[u8], k: usize) -> Vec<u64> {
    let mut index = 0;
    let proof_bits = BitReader::from_bytes_be(proof, proof.len() * 8);
    let mut new_proof = vec![];
    while index < 64usize {
        let as_int = proof_bits.slice_to_int(k * index, k * (index + 1));
        new_proof.push(as_int);
        index += 1;
    }
    new_proof
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_precision_loss)]
pub fn validate_proof(
    id: &[u8; 32],
    k: u8,
    proof: &[u8],
    challenge: &[u8],
) -> Result<Bytes32, Error> {
    let mut fx = vec![0; PROOF_X_COUNT];
    let mut meta: Vec<BitReader> = Vec::with_capacity(PROOF_X_COUNT);
    let f7 = get_f7_from_proof(
        u32::from(k),
        id,
        &uncompress_proof(proof, k as usize),
        &mut fx,
        &mut meta,
    )?;
    let challenge_bits = BitReader::from_bytes_be(challenge, challenge.len() * 8);
    let index = (challenge_bits
        .range(256 - 5, challenge_bits.get_size())
        .read_u64(5)?
        << 1) as u16;
    if challenge_bits.range(0, k as usize).first_u64() == f7 {
        get_quality_string(k, proof, index, challenge)
    } else {
        Ok(Bytes32::default())
    }
}

pub fn get_f7_from_proof(
    k: u32,
    plot_id: &[u8; 32],
    proof: &[u64],
    fx: &mut [u64],
    meta: &mut Vec<BitReader>,
) -> Result<u64, Error> {
    meta.clear();
    get_proof_f1_and_meta(k, plot_id, proof, fx, meta)?;
    forward_prop_f1_to_f7(None, fx, meta, k)?;
    Ok(fx[0] >> K_EXTRA_BITS)
}

pub fn get_f7_from_proof_and_reorder(
    k: u32,
    plot_id: &[u8; 32],
    proof: &[u64],
    fx: &mut [u64],
    meta: &mut Vec<BitReader>,
) -> Result<(u64, Vec<u64>), Error> {
    pub fn compress_proof(mut new_proof: Vec<u64>, k: usize) -> Vec<u64> {
        let mut bits = BitReader::default();
        for v in &new_proof {
            bits.append_value(*v, k);
        }
        new_proof.resize(k, 0u64);
        for (i, b) in bits
            .values()
            .into_iter()
            .enumerate()
            .take(PROOF_X_COUNT / 2)
        {
            new_proof[i] = b;
        }
        new_proof
    }
    meta.clear();
    let mut new_proof = proof.to_vec();
    get_proof_f1_and_meta(k, plot_id, proof, fx, meta)?;
    forward_prop_f1_to_f7(Some(&mut new_proof), fx, meta, k)?;
    Ok((fx[0] >> K_EXTRA_BITS, compress_proof(new_proof, k as usize)))
}

#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::cast_sign_loss)]
pub fn get_quality_string(
    k: u8,
    proof: &[u8],
    quality_index: u16,
    challenge: &[u8],
) -> Result<Bytes32, Error> {
    let mut proof_bits = BitReader::from_bytes_be(proof, proof.len() * 8);
    let mut table_index: u8 = 1;
    while table_index < 7 {
        let mut new_proof: BitReader = BitReader::default();
        let size: u16 = u16::from(k) * (1 << (table_index - 1)) as u16;
        let mut j = 0;
        while j < (1 << (7 - table_index)) {
            let mut left = proof_bits.range((j * size) as usize, ((j + 1) * size) as usize);
            let mut right = proof_bits.range(((j + 1) * size) as usize, ((j + 2) * size) as usize);
            if compare_proof_bits(&left, &right, k)? {
                left += &right;
                new_proof += &left;
            } else {
                right += &left;
                new_proof += &right;
            }
            j += 2;
        }
        proof_bits = new_proof;
        table_index += 1;
    }
    // Hashes two of the x values, based on the quality index
    let mut to_hash = challenge.to_vec();
    to_hash.extend(
        proof_bits
            .range(
                (u16::from(k) * quality_index) as usize,
                (u16::from(k) * (quality_index + 2)) as usize,
            )
            .to_bytes(),
    );
    Ok(Bytes32::new(hash_256(to_hash)))
}

pub async fn check_plot<T: AsRef<Path>>(
    path: T,
    challenges: usize,
) -> Result<(usize, usize), Error> {
    debug!("Testing plot {:?}", path.as_ref());
    let reader = PlotReader::new(DiskPlot::new(path.as_ref()).await?, None, None).await?;
    if reader.plot_file().compression_level() > 0 {
        warn!(
            "Plot Check skipped for plot at compression level {}",
            reader.plot_file().compression_level()
        );
        return Ok((challenges, 0));
    }
    let id = match reader.header() {
        //This is used to filter out GH plots
        PlotHeader::V1(h) => h.id,
        PlotHeader::V2(h) => h.id,
        PlotHeader::GHv2_5(_) => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Gigahorse Plots are Not Supported",
            ))
        }
    };
    let k = reader.header().k();
    let mut total_proofs = 0;
    let mut bad_proofs = 0;
    for i in 0..challenges {
        let challenge_hash = Bytes32::new(hash_256(i.to_be_bytes()));
        let start = Instant::now();
        let qualities = reader
            .fetch_qualities_for_challenge(challenge_hash.as_ref())
            .await?;
        let duration = Instant::now().duration_since(start).as_millis();
        for (index, _quality) in &qualities {
            if duration > 5000 {
                warn!("\tLooking up qualities took: {duration} ms. This should be below 5 seconds to minimize risk of losing rewards.");
            } else {
                debug!("\tLooking up qualities took: {duration} ms.");
            }
            let proof_start = Instant::now();
            let proof = reader.fetch_ordered_proof(*index).await?;
            let proof_duration = Instant::now().duration_since(proof_start).as_millis();
            if proof_duration > 15000 {
                warn!("\tFinding proof took: {proof_duration} ms. This should be below 15 seconds to minimize risk of losing rewards.");
            } else {
                debug!("\tFinding proof took: {proof_duration} ms");
            }
            total_proofs += 1;
            if validate_proof(
                &id.bytes(),
                k,
                &proof_to_bytes(&proof),
                challenge_hash.as_ref(),
            )
            .is_err()
            {
                bad_proofs += 1;
                error!("Error Proving Plot: {:?}", path.as_ref());
            }
        }
    }
    Ok((total_proofs, bad_proofs))
}

#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_wrap)]
fn compare_proof_bits(left: &BitReader, right: &BitReader, k: u8) -> Result<bool, Error> {
    let size = left.get_size() / k as usize;
    if left.get_size() != right.get_size() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Right and Left are not Equal",
        ));
    }
    let mut i = size as isize - 1;
    while i >= 0 {
        let ui = i as usize;
        let left_val = left.range(k as usize * ui, k as usize * (ui + 1));
        let right_val = right.range(k as usize * ui, k as usize * (ui + 1));
        if left_val < right_val {
            return Ok(true);
        }
        if left_val > right_val {
            return Ok(false);
        }
        i -= 1;
    }
    Ok(false)
}

#[must_use]
pub fn proof_to_bytes(src: &[u64]) -> Vec<u8> {
    src.iter()
        .map(|b| b.to_be_bytes())
        .collect::<Vec<[u8; size_of::<u64>()]>>()
        .concat()
}
