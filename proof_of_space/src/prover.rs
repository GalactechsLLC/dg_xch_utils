use crate::bitvec::BitVec;
use crate::constants::*;
use crate::encoding::{ans_decode_deltas, line_point_to_square};
use crate::entry_sizes::EntrySizes;
use crate::f_calc::{F1Calculator, FXCalculator};
use crate::util::{bytes_to_u64, slice_u128from_bytes};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::plots::{PlotHeader, PlotMemo};
use dg_xch_serialize::hash_256;
use log::trace;
use nix::libc;
use std::cmp::min;
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

#[derive(Debug, Clone)]
pub struct DiskProver {
    pub version: u16,
    pub filename: Arc<PathBuf>,
    pub header: Arc<PlotHeader>,
    table_begin_pointers: Arc<[u64; 11]>,
    c2: Vec<u64>,
}
impl DiskProver {
    pub fn new(path: &Path) -> Result<Self, Error> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT & libc::O_SYNC)
            .open(path)?;
        let header = read_plot_header(&mut file)?;
        trace!("Plot ID: {:?}", &header.id);
        let mut table_begin_pointers: [u64; 11] = [0; 11];
        let mut u64_buf: [u8; 8] = [0; 8];
        for pointer in &mut table_begin_pointers[1..11] {
            file.read_exact(&mut u64_buf)?;
            *pointer = u64::from_be_bytes(u64_buf);
        }
        file.seek(SeekFrom::Start(table_begin_pointers[9]))?;
        let c2_size: u8 = (byte_align(header.k as u32) / 8) as u8;
        let c2_entries: u32 =
            ((table_begin_pointers[10] - table_begin_pointers[9]) / c2_size as u64) as u32;
        if c2_entries == 0 || c2_entries == 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid C2 table size"));
        }
        let mut prev_c2_f7: u64 = 0;
        let mut c2_buf = vec![0; c2_size as usize];
        let mut c2 = vec![];
        for _ in 0..c2_entries - 1 {
            file.read_exact(&mut c2_buf)?;
            let f7 = BitVec::from_be_bytes(&c2_buf, c2_size as u32, (c2_size * 8) as u32)
                .range(0, header.k as u32)
                .get_value_unchecked();
            if f7 < prev_c2_f7 {
                break;
            }
            c2.push(f7);
            prev_c2_f7 = f7;
        }
        Ok(DiskProver {
            version: VERSION,
            filename: Arc::new(path.to_path_buf()),
            header: Arc::new(header),
            table_begin_pointers: Arc::new(table_begin_pointers),
            c2,
        })
    }

    pub fn get_qualities_for_challenge(&self, challenge: &Bytes32) -> Result<Vec<BitVec>, Error> {
        // This tells us how many f7 outputs (and therefore proofs) we have for this
        // challenge. The expected value is one proof.
        let mut qualities = vec![];
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT & libc::O_SYNC)
            .open(&*self.filename)?;
        let p7_entries = self.get_p7_entries(&mut file, challenge)?;
        if p7_entries.is_empty() {
            return Ok(vec![]);
        }
        // The last 5 bits of the challenge determine which route we take to get to
        // our two x values in the leaves.
        let last_5_bits: u8 = challenge.bytes[31] & 0x1f;
        for mut position in p7_entries {
            // This inner loop goes from table 6 to table 1, getting the two back-pointers,
            // and following one of them.
            for table_index in (2..=6).rev() {
                let line_point = Self::read_line_point(
                    self.header.clone(),
                    self.table_begin_pointers.clone(),
                    &mut file,
                    table_index,
                    position,
                )?;
                let (x, y) = line_point_to_square(line_point);
                //assert(xy.first >= xy.second);
                if ((last_5_bits >> (table_index - 2)) & 1) == 0 {
                    position = y;
                } else {
                    position = x;
                }
            }
            let new_line_point = Self::read_line_point(
                self.header.clone(),
                self.table_begin_pointers.clone(),
                &mut file,
                1,
                position,
            )?;
            let (x1, x2) = line_point_to_square(new_line_point);
            let k = self.header.k;
            // The final two x values (which are stored in the same location) are hashed
            let mut hash_input = vec![];
            hash_input.extend(challenge.bytes.iter());
            hash_input.extend(
                (BitVec::new(x2 as u128, k as u32) + BitVec::new(x1 as u128, k as u32)).to_bytes(),
            );
            qualities.push(BitVec::from_be_bytes(hash_256(&hash_input), 32, 256));
        }
        Ok(qualities)
    }

    fn read_line_point(
        header: Arc<PlotHeader>,
        table_begin_pointers: Arc<[u64; 11]>,
        disk_file: &mut File,
        table_index: u8,
        position: u64,
    ) -> Result<u128, Error> {
        let park_index = position / K_ENTRIES_PER_PARK as u64;
        let park_size_bits = EntrySizes::calculate_park_size(header.k, table_index) * 8;
        disk_file.seek(SeekFrom::Start(
            table_begin_pointers[table_index as usize] + (park_size_bits as u64 / 8) * park_index,
        ))?;

        // This is the checkpoint at the beginning of the park
        let line_point_size = EntrySizes::calculate_line_point_size(header.k);
        let mut line_point_bin = vec![0; line_point_size as usize];
        disk_file.read_exact(&mut line_point_bin)?;
        let line_point = slice_u128from_bytes(line_point_bin, 0, header.k as u32 * 2);

        // Reads EPP stubs
        let stubs_size_bits = EntrySizes::calculate_stubs_size(header.k as u32) * 8;
        let mut stubs_bin = vec![0; (stubs_size_bits / 8) as usize];
        disk_file.read_exact(&mut stubs_bin)?;

        // Reads EPP deltas
        let max_deltas_size_bits = EntrySizes::calculate_max_deltas_size(table_index) * 8;

        // Reads the size of the encoded deltas object
        let mut encoded_deltas_buf: [u8; 2] = [0; 2];
        disk_file.read_exact(&mut encoded_deltas_buf)?;
        let mut encoded_deltas_size = u16::from_le_bytes(encoded_deltas_buf);
        if encoded_deltas_size * 8 > max_deltas_size_bits as u16 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid size for deltas: {}", encoded_deltas_size),
            ));
        }
        let mut deltas;
        if 0x8000 & encoded_deltas_size > 0 {
            // Uncompressed
            encoded_deltas_size &= 0x7fff;
            deltas = vec![0u8; encoded_deltas_size as usize];
            disk_file.read_exact(&mut deltas)?;
        } else {
            // Compressed
            let mut deltas_bin = vec![0; (max_deltas_size_bits / 8) as usize];
            disk_file.read_exact(&mut deltas_bin)?;
            // Decodes the deltas
            let r = K_RVALUES[(table_index - 1) as usize];
            deltas = ans_decode_deltas(
                &deltas_bin,
                encoded_deltas_size as usize,
                (K_ENTRIES_PER_PARK - 1) as usize,
                r,
            )?;
        }
        let mut start_bit: usize = 0;
        let stub_size = header.k - K_STUB_MINUS_BITS;
        let mut sum_deltas: u64 = 0;
        let mut sum_stubs: u64 = 0;
        let mut stub;
        for delta in deltas.iter().take(min(
            (position % K_ENTRIES_PER_PARK as u64) as usize,
            deltas.len(),
        )) {
            stub = bytes_to_u64(&stubs_bin[(start_bit / 8)..]);
            stub <<= start_bit % 8;
            stub >>= 64 - stub_size;
            sum_stubs += stub;
            start_bit += stub_size as usize;
            sum_deltas += *delta as u64;
        }
        let big_delta = ((sum_deltas as u128) << stub_size) + sum_stubs as u128;
        let final_line_point = line_point + big_delta;
        Ok(final_line_point)
    }

    fn get_p7_positions<T: AsRef<[u8]>>(
        &self,
        mut curr_f7: u64,
        f7: u64,
        mut curr_p7_pos: u64,
        bit_mask: T,
        encoded_size: usize,
        c1_index: u64,
    ) -> Result<Vec<u64>, Error> {
        let deltas = ans_decode_deltas(
            bit_mask.as_ref(),
            encoded_size,
            K_CHECKPOINT1INTERVAL as usize,
            K_C3R,
        )?;
        let mut p7_positions = vec![];
        let mut surpassed_f7 = false;
        for delta in deltas {
            if curr_f7 > f7 {
                surpassed_f7 = true;
                break;
            }
            curr_f7 += delta as u64;
            curr_p7_pos += 1;
            if curr_f7 == f7 {
                p7_positions.push(curr_p7_pos);
            }
            // In the last park, we don't know how many entries we have, and there is no stop marker
            // for the deltas. The rest of the park bytes will be set to 0, and
            // at this point curr_f7 stops incrementing. If we get stuck in this loop
            // where curr_f7 == f7, we will not return any positions, since we do not know if
            // we have an actual solution for f7.
            if curr_p7_pos >= ((c1_index + 1) * K_CHECKPOINT1INTERVAL as u64) - 1
                || curr_f7 >= (1u64 << self.header.k) - 1
            {
                break;
            }
        }
        if !surpassed_f7 {
            return Ok(vec![]);
        }
        Ok(p7_positions)
    }

    // Returns P7 table entries (which are positions into table P6), for a given challenge
    fn get_p7_entries(
        &self,
        disk_file: &mut (impl Read + Seek),
        challenge: &Bytes32,
    ) -> Result<Vec<u64>, Error> {
        let k = self.header.k;
        if self.c2.is_empty() {
            return Ok(vec![]);
        }
        let challenge_bits = BitVec::from_be_bytes(&challenge.bytes, 256 / 8, 256);

        // The first k bits determine which f7 matches with the challenge.
        let f7 = challenge_bits
            .range(0, self.header.k as u32)
            .get_value_unchecked();

        let mut c1_index = 0u64;
        let mut broke = false;
        let mut c2_entry_f = 0;
        // Goes through C2 entries until we find the correct C2 checkpoint. We read each entry,
        // comparing it to our target (f7).
        for c2_entry in &self.c2 {
            c2_entry_f = *c2_entry;
            if f7 < *c2_entry {
                // If we passed our target, go back by one.
                c1_index -= K_CHECKPOINT2INTERVAL as u64;
                broke = true;
                break;
            }
            c1_index += K_CHECKPOINT2INTERVAL as u64;
        }
        if !broke {
            // If we didn't break, go back by one, to get the final checkpoint.
            c1_index -= K_CHECKPOINT2INTERVAL as u64;
        }
        let c1_entry_size: usize = (byte_align(k as u32) / 8) as usize;
        let mut c1_entry_bytes = vec![0; c1_entry_size];

        disk_file.seek(SeekFrom::Start(
            self.table_begin_pointers[8] + c1_index * byte_align(k as u32) as u64 / 8,
        ))?;

        let mut curr_f7 = c2_entry_f;
        let mut prev_f7 = c2_entry_f;
        broke = false;
        // Goes through C2 entries until we find the correct C1 checkpoint.
        for start in 0..K_CHECKPOINT1INTERVAL {
            disk_file.read_exact(&mut c1_entry_bytes)?;
            let c1_entry = BitVec::from_be_bytes(
                &c1_entry_bytes,
                byte_align(k as u32) / 8,
                byte_align(k as u32),
            );
            let read_f7 = c1_entry.range(0, k as u32).get_value_unchecked();
            if start != 0 && read_f7 == 0 {
                // We have hit the end of the checkpoint list
                break;
            }
            curr_f7 = read_f7;
            if f7 < curr_f7 {
                // We have passed the number we are looking for, so go back by one
                curr_f7 = prev_f7;
                c1_index -= 1;
                broke = true;
                break;
            }
            c1_index += 1;
            prev_f7 = curr_f7;
        }
        if !broke {
            // We never broke, so go back by one.
            c1_index -= 1;
        }

        let c3_entry_size: usize = EntrySizes::calculate_c3size(k) as usize;
        // Double entry means that our entries are in more than one checkpoint park.
        let double_entry = f7 == curr_f7 && c1_index > 0;

        let next_f7;
        let mut encoded_size_buf: [u8; 2] = [0; 2];
        let mut encoded_size;
        let mut p7_positions = vec![];
        let mut curr_p7_pos = c1_index * K_CHECKPOINT1INTERVAL as u64;
        let mut bit_mask;
        if double_entry {
            // In this case, we read the previous park as well as the current one
            c1_index -= 1;
            disk_file.seek(SeekFrom::Start(
                self.table_begin_pointers[8] + c1_index * byte_align(k as u32) as u64 / 8,
            ))?;
            disk_file.read_exact(&mut c1_entry_bytes)?;
            let c1_entry_bits = BitVec::from_be_bytes(
                c1_entry_bytes,
                byte_align(k as u32) / 8,
                byte_align(k as u32),
            );
            next_f7 = curr_f7;
            curr_f7 = c1_entry_bits.range(0, k as u32).get_value_unchecked();

            disk_file.seek(SeekFrom::Start(
                self.table_begin_pointers[10] + c1_index * c3_entry_size as u64,
            ))?;

            disk_file.read_exact(&mut encoded_size_buf)?;
            encoded_size =
                BitVec::from_be_bytes(encoded_size_buf, 2, 16).get_value_unchecked() as u16;

            // Avoid telling GetP7Positions and functions it uses that we have more
            // bytes than we allocated for bit_mask above.
            if encoded_size > (c3_entry_size - 2) as u16 {
                return Ok(vec![]);
            }

            bit_mask = vec![0; c3_entry_size];
            disk_file.read_exact(&mut bit_mask)?;

            let mut p7_positions = self.get_p7_positions(
                curr_f7,
                f7,
                curr_p7_pos,
                &bit_mask,
                encoded_size as usize,
                c1_index,
            )?;

            disk_file.read_exact(&mut encoded_size_buf)?;
            encoded_size =
                BitVec::from_be_bytes(encoded_size_buf, 2, 16).get_value_unchecked() as u16;

            // Avoid telling GetP7Positions and functions it uses that we have more
            // bytes than we allocated for bit_mask above.
            if encoded_size > (c3_entry_size - 2) as u16 {
                return Ok(vec![]);
            }

            disk_file.read_exact(&mut bit_mask)?;
            c1_index += 1;
            curr_p7_pos = c1_index * K_CHECKPOINT1INTERVAL as u64;
            let second_positions = self.get_p7_positions(
                next_f7,
                f7,
                curr_p7_pos,
                &bit_mask,
                encoded_size as usize,
                c1_index,
            )?;
            p7_positions.extend(second_positions);
        } else {
            disk_file.seek(SeekFrom::Start(
                self.table_begin_pointers[10] + c1_index * c3_entry_size as u64,
            ))?;
            disk_file.read_exact(&mut encoded_size_buf)?;
            encoded_size =
                BitVec::from_be_bytes(encoded_size_buf, 2, 16).get_value_unchecked() as u16;

            // Avoid telling GetP7Positions and functions it uses that we have more
            // bytes than we allocated for bit_mask above.
            if encoded_size > (c3_entry_size - 2) as u16 {
                return Ok(vec![]);
            }
            bit_mask = vec![0; c3_entry_size - 2];

            disk_file.read_exact(&mut bit_mask)?;

            p7_positions = self.get_p7_positions(
                curr_f7,
                f7,
                curr_p7_pos,
                &bit_mask,
                encoded_size as usize,
                c1_index,
            )?;
        }

        // p7_positions is a list of all the positions into table P7, where the output is equal to
        // f7. If it's empty, no proofs are present for this f7.
        if p7_positions.is_empty() {
            return Ok(vec![]);
        }

        let p7_park_size_bytes: u32 = byte_align((k as u32 + 1) * K_ENTRIES_PER_PARK) / 8;
        let mut p7_entries = vec![];

        // Given the p7 positions, which are all adjacent, we can read the pos6 values from table
        // P7.
        let mut p7_park_buf = vec![0; p7_park_size_bytes as usize];
        let park_index = if p7_positions[0] == 0 {
            0
        } else {
            p7_positions[0]
        } / K_ENTRIES_PER_PARK as u64;
        disk_file.seek(SeekFrom::Start(
            self.table_begin_pointers[7] + park_index * p7_park_size_bytes as u64,
        ))?;
        disk_file.read_exact(&mut p7_park_buf)?;
        let mut p7_park =
            BitVec::from_be_bytes(&p7_park_buf, p7_park_size_bytes, p7_park_size_bytes * 8);
        for i in 0..(p7_positions[p7_positions.len() - 1] - p7_positions[0] + 1) {
            let new_park_index = (p7_positions[i as usize]) / K_ENTRIES_PER_PARK as u64;
            if new_park_index > park_index {
                disk_file.seek(SeekFrom::Start(
                    self.table_begin_pointers[7] + new_park_index * p7_park_size_bytes as u64,
                ))?;
                disk_file.read_exact(&mut p7_park_buf)?;
                p7_park =
                    BitVec::from_be_bytes(&p7_park_buf, p7_park_size_bytes, p7_park_size_bytes * 8);
                p7_park_buf.clear();
            }
            let start_bit_index =
                (p7_positions[i as usize] % K_ENTRIES_PER_PARK as u64) * (k as u64 + 1);
            let p7_int = p7_park
                .range(
                    start_bit_index as u32,
                    (start_bit_index + k as u64 + 1) as u32,
                )
                .get_value_unchecked();
            p7_entries.push(p7_int);
        }
        Ok(p7_entries)
    }

    pub fn get_full_proof(
        &self,
        challenge: &Bytes32,
        index: usize,
        parallel_read: bool,
    ) -> Result<BitVec, Error> {
        let mut full_proof = BitVec::new(0, 0);
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT & libc::O_SYNC)
            .open(&*self.filename)?;
        let p7_entries = self.get_p7_entries(&mut file, challenge)?;
        if p7_entries.is_empty() || index >= p7_entries.len() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "No proof of space for this challenge",
            ));
        }
        // Gets the 64 leaf x values, concatenated together into a k*64 bit string.
        let xs = Self::get_inputs(
            self.header.clone(),
            self.table_begin_pointers.clone(),
            self.filename.clone(),
            &mut file,
            p7_entries[index],
            6,
            parallel_read,
        )?;
        // Sorts them according to proof ordering, where
        // f1(x0) m= f1(x1), f2(x0, x1) m= f2(x2, x3), etc. On disk, they are not stored in
        // proof ordering, they're stored in plot ordering, due to the sorting in the Compress
        // phase.
        let xs_sorted = self.reorder_proof(&xs)?;
        for x in xs_sorted {
            full_proof += x;
        }
        Ok(full_proof)
    }

    // Changes a proof of space (64 k bit x values) from plot ordering to proof ordering.
    // Proof ordering: x1..x64 s.t.
    //  f1(x1) m= f1(x2) ... f1(x63) m= f1(x64)
    //  f2(C(x1, x2)) m= f2(C(x3, x4)) ... f2(C(x61, x62)) m= f2(C(x63, x64))
    //  ...
    //  f7(C(....)) == challenge
    //
    // Plot ordering: x1..x64 s.t.
    //  f1(x1) m= f1(x2) || f1(x2) m= f1(x1) .....
    //  For all the levels up to f7
    //  AND x1 < x2, x3 < x4
    //     C(x1, x2) < C(x3, x4)
    //     For all comparisons up to f7
    //     Where a < b is defined as:  max(b) > max(a) where a and b are lists of k bit elements
    fn reorder_proof(&self, xs_input: &[BitVec]) -> Result<Vec<BitVec>, Error> {
        let k = self.header.k;
        let f1 = F1Calculator::new(k, &self.header.id.to_sized_bytes());
        let mut results = vec![];
        let mut xs = BitVec::new(0, 0);
        // Calculates f1 for each of the inputs
        for i in 0..64 {
            let res = f1.calculate_bucket(&xs_input[i])?;
            //println!("{}:{}", res.0.values[0], res.1.values[0]);
            results.push(res);
            xs += &results[i].1;
        }
        // The plotter calculates f1..f7, and at each level, decides to swap or not swap. Here, we
        // are doing a similar thing, we swap left and right, such that we end up with proof
        // ordering.
        for table_index in 2..8 {
            let mut new_xs = BitVec::new(0, 0);
            // New results will be a list of pairs of (y, metadata), it will decrease in size by 2x
            // at each iteration of the outer loop.
            let mut new_results = vec![];
            let f = FXCalculator::new(k, table_index);
            // Iterates through pairs of things, starts with 64 things, then 32, etc, up to 2.
            let mut i = 0;
            while i < results.len() {
                let new_output;
                // Compares the buckets of both ys, to see which one goes on the left, and which
                // one goes on the right
                if results[i].0.get_value() < results[i + 1].0.get_value() {
                    new_output =
                        f.calculate_bucket(&results[i].0, &results[i].1, &results[i + 1].1);
                    let start = k as u64 * i as u64 * (1u64 << (table_index - 2));
                    let end = k as u64 * (i as u64 + 2) * (1u64 << (table_index - 2));
                    new_xs += xs.range(start as u32, end as u32);
                } else {
                    // Here we switch the left and the right
                    new_output =
                        f.calculate_bucket(&results[i + 1].0, &results[i + 1].1, &results[i].1);
                    let start = k as u64 * i as u64 * (1u64 << (table_index - 2));
                    let start2 = k as u64 * (i as u64 + 1) * (1u64 << (table_index - 2));
                    let end = k as u64 * (i as u64 + 2) * (1u64 << (table_index - 2));
                    new_xs +=
                        xs.range(start2 as u32, end as u32) + xs.range(start as u32, start2 as u32);
                }
                assert_ne!(new_output.0.get_size(), 0);
                new_results.push(new_output);
                i += 2
            }
            // Advances to the next table
            // xs is a concatenation of all 64 x values, in the current order. Note that at each
            // iteration, we can swap several parts of xs
            results = new_results;
            //println!("New Results: {}", results.len());
            // for result in &results {
            //     println!("{} : {}", result.0.values[0], result.1.values[0]);
            // }
            xs = new_xs;
        }
        let mut ordered_proof = vec![];
        for i in 0..64 {
            ordered_proof.push(xs.range(i as u32 * k as u32, (i as u32 + 1) * k as u32));
        }
        Ok(ordered_proof)
    }

    // Recursive function to go through the tables on disk, backpropagating and fetching
    // all of the leaves (x values). For example, for depth=5, it fetches the position-th
    // entry in table 5, reading the two back pointers from the line point, and then
    // recursively calling GetInputs for table 4.
    fn get_inputs(
        header: Arc<PlotHeader>,
        table_begin_pointers: Arc<[u64; 11]>,
        file_name: Arc<PathBuf>,
        disk_file: &mut File,
        position: u64,
        depth: u8,
        parallel: bool,
    ) -> Result<Vec<BitVec>, Error> {
        let k = header.k as u32;
        let line_point = Self::read_line_point(
            header.clone(),
            table_begin_pointers.clone(),
            disk_file,
            depth,
            position,
        )?;
        let (x, y) = line_point_to_square(line_point);
        if depth == 1 {
            // For table P1, the line point represents two concatenated x values.
            Ok(vec![BitVec::new(y as u128, k), BitVec::new(x as u128, k)])
        } else {
            let mut left;
            if parallel {
                let mut left_file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECT & libc::O_SYNC)
                    .open(&*file_name)?;
                let mut right_file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECT & libc::O_SYNC)
                    .open(&*file_name)?;
                let l_arcs = (
                    header.clone(),
                    table_begin_pointers.clone(),
                    file_name.clone(),
                );
                let r_arcs = (header, table_begin_pointers, file_name);
                let left_handle = thread::spawn(move || {
                    Self::get_inputs(
                        l_arcs.0,
                        l_arcs.1,
                        l_arcs.2,
                        &mut left_file,
                        y,
                        depth - 1,
                        parallel,
                    )
                });
                let right_handle = thread::spawn(move || {
                    Self::get_inputs(
                        r_arcs.0,
                        r_arcs.1,
                        r_arcs.2,
                        &mut right_file,
                        x,
                        depth - 1,
                        parallel,
                    )
                });
                if let (Ok(Ok(l)), Ok(Ok(r))) = (left_handle.join(), right_handle.join()) {
                    left = l;
                    left.extend(r);
                } else {
                    return Err(Error::new(ErrorKind::InvalidData, "Failed to get inputs"));
                }
            } else {
                left = Self::get_inputs(
                    header.clone(),
                    table_begin_pointers.clone(),
                    file_name.clone(),
                    disk_file,
                    y,
                    depth - 1,
                    parallel,
                )?;
                let right = Self::get_inputs(
                    header,
                    table_begin_pointers,
                    file_name,
                    disk_file,
                    x,
                    depth - 1,
                    parallel,
                )?;
                left.extend(right);
            }
            Ok(left)
        }
    }
}

