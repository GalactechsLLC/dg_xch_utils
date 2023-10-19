use crate::utils::calc_thread_vars;
use crate::utils::span::Span;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::cmp::max;
use std::mem::{size_of, swap};
use std::sync::Arc;

pub const RADIX: usize = 256;
pub const SHIFT_BASE: u8 = 8;
pub const ZEROS: [u64; RADIX] = [0u64; RADIX];
pub const ONES: [u64; RADIX] = [1u64; RADIX];

pub struct RadixSorter {
    thread_count: usize,
    total_count: usize,
    entries_per_thread: usize,
    counts: Vec<u64>,
    prefix_sums: Vec<u64>,
    thread_pool: ThreadPool,
}
impl RadixSorter {
    pub fn new(thread_count: usize, total_count: usize) -> Self {
        let entries_per_thread = max(total_count / thread_count, 1);
        Self {
            thread_count,
            total_count,
            entries_per_thread,
            counts: vec![0u64; thread_count * RADIX],
            prefix_sums: vec![0u64; thread_count * RADIX],
            thread_pool: ThreadPoolBuilder::new()
                .num_threads(thread_count)
                .build()
                .unwrap(),
        }
    }

    pub fn generate_key(&self, key: &mut [u32]) {
        let key = Span::new(key.as_mut_ptr(), self.total_count);
        self.thread_pool.broadcast(|ctx| {
            let thread_vars = calc_thread_vars(ctx.index(), self.thread_count, self.total_count);
            for (i, key) in key.range(thread_vars.offset, thread_vars.count)[0..thread_vars.count]
                .iter_mut()
                .enumerate()
            {
                *key = (thread_vars.offset + i) as u32;
            }
        });
    }

    pub fn sort_on_key<T: Copy + Sized + Sync + Send>(
        &self,
        key: &[u32],
        entries_in: &[T],
        entries_out: &mut [T],
    ) {
        let entries_out = Span::new(entries_out.as_mut_ptr(), self.total_count);
        self.thread_pool.broadcast(|ctx| {
            let thread_vars = calc_thread_vars(ctx.index(), self.thread_count, self.total_count);
            for (out, key_index) in entries_out.range(thread_vars.offset, thread_vars.count)
                [0..thread_vars.count]
                .iter_mut()
                .zip(key[thread_vars.offset..thread_vars.end].iter())
            {
                *out = entries_in[*key_index as usize];
            }
        });
    }

    fn calc_counts(&mut self, input: &mut [u64], shift: u8) {
        let counts = Span::new(self.counts.as_mut_ptr(), self.counts.len());
        let trailing_entries = self.total_count - (self.entries_per_thread * self.thread_count);
        let last = self.thread_count - 1;
        let input = Arc::new(input);
        self.thread_pool.broadcast(|ctx| {
            let length =
                self.entries_per_thread + (trailing_entries * (ctx.index() == last) as usize);
            let offset = ctx.index() * self.entries_per_thread;
            let counts = &mut counts.range(ctx.index() * RADIX, RADIX)[0..RADIX];
            counts.copy_from_slice(ZEROS.as_slice());
            for value in &input[offset..offset + length] {
                counts[((value >> shift) & 0xFF) as usize] += 1;
            }
        });
    }

    fn calc_prefix_sums(&mut self) {
        let t_offset = (self.thread_count - 1) * RADIX;
        self.prefix_sums[t_offset..t_offset + RADIX].copy_from_slice(&self.counts[0..RADIX]);
        for i in 1..self.thread_count {
            for (prefix, t_count) in self.prefix_sums[t_offset..t_offset + RADIX]
                .iter_mut()
                .zip(self.counts[i * RADIX..i * RADIX + RADIX].iter())
            {
                *prefix += *t_count;
            }
        }
        for j in 1..RADIX {
            self.prefix_sums[t_offset + j] += self.prefix_sums[t_offset + j - 1];
        }
        let mut cur;
        let mut prev = t_offset;
        for i in 1..self.thread_count {
            cur = t_offset - RADIX * i;
            for j in 0..RADIX {
                self.prefix_sums[cur + j] = self.prefix_sums[prev + j] - self.counts[prev + j];
            }
            prev = cur;
        }
    }

