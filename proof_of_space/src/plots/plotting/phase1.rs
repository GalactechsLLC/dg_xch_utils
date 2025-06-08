use std::cmp::min;
use std::fs::File;
use std::io::{Error, ErrorKind};
use std::mem::swap;
use std::ops::Shl;
use std::path::{Path, PathBuf};
use log::info;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use crate::constants::{cdiv, ucdiv, PlotEntry, K_BATCH_SIZES, K_BC, K_EXTRA_BITS, K_OFFSET_SIZE, K_VECTOR_LENS};
use crate::entry_sizes::EntrySizes;
use crate::f_calc::{F1Calculator, FXCalculator};
use crate::plots::plotting::BucketSorter::{BucketSorter, BucketStorage};
use crate::utils::bit_reader::BitReader;

pub async fn phase1(
    tmp_files: &[&File],
    k: u8,
    plot_id: Bytes32,
    tmp_dir: &Path,
    filename: &Path,
    memory_size: u64,
    num_buckets: u32,
    log_num_buckets: u32,
    stripe_size: u32,
    num_threads: u8
) -> Result<Vec<()>, Error> {
    let phase_1_start = Instant::now();
    info!("Computing table 1");
    let f1_calc = F1Calculator::new(k, &plot_id.bytes());
    let t1_entry_size_bytes = EntrySizes::get_max_entry_size(k, 1, true);
    let mut f1_threads = JoinSet::new();
    let mut bucket_sorter_left = BucketSorter::new(
        BucketStorage::Disk(tmp_dir.to_path_buf(), filename.with_extension(".plot.p1.t1")),
        num_buckets,
        log_num_buckets,
        t1_entry_size_bytes,
        0,
        stripe_size
    );
    let entry_size_bytes = 16u64;
    let max_value: u64 = 1u64 << k;
    let right_buf_entries: u64 = 1u64 << K_BATCH_SIZES;
    for i in 0..num_threads {
        let bucket_sorter = &bucket_sorter_left;
        f1_threads.spawn(async move {
            let mut f1_entries = Vec::with_capacity(1usize << K_BATCH_SIZES);
            let mut output = Vec::with_capacity((right_buf_entries * entry_size_bytes) as usize);
            let loop_count = 1u64 << (k as u32 - K_BATCH_SIZES);
            for lp in (i as u64..=loop_count).step_by(num_threads as usize) {
                let mut x = lp * (1 << (K_BATCH_SIZES));
                let f1_loop_count = min(max_value - x, 1u64 << (K_BATCH_SIZES));
                f1_calc.calculate_buckets(x, f1_loop_count, &mut f1_entries);
                for i in 0.. f1_loop_count {
                    let mut entry = 0u128;
                    entry = (f1_entries[i] as u128).shl(128 - K_EXTRA_BITS - k);
                    entry = entry | (x as u128).shl(128 - K_EXTRA_BITS - 2 * k);
                    output.push(entry);
                    x += 1;
                }
                for entry in &output {
                    bucket_sorter.insert(&entry.to_be_bytes())?;
                }
                output.clear();
            }
            Ok(())
        });
    }
    f1_threads.join_all().await;
    let mut table_sizes = [0u64; 8];
    info!("\tF1 Completed in: {:.8} seconds", phase_1_start.elapsed().as_secs_f64());
    let prevtableentries = 1u64 << k;
    let pos_size = k;
    let mut right_entry_size_bytes = 0;
    let progress_percent = [0.06, 0.12, 0.2, 0.28, 0.36, 0.42];
    for table_index in 1u8..7u8 {
        let table_timer = Instant::now();
        let metadata_size = K_VECTOR_LENS[table_index + 1] * k;
        let entry_size_bytes = EntrySizes::get_max_entry_size(k, table_index, true);
        let mut compressed_entry_size_bytes = EntrySizes::get_max_entry_size(k, table_index, false);
        right_entry_size_bytes = EntrySizes::get_max_entry_size(k, table_index + 1, true);
        if table_index != 1 {
            compressed_entry_size_bytes = ucdiv((k + K_OFFSET_SIZE) as u32, 8);
            if table_index == 6 {
                right_entry_size_bytes = EntrySizes::get_key_pos_offset_size(k);
            }
        }
        info!("\tComputing table: {}", table_index + 1);
        info!("\tProgress update: {}", progress_percent[table_index - 1]);
        let bucket_sorter_right = BucketSorter::new(
            BucketStorage::Disk(tmp_dir.to_path_buf(), filename.with_extension(format!(".plot.p1.t{}", table_index + 1))),
            num_buckets,
            log_num_buckets,
            right_entry_size_bytes,
            0,
            stripe_size
        );
        bucket_sorter_left.trigger_new(0).await?;
        let matches = 0;
        let left_writer_count = 0;
        let right_writer_count = 0;
        let right_writer = 0;
        let left_writer = 0;
        let sems = &[Semaphore::new(0); 8];
        let mut phase1_threads = JoinSet::new();
        for i in 0..num_threads {
            let mine = &sems[i];
            let theirs = &sems[(num_threads + i - 1) % num_threads];
            phase1_threads.spawn(async move {
                let left_buf_entries = 5000 + ((1.1 * stripe_size) as u64);
                let right_buf_entries = 5000 + ((1.1 * stripe_size) as u64);
                let right_writer_buf = vec![0u8; (right_buf_entries * right_entry_size_bytes + 7) as usize];
                let left_writer_buf = vec![0u8; (left_buf_entries * compressed_entry_size_bytes + 7) as usize];
                let mut fx = FXCalculator::new(k, table_index + 1);
                let position_map_size = 2000;
                let mut l_position_map = vec![0u16; position_map_size];
                let mut r_position_map = vec![0u16; position_map_size];

                let totalstripes = (prevtableentries + stripe_size - 1) / stripe_size;
                let threadstripes = (totalstripes + num_threads - 1) / num_threads;
                for stripe in 0..threadstripes {
                    let mut pos = (stripe * num_threads + i) * stripe_size;
                    let endpos = pos + stripe_size + 1;  // one y value overlap
                    let mut left_reader = pos * entry_size_bytes;
                    let mut left_writer_count = 0;
                    let mut stripe_left_writer_count = 0;
                    let mut stripe_start_correction = 0xffffffffffffffff;
                    let mut right_writer_count = 0;
                    let mut matches = 0;  // Total matches

                    let mut bucket_l = vec![];
                    let mut bucket_r = vec![];

                    let mut bucket = 0u64;
                    let mut end_of_table = false;  // We finished all entries in the left table

                    let mut ignorebucket = 0xffffffffffffffffu64;
                    let mut b_match = false;
                    let mut b_first_stripe_overtime_pair = false;
                    let mut b_second_strip_overtime_pair = false;
                    let mut b_third_stripe_overtime_pair = false;

                    let mut b_stripe_pregame_pair = false;
                    let mut b_stripe_start_pair = false;
                    let mut need_new_bucket = false;
                    let first_thread = i % num_threads == 0;
                    let last_thread = i % num_threads == num_threads - 1;

                    let mut l_position_base = 0u64;
                    let mut r_position_base = 0u64;
                    let mut newlpos = 0u64;
                    let mut newrpos = 0u64;

                    // std::vector<std::tuple<PlotEntry, PlotEntry, std::pair<Bits, Bits>>>
                    //     current_entries_to_write;
                    // std::vector<std::tuple<PlotEntry, PlotEntry, std::pair<Bits, Bits>>>
                    //     future_entries_to_write;
                    let mut not_dropped: Vec<PlotEntry> = vec![];

                    if pos == 0 {
                        b_match = true;
                        b_stripe_pregame_pair = true;
                        b_stripe_start_pair = true;
                        stripe_left_writer_count = 0;
                        stripe_start_correction = 0;
                    }
                    theirs.aquire().await?;
                    need_new_bucket = bucket_sorter_left.close_to_new_bucket(left_reader);
                    if need_new_bucket {
                        if !first_thread {
                            theirs.aquire().await?;
                        }
                        bucket_sorter_left.trigger_new(left_reader).await?;
                    }
                    if !last_thread {
                        mine.add_permits(1);
                    }
                    while pos < prevtableentries + 1 {
                        let mut left_entry = PlotEntry::default();
                        if pos >= prevtableentries {
                            end_of_table = true;
                            left_entry.y = 0;
                            left_entry.left_metadata = 0;
                            left_entry.right_metadata = 0;
                            left_entry.used = false;
                        } else {
                            let left_buf = bucket_sorter_left.read_entry(left_reader).await?;
                            left_entry = GetLeftEntry(table_index, left_buf, k, metadata_size, pos_size);
                        }
                        left_entry.pos = pos;
                        left_entry.used = false;
                        let y_bucket = left_entry.y / K_BC;
                        if !b_match {
                            if ignorebucket == 0xffffffffffffffff {
                                ignorebucket = y_bucket;
                            } else {
                                if y_bucket != ignorebucket {
                                    bucket = y_bucket;
                                    b_match = true;
                                }
                            }
                        }
                        if !b_match {
                            stripe_left_writer_count += 1;
                            r_position_base = stripe_left_writer_count;
                            pos += 1;
                            continue;
                        }
                        if y_bucket == bucket {
                            bucket_l.push(left_entry);
                        } else if y_bucket == bucket + 1 {
                            bucket_r.push(left_entry);
                        } else {
                            let mut idx_l = [0u16; 10000];
                            let mut idx_r = [0u16; 10000];
                            let mut idx_count = 0;
                            if !bucket_l.is_empty() {
                                not_dropped.clear();
                                if !bucket_r.is_empty() {
                                    idx_count = fx.find_matches(&bucket_l, &bucket_r, Some(&mut idx_l), Some(&mut idx_r));
                                    if idx_count >= 10000 {
                                        return Err(Error::new(ErrorKind::InvalidInput, "sanity check: idx_count exceeded 10000!"))
                                    }
                                    for i in 0..idx_count {
                                        bucket_l[idx_l[i]].used = true;
                                        if end_of_table {
                                            bucket_r[idx_r[i]].used = true;
                                        }
                                    }
                                }
                                for bucket_index in 0..bucket_l.len() {
                                    let l_entry = &bucket_l[bucket_index];
                                    if l_entry.used {
                                        not_dropped.push(l_entry);
                                    }
                                }
                                if end_of_table {
                                    for bucket_index in 0..bucket_r.len() {
                                        let r_entry = &bucket_r[bucket_index];
                                        if r_entry.used {
                                            not_dropped.push(&r_entry);
                                        }
                                    }
                                }
                                swap(&mut l_position_map, &mut r_position_map);
                                l_position_base = r_position_base;
                                r_position_base = stripe_left_writer_count;
                                for entry in not_dropped {
                                    r_position_map[entry.pos % position_map_size] = stripe_left_writer_count - r_position_base;
                                    if b_stripe_start_pair {
                                        if stripe_start_correction == 0xffffffffffffffff {
                                            stripe_start_correction = stripe_left_writer_count;
                                        }
                                        if left_writer_count >= left_buf_entries {
                                            return Err(Error::new(ErrorKind::InvalidInput, "Left writer count overrun"));
                                        }
                                        let tmp_buf = left_writer_buf.get() + left_writer_count * compressed_entry_size_bytes;
                                        left_writer_count += 1;
                                        let mut new_left_entry = if table_index == 1 {
                                            entry.left_metadata
                                        } else {
                                            entry.read_posoffset
                                        };
                                        new_left_entry <<= 64 - if table_index == 1 {
                                            k
                                        } else {
                                            pos_size + K_OFFSET_SIZE
                                        };

                                        Util::IntToEightBytes(tmp_buf, new_left_entry);
                                    }
                                    stripe_left_writer_count += 1;
                                }
                                current_entries_to_write = future_entries_to_write;
                                future_entries_to_write.clear();
                                for i in 0..idx_count {
                                    let l_entry = bucket_l[idx_l[i]];
                                    let mut r_entry = bucket_r[idx_r[i]];
                                    if b_stripe_start_pair {
                                        matches += 1;
                                    }
                                    r_entry.used = true;
                                    if metadata_size <= 128 {
                                        let f_output = fx.calculate_bucket(
                                            &BitReader::new(l_entry.y, k + K_EXTRA_BITS),
                                            &BitReader::new(l_entry.left_metadata, metadata_size),
                                            &BitReader::new(r_entry.left_metadata, metadata_size));
                                        future_entries_to_write.emplace_back(l_entry, r_entry, f_output);
                                    } else {
                                        let f_output = fx.calculate_bucket(
                                            &BitReader::new(l_entry.y, k + K_EXTRA_BITS),
                                            &BitReader::new(l_entry.left_metadata, 128) +
                                                &BitReader::new(l_entry.right_metadata, metadata_size - 128),
                                            &BitReader::new(r_entry.left_metadata, 128) +
                                                &BitReader::new(r_entry.right_metadata, metadata_size - 128));
                                        future_entries_to_write.emplace_back(l_entry, r_entry, f_output);
                                    }
                                }
                                let final_current_entry_size = current_entries_to_write.size();
                                if end_of_table {
                                    current_entries_to_write.insert(
                                        current_entries_to_write.end(),
                                        future_entries_to_write.begin(),
                                        future_entries_to_write.end());
                                }
                                for i in 0..current_entries_to_write.len() {
                                    let (L_entry, R_entry, f_output) = current_entries_to_write[i];
                                    Bits new_entry = table_index + 1 == 7 ? std::get<0>(f_output).Slice(0, k)
                                        : std::get<0>(f_output);
                                    if !end_of_table || i < final_current_entry_size {
                                        newlpos =
                                            l_position_map[L_entry.pos % position_map_size] + l_position_base;
                                    } else {
                                        newlpos =
                                            r_position_map[L_entry.pos % position_map_size] + r_position_base;
                                    }
                                    newrpos = r_position_map[R_entry.pos % position_map_size] + r_position_base;
                                    new_entry.AppendValue(newlpos, pos_size);
                                    if (newrpos - newlpos > (1U << K_OFFSET_SIZE) * 97 / 100) {
                                        throw InvalidStateException(
                                            "Offset too large: " + std::to_string(newrpos - newlpos));
                                    }
                                    new_entry.AppendValue(newrpos - newlpos, K_OFFSET_SIZE);
                                    new_entry += std::get<1>(f_output);
                                    if right_writer_count >= right_buf_entries {
                                        return Err(Error::new(ErrorKind::InvalidInput, "Left writer count overrun"));
                                    }
                                    if b_stripe_start_pair {
                                        let right_buf = right_writer_buf[(right_writer_count * right_entry_size_bytes)..];
                                        new_entry.ToBytes(right_buf);
                                        right_writer_count += 1;
                                    }
                                }
                            }
                            if pos >= endpos {
                                if !b_first_stripe_overtime_pair {
                                    b_first_stripe_overtime_pair = true;
                                } else if !b_second_strip_overtime_pair {
                                    b_second_strip_overtime_pair = true;
                                } else if !b_third_stripe_overtime_pair {
                                    b_third_stripe_overtime_pair = true;
                                } else {
                                    break;
                                }
                            } else {
                                if !b_stripe_pregame_pair {
                                    b_stripe_pregame_pair = true;
                                }
                                else if !b_stripe_start_pair {
                                    b_stripe_start_pair = true;
                                }
                            }
                            if y_bucket == bucket + 2 {
                                bucket_l = bucket_r;
                                bucket_r = vec![];
                                bucket_r.push(left_entry);
                                bucket += 1;
                            } else {
                                bucket = y_bucket;
                                bucket_l.clear();
                                bucket_l.push(left_entry);
                                bucket_r.clear();
                            }
                        }
                        pos += 1;
                    }
                    if !need_new_bucket && !first_thread {
                        theirs.aquire().await?;//Sem::Wait(ptd->theirs);
                    }
                    uint32_t const ysize = (table_index + 1 == 7) ? k : k + K_EXTRA_BITS;
                    uint32_t const startbyte = ysize / 8;
                    uint32_t const endbyte = (ysize + pos_size + 7) / 8 - 1;
                    uint64_t const shiftamt = (8 - ((ysize + pos_size) % 8)) % 8;
                    uint64_t const correction = (globals.left_writer_count - stripe_start_correction) << shiftamt;
                    for (uint32_t i = 0; i < right_writer_count; i++) {
                        uint64_t posaccum = 0;
                        uint8_t* entrybuf = right_writer_buf.get() + i * right_entry_size_bytes;

                        for (uint32_t j = startbyte; j <= endbyte; j++) {
                            posaccum = (posaccum << 8) | (entrybuf[j]);
                        }
                        posaccum += correction;
                        for (uint32_t j = endbyte; j >= startbyte; --j) {
                            entrybuf[j] = posaccum & 0xff;
                            posaccum = posaccum >> 8;
                        }
                    }
                    if (table_index < 6) {
                        for (uint64_t i = 0; i < right_writer_count; i++) {
                            globals.R_sort_manager->AddToCache(right_writer_buf.get() + i * right_entry_size_bytes);
                        }
                    } else {
                        (*ptmp_1_disks)[table_index + 1].Write(
                            globals.right_writer,
                            right_writer_buf.get(),
                            right_writer_count * right_entry_size_bytes);
                    }
                    globals.right_writer += right_writer_count * right_entry_size_bytes;
                    globals.right_writer_count += right_writer_count;

                    (*ptmp_1_disks)[table_index].Write(
                        globals.left_writer, left_writer_buf.get(), left_writer_count * compressed_entry_size_bytes
                    );
                    globals.left_writer += left_writer_count * compressed_entry_size_bytes;
                    globals.left_writer_count += left_writer_count;
                    globals.matches += matches;
                    Sem::Post(ptd->mine);
                }
                Ok(())
            });
        }
        info!("\tTotal matches {matches}");
        table_sizes[table_index] = left_writer_count;
        table_sizes[table_index + 1] = right_writer_count;
        tmp_files[table_index].Truncate(left_writer);
        if table_index < 6 {
            bucket_sorter_right.flush().await?;
            bucket_sorter_left = bucket_sorter_right;
        } else {
            tmp_files[table_index + 1].Truncate(right_writer);
        }
        if matches != right_writer_count {
            return Err(Error::other( format!(
                "Matches do not match with number of write entries: Matches {matches} vs Writer Count {right_writer_count}"
            )));
        }
    }
    table_sizes[0] = 0;
    table_sizes
}