use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use crate::util::extract_num;

pub enum BucketStorage {
    Disk(PathBuf, PathBuf),
    Memory,
}
pub enum Bucket {
    File(Mutex<File>),
    Memory(Mutex<Vec<u8>>),
}

pub struct BucketSorter {
    storage: BucketStorage,
    final_position_start: AtomicUsize,
    final_position_end: AtomicUsize,
    prev_bucket_position_start: AtomicUsize,
    next_bucket_to_sort: AtomicUsize,
    num_buckets: u32,
    log_num_buckets: u32,
    entry_size: u32,
    begin_bits: u32,
    stripe_size: u32,
    buckets: Vec<Bucket>,
    prev_bucket_buf: Mutex<Vec<u8>>,
    sort_buffer: Mutex<Vec<u8>>,
}
impl BucketSorter {
    pub fn new(
        storage: BucketStorage,
        num_buckets: u32,
        log_num_buckets: u32,
        entry_size: u32,
        begin_bits: u32,
        stripe_size: u32,
    ) -> Self {
        let mut buckets = Vec::with_capacity(num_buckets as usize);
        for i in 0..num_buckets {
            match &storage {
                BucketStorage::Disk(tmp_dir, filename) => {
                    let filename = format!("{}/{}.bucket_{i:0>3}.tmp", tmp_dir.display(), filename.display());
                    buckets.push(Bucket::File(Mutex::new(File::create(filename))));
                }
                BucketStorage::Memory => {
                    buckets.push(Bucket::Memory(Mutex::default()));
                }
            }
        }
        BucketSorter {
            storage,
            final_position_start: AtomicUsize::default(),
            final_position_end: AtomicUsize::default(),
            prev_bucket_position_start: AtomicUsize::default(),
            num_buckets,
            log_num_buckets,
            entry_size,
            begin_bits,
            stripe_size,
            buckets,
            prev_bucket_buf: Mutex::default(),
            sort_buffer: Mutex::default(),
        }
    }
    pub async fn insert(&self, value: &[u8]) -> Result<(), Error> {
        let bucket_index = extract_num(value, self.entry_size, self.begin_bits, self.log_num_buckets);
        match &self.buckets[bucket_index as usize] {
            Bucket::File(file) => {
                file.lock().await.write_all(value[0..self.entry_size]).await?;
            }
            Bucket::Memory(buffer) => {
                buffer.lock().await.extend(value[0..self.entry_size]);
            }
        }
        Ok(())
    }

    pub async fn trigger_new(&self, position: usize) -> Result<(), Error> {
        if !(position <= self.final_position_end.load(Ordering::Relaxed)) {
            return Err(Error::new(ErrorKind::InvalidInput, "Triggering bucket too late"));
        }
        if !(position >= self.final_position_start.load(Ordering::Relaxed)) {
            return Err(Error::new(ErrorKind::InvalidInput, "Triggering bucket too early"));
        }
        let sort_lock = self.sort_buffer.lock().await;
        if !sort_lock.is_empty() {
            // save some of the current bucket, to allow some reverse-tracking
            // in the reading pattern,
            // position is the first position that we need in the new array
            let cache_size = self.final_position_end.load(Ordering::Relaxed) - position;
            let mut prev_lock = self.prev_bucket_buf.lock().await;
            prev_lock.clear();
            prev_lock.fill(0);
            prev_lock[0..cache_size].copy_from_slice(&sort_lock[position..cache_size]);
        }
        self.sort_bucket().await?;
        self.prev_bucket_position_start.store(position, Ordering::Relaxed);
        Ok(())
    }

    pub async fn read_entry<'a>(&'a self, position: usize) -> Result<&'a [u8], Error> {
        if position < self.final_position_start.load(Ordering::Relaxed) {
            return if !(position >= self.prev_bucket_position_start.load(Ordering::Relaxed)) {
                Err(Error::new(ErrorKind::InvalidInput, "Invalid prev bucket start"))
            } else {
                Ok(&self.prev_bucket_buf.lock().await[(position - self.prev_bucket_position_start.load(Ordering::Relaxed))..])
            };
        }
        while position >= self.final_position_end.load(Ordering::Relaxed) {
            self.sort_bucket().await?;
        }
        if !(self.final_position_end.load(Ordering::Relaxed) > position) {
            Err(Error::new(ErrorKind::InvalidInput, "Position too large"))
        } else if !(self.final_position_start.load(Ordering::Relaxed) <= position) {
            Err(Error::new(ErrorKind::InvalidInput, "Position too small"))
        } else {
            Ok(&self.sort_buffer.lock().await[(position - self.final_position_start.load(Ordering::Relaxed))..])
        }
    }

    pub async fn flush(&self) -> Result<(), Error> {
        for bucket in self.buckets {
            match bucket {
                Bucket::File(file) => {
                    file.lock().await.flush().await?;
                }
                Bucket::Memory(_) => {}
            }
        }
        self.final_position_end.store(0, Ordering::Relaxed);
        self.sort_buffer.lock().await.clear();
        Ok(())
    }

    pub async fn sort_bucket(&self) -> Result<(), Error> {
        todo!()
    }

    pub async fn close_to_new_bucket(&self, position: usize) -> bool {
        let final_position_end = self.final_position_end.load(Ordering::Relaxed);
        let next_bucket_to_sort = self.next_bucket_to_sort.load(Ordering::Relaxed);
        if !(position <= final_position_end) {
            return next_bucket_to_sort < self.buckets.len();
        }
        position + self.prev_bucket_buf_size / 2 >= final_position_end
            && next_bucket_to_sort < self.buckets.len()
    }
}