    fn write_outut(&mut self, shift: u8, input: &mut [u64], output: &mut [u64]) {
        let input = Arc::new(input);
        let output = Span::new(output.as_mut_ptr(), output.len());
        let prefix_sums = Span::new(self.prefix_sums.as_mut_ptr(), self.prefix_sums.len());
        let trailing_entries = self.total_count - (self.entries_per_thread * self.thread_count);
        let last = self.thread_count - 1;
        self.thread_pool.broadcast(|ctx| {
            let length =
                self.entries_per_thread + (trailing_entries * (ctx.index() == last) as usize);
            let offset = ctx.index() * self.entries_per_thread;
            let mut output = output;
            let mut prefix_sums = prefix_sums.slice(ctx.index() * RADIX);
            for value in input[offset..offset + length].iter().rev() {
                let p_sum = &mut prefix_sums[((value >> shift) & 0xFF) as usize];
                *p_sum -= 1;
                output[*p_sum] = *value;
            }
        });
    }

    fn write_outut_keyed(
        &mut self,
        shift: u8,
        input: &[u64],
        output: &mut [u64],
        key_input: &[u32],
        key_output: &mut [u32],
    ) {
        let output = Span::new(output.as_mut_ptr(), output.len());
        let key_output = Span::new(key_output.as_mut_ptr(), key_output.len());
        let prefix_sums = Span::new(self.prefix_sums.as_mut_ptr(), self.prefix_sums.len());
        let trailing_entries = self.total_count - (self.entries_per_thread * self.thread_count);
        let last = self.thread_count - 1;
        let entries_per_thread = self.entries_per_thread;
        self.thread_pool.broadcast(|ctx| {
            let length = entries_per_thread + (trailing_entries * (ctx.index() == last) as usize);
            let offset = ctx.index() * entries_per_thread;
            let mut output = output;
            let mut key_output = key_output;
            let mut prefix_sums = prefix_sums.slice(ctx.index() * RADIX);
            for (value, key) in input[offset..offset + length]
                .iter()
                .zip(key_input[offset..offset + length].iter())
                .rev()
            {
                let p_sum = &mut prefix_sums[((value >> shift) & 0xFF) as usize];
                *p_sum -= 1;
                output[*p_sum] = *value;
                key_output[*p_sum] = *key;
            }
        });
    }

    pub fn sort(&mut self, max_iter: usize, input: &mut Vec<u64>, output: &mut Vec<u64>) {
        let iterations = if max_iter > 0 {
            max_iter
        } else {
            size_of::<u64>()
        };
        let mut shift = 0;
        for _ in 0..iterations {
            self.calc_counts(input, shift);
            self.calc_prefix_sums();
            self.write_outut(shift, input, output);
            swap(input, output);
            shift += SHIFT_BASE;
        }
        if max_iter % 2 == 0 {
            output.copy_from_slice(input)
        }
    }

    pub fn sort_keyed<'a>(
        &mut self,
        max_iter: usize,
        mut input: &'a mut [u64],
        mut output: &'a mut [u64],
        mut key_input: &'a mut [u32],
        mut key_output: &'a mut [u32],
    ) {
        let iterations = if max_iter > 0 {
            max_iter
        } else {
            size_of::<u64>()
        };
        let mut shift = 0;
        for _ in 0..iterations {
            self.calc_counts(input, shift);
            self.calc_prefix_sums();
            self.write_outut_keyed(shift, input, output, key_input, key_output);
            swap(&mut input, &mut output);
            swap(&mut key_input, &mut key_output);
            shift += SHIFT_BASE;
        }
        if max_iter % 2 == 0 {
            output.copy_from_slice(input)
        }
    }
}

#[test]
pub fn sort_test() -> Result<(), std::io::Error> {
    use rand::prelude::*;
    use rayon::prelude::*;
    use std::thread::available_parallelism;
    use std::time::Instant;
    let count = 10_000_000;
    let mut sorter = RadixSorter::new(available_parallelism().map(|u| u.get()).unwrap_or(8), count);
    let mut times = vec![];
    let mut input = vec![0u64; count];
    let mut output = vec![0u64; count];
    let mut key_input = vec![0u32; count];
    let mut key_output = vec![0u32; count];
    let test_count = 1000;
    let mut start;
    for i in 0..test_count {
        println!("Generating Random Data");
        input.par_iter_mut().for_each(|v| {
            let mut rng = thread_rng();
            *v = rng.gen::<u64>();
        });
        println!("Starting Sort {i}");
        start = Instant::now();
        sorter.sort_keyed(8, &mut input, &mut output, &mut key_input, &mut key_output);
        let elapsed = start.elapsed().as_millis();
        println!("Sort {i} took {elapsed} millis");
        times.push(elapsed);
        //Verify Sorted
        for i in 1..count {
            if input[i] < input[i - 1] {
                let err = format!("Error Not Sorted of {} > {}", input[i - 1], input[i]);
                println!("{}", &err);
                return Err(std::io::Error::new(std::io::ErrorKind::Other, err));
            }
        }
    }
    println!(
        "Avg Sort of took {} millis",
        times.iter().sum::<u128>() / test_count
    );
    Ok(())
}
