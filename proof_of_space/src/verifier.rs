use crate::bitvec::BitVec;
use crate::constants::*;
use crate::f_calc::F1Calculator;
use crate::f_calc::FXCalculator;
use crate::prover::DiskProver;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_serialize::hash_256;
use log::{debug, error, warn};
use log::{info, trace};
use sha2::{Digest, Sha256};
use std::io::Error;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Instant;

pub fn get_quality_string(
    k: u8,
    proof: &Vec<u8>,
    quality_index: u16,
    challenge: &[u8],
) -> Result<Vec<u8>, Error> {
    let mut proof_bits =
        BitVec::from_be_bytes(proof.clone(), proof.len() as u32, (proof.len() * 8) as u32);
    let mut table_index: u8 = 1;
    while table_index < 7 {
        let mut new_proof: BitVec = BitVec::new(0, 0);
        let size: u16 = k as u16 * (1 << (table_index - 1)) as u16;
        let mut j = 0;
        while j < (1 << (7 - table_index)) {
            let mut left = proof_bits.range((j * size) as u32, ((j + 1) * size) as u32);
            let mut right = proof_bits.range(((j + 1) * size) as u32, ((j + 2) * size) as u32);
            if compare_proof_bits(&left, &right, k)? {
                left += right;
                new_proof += left;
            } else {
                right += left;
                new_proof += right;
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
                (k as u16 * quality_index) as u32,
                (k as u16 * (quality_index + 2)) as u32,
            )
            .to_bytes(),
    );
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    Ok(hasher.finalize().to_vec())
}

pub fn validate_proof(
    id: &[u8; 32],
    k: u8,
    challenge: &[u8],
    proof_bytes: &[u8],
) -> Result<BitVec, Error> {
    let proof_bits = BitVec::from_be_bytes(
        proof_bytes,
        proof_bytes.len() as u32,
        (proof_bytes.len() * 8) as u32,
    );
    if k as usize * 64 != proof_bits.get_size() as usize {
        return Ok(BitVec::new(0, 0));
    }
    let mut proof: Vec<BitVec> = Vec::new();
    let mut ys: Vec<BitVec> = Vec::new();
    let mut metadata: Vec<BitVec> = Vec::new();
    let f1 = F1Calculator::new(k, id);

    let mut index: u8 = 0;
    while index < 64 {
        let as_int =
            proof_bits.slice_to_int(k as u32 * index as u32, k as u32 * (index as u32 + 1));
        proof.push(BitVec::new(as_int as u128, k as u32));
        index += 1;
    }

    // Calculates f1 for each of the given xs. Note that the proof is in proof order.
    index = 0;
    while index < 64 {
        let proof_slice = &proof[index as usize];
        let results = f1.calculate_bucket(proof_slice)?;
        ys.push(results.0);
        metadata.push(results.1);
        index += 1;
    }

    // Calculates fx for each table from 2..7, making sure everything matches on the way.
    let mut depth = 2;
    while depth < 8 {
        let mut f = FXCalculator::new(k, depth);
        let mut new_ys: Vec<BitVec> = Default::default();
        let mut new_metadata: Vec<BitVec> = Default::default();
        index = 0;
        while index < (1 << (8 - depth)) {
            let mut l_plot_entry = PlotEntry {
                y: 0,
                pos: 0,
                offset: 0,
                left_metadata: 0,
                right_metadata: 0,
                used: false,
                read_posoffset: 0,
            };
            let mut r_plot_entry = PlotEntry {
                y: 0,
                pos: 0,
                offset: 0,
                left_metadata: 0,
                right_metadata: 0,
                used: false,
                read_posoffset: 0,
            };

            l_plot_entry.y = ys[index as usize]
                .get_value()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Invalid L Plot Entry"))?;
            r_plot_entry.y = ys[index as usize + 1]
                .get_value()
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Invalid R Plot Entry"))?;

            // If there is no match, fails.
            let r_diff = r_plot_entry.y / K_BC as u64;
            let l_diff = l_plot_entry.y / K_BC as u64;
            let cdiff = r_diff.wrapping_sub(l_diff);
            let bucket_l: Vec<PlotEntry> = vec![l_plot_entry];
            let bucket_r: Vec<PlotEntry> = vec![r_plot_entry];
            if cdiff != 1 || f.find_matches(&bucket_l, &bucket_r, None, None) != 1 {
                return Ok(BitVec::new(0, 0));
            }
            let results = f.calculate_bucket(
                &ys[index as usize],
                &metadata[index as usize],
                &metadata[index as usize + 1],
            );
            new_ys.push(results.0);
            new_metadata.push(results.1);
            index += 2;
        }

        for new_y in &new_ys {
            if new_y.get_size() == 0 {
                return Ok(BitVec::new(0, 0));
            }
        }
        ys = new_ys;
        metadata = new_metadata;
        depth += 1;
    }

    let challenge_bits = BitVec::from_be_bytes(
        challenge,
        challenge.len() as u32,
        (challenge.len() * 8) as u32,
    );
    let quality_index = (challenge_bits
        .range(256 - 5, challenge_bits.get_size())
        .get_value()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Invalid u64 for Challenge Bits"))?
        << 1) as u16;

    // Makes sure the output is equal to the first k bits of the challenge
    match get_quality_string(k, &proof_bits.to_bytes(), quality_index, challenge) {
        Ok(qs) => {
            if challenge_bits.range(0, k as u32) == ys[0].range(0, k as u32) {
                // Returns quality string, which requires changing proof to plot ordering
                Ok(BitVec::from_be_bytes(qs, 32, 256))
            } else {
                Ok(BitVec::new(0, 0))
            }
        }
        Err(e) => {
            trace!("Error in get_quality_string: {}", e);
            Ok(BitVec::new(0, 0))
        }
    }
}

fn compare_proof_bits(left: &BitVec, right: &BitVec, k: u8) -> Result<bool, Error> {
    let size = left.get_size() / k as u32;
    if left.get_size() != right.get_size() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Right and Left are not Equal",
        ));
    }
    let mut i: i32 = size as i32 - 1;
    while i >= 0 {
        let left_val = left.range(k as u32 * i as u32, k as u32 * (i + 1) as u32);
        let right_val = right.range(k as u32 * i as u32, k as u32 * (i + 1) as u32);
        if left_val < right_val {
            return Ok(true);
        }
        if left_val > right_val {
            return Ok(false);
        }

        if i == 0 {
            break;
        }
        i -= 1;
    }
    Ok(false)
}