pub fn read_plot_file_header(p: impl AsRef<Path>) -> Result<(PathBuf, PlotHeader), Error> {
    if !p.as_ref().is_file() {
        return Err(Error::new(ErrorKind::InvalidInput, "Path must be a file"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT & libc::O_SYNC)
        .open(&p)?;
    Ok((p.as_ref().to_path_buf(), read_plot_header(&mut file)?))
}

pub fn read_plot_header(file: &mut File) -> Result<PlotHeader, Error> {
    let mut plot_header = PlotHeader::default();
    file.read_exact(&mut plot_header.magic)?;
    if HEADER_MAGIC != plot_header.magic {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid plot header magic",
        ));
    }
    let mut plot_header_buf = [0u8; 32];
    file.read_exact(&mut plot_header_buf)?;
    plot_header.id = plot_header_buf.into();
    let mut k_buf: [u8; 1] = [0; 1];
    file.read_exact(&mut k_buf)?;
    plot_header.k = k_buf[0];
    let mut format_len_buf = [0; 2];
    file.read_exact(&mut format_len_buf)?;
    plot_header.format_desc_len = u16::from_be_bytes(format_len_buf);
    let mut format_buf = vec![0; plot_header.format_desc_len as usize];
    file.read_exact(format_buf.as_mut_slice())?;
    plot_header.format_desc = format_buf;
    let mut memo_len_buf = [0; 2];
    file.read_exact(&mut memo_len_buf)?;
    plot_header.memo_len = u16::from_be_bytes(memo_len_buf);
    let mut memo_buf = vec![0; plot_header.memo_len as usize];
    file.read_exact(memo_buf.as_mut_slice())?;
    plot_header.memo = PlotMemo::try_from(memo_buf)?;
    Ok(plot_header)
}