pub fn check_plot<T: AsRef<Path>>(path: T, challenges: usize) -> Result<(usize, usize), Error> {
    info!("Testing plot {:?}", path.as_ref());
    let prover = DiskProver::new(path.as_ref())?;
    let mut total_proofs = 0;
    let mut bad_proofs = 0;
    for i in 0..challenges {
        let challenge_hash = Bytes32::new(&hash_256(i.to_be_bytes()));
        let start = Instant::now();
        let qualities = prover.get_qualities_for_challenge(&challenge_hash)?;
        let duration = Instant::now().duration_since(start).as_millis();
        for (index, quality) in qualities.iter().enumerate() {
            if duration > 5000 {
                warn!("\tLooking up qualities took: {duration} ms. This should be below 5 seconds to minimize risk of losing rewards.");
            } else {
                debug!("\tLooking up qualities took: {duration} ms.");
            }
            let proof_start = Instant::now();
            let proof = prover.get_full_proof(&challenge_hash, index, true)?;
            let proof_duration = Instant::now().duration_since(proof_start).as_millis();
            if proof_duration > 15000 {
                warn!("\tFinding proof took: {proof_duration} ms. This should be below 15 seconds to minimize risk of losing rewards.");
            } else {
                debug!("\tFinding proof took: {proof_duration} ms");
            }
            total_proofs += 1;
            let v_quality = validate_proof(
                prover.header.id.to_sized_bytes(),
                prover.header.k,
                &challenge_hash.bytes,
                &proof,
            )?;
            if *quality != v_quality {
                bad_proofs += 1;
                error!("Error Proving Plot: {:?}", path.as_ref());
            }
        }
    }
    Ok((total_proofs, bad_proofs))
}

#[test]
pub fn test_check_plot() {
    let path = "/mnt/96acc2b7-d09d-4d27-a09c-a8b425a59813/plot-k32-2023-05-30-23-09-003fd7e478ccf85bddf96300461963bc9543e7b9cc0360ba429c40c5f0757edf.plot";
    let (total, bad) = check_plot(path, 30).unwrap();
    println!("Proofs Found: {total}/30, Bad Proofs: {bad}");
}

#[test]
pub fn test_parallel_check_plots() {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    let path = "/mnt/96acc2b7-d09d-4d27-a09c-a8b425a59813/";
    let mut total_plots = AtomicU64::new(0);
    let start = Instant::now();
    match Path::new(path).read_dir() {
        Ok(dir) => {
            dir.par_bridge().for_each(|entry| match entry {
                Ok(file) => {
                    let path = file.path();
                    if let Some(s) = path.extension() {
                        if s == "plot" {
                            let (total, bad) = check_plot(path, 30).unwrap();
                            println!("Proofs Found: {total}/30, Bad Proofs: {bad}");
                            total_plots.fetch_add(1, Ordering::AcqRel);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to read file {:?}", e);
                }
            });
        }
        Err(e) => {
            eprintln!("Failed to read Plot Directory {:?}, {:?}", path, e);
        }
    }
    println!(
        "Checked {} Plots in {} ms",
        total_plots.get_mut(),
        start.duration_since(Instant::now()).as_millis()
    );
}
