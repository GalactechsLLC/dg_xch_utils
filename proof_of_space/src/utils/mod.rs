use num_traits::One;
use std::io::Error;
use std::mem::size_of;
use std::ops::{Add, AddAssign, Div, Mul, Sub};
use std::path::Path;

pub mod bit_reader;
pub mod radix_sort;
pub mod span;

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
use libc::O_DIRECT;
#[cfg(target_os = "dragonfly")]
use libc::O_FSYNC;
#[cfg(not(any(target_os = "dragonfly", target_os = "windows")))]
use libc::O_SYNC;
#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "windows")]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(target_os = "windows")]
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH};
pub async fn open_read_only_async(filename: &Path) -> Result<tokio::fs::File, Error> {
    #[cfg(target_os = "dragonfly")]
    {
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECT & O_FSYNC)
            .open(filename)
            .await
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
    {
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECT & O_SYNC)
            .open(filename)
            .await
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "openbsd"
    ))]
    {
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_SYNC)
            .open(filename)
            .await
    }
    #[cfg(target_os = "windows")]
    {
        tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING & FILE_FLAG_WRITE_THROUGH)
            .open(filename)
            .await
    }
}
pub fn open_read_only(filename: &Path) -> Result<std::fs::File, Error> {
    #[cfg(target_os = "dragonfly")]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECT & O_FSYNC)
            .open(filename)
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECT & O_SYNC)
            .open(filename)
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "openbsd"
    ))]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_SYNC)
            .open(filename)
    }
    #[cfg(target_os = "windows")]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING & FILE_FLAG_WRITE_THROUGH)
            .open(filename)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ThreadVars<
    T: Div<Output = T>
        + Mul<Output = T>
        + Eq
        + PartialEq
        + Sub<Output = T>
        + Add<Output = T>
        + AddAssign
        + Copy
        + Clone
        + One,
> {
    pub count: T,
    pub offset: T,
    pub end: T,
}

pub fn calc_thread_vars<
    T: Div<Output = T>
        + Mul<Output = T>
        + Eq
        + PartialEq
        + Sub<Output = T>
        + Add<Output = T>
        + AddAssign
        + Copy
        + Clone
        + One
        + From<bool>,
>(
    index: T,
    thread_count: T,
    total_count: T,
) -> ThreadVars<T> {
    let mut count = total_count / thread_count;
    let offset = index * count;
    count += (total_count - count * thread_count) * T::from(thread_count - T::one() == index);
    ThreadVars {
        count,
        offset,
        end: offset + count,
    }
}

pub fn bytes_to_u64<T: AsRef<[u8]>>(bytes: T) -> u64 {
    let bytes = bytes.as_ref();
    let mut buf: [u8; size_of::<u64>()] = [0; size_of::<u64>()];
    let length = (bytes.len() < size_of::<u64>()) as usize * bytes.len()
        + (bytes.len() >= size_of::<u64>()) as usize * size_of::<u64>();
    buf[0..length].copy_from_slice(&bytes[0..length]);
    u64::from_be_bytes(buf)
}

// 'bytes' points to a big-endian 64 bit value (possibly truncated, if
// (start_bit % 8 + num_bits > 64)). Returns the integer that starts at
// 'start_bit' that is 'num_bits' long (as a native-endian integer).
//
// Note: requires that 8 bytes after the first sliced byte are addressable
// (regardless of 'num_bits'). In practice it can be ensured by allocating
// extra 7 bytes to all memory buffers passed to this function.
pub fn slice_u64from_bytes<T: AsRef<[u8]>>(bytes: T, start_bit: u32, num_bits: u32) -> u64 {
    let mut bytes = bytes.as_ref().to_vec();
    let mut start_bit = start_bit;
    if start_bit + num_bits > 64 {
        bytes.push((start_bit / 8) as u8);
        start_bit %= 8;
    }
    let mut tmp = bytes_to_u64(&bytes);
    tmp <<= start_bit;
    tmp >>= 64 - num_bits;
    tmp
}

pub fn slice_u64from_bytes_full<T: AsRef<[u8]>>(bytes: T, start_bit: u32, num_bits: u32) -> u64 {
    let last_bit = start_bit + num_bits;
    let mut r = slice_u64from_bytes(bytes.as_ref(), start_bit, num_bits);
    if start_bit % 8 + num_bits > 64 {
        r |= bytes.as_ref()[(last_bit / 8) as usize] as u64 >> (8 - last_bit % 8);
    }
    r
}

pub fn slice_u128from_bytes<T: AsRef<[u8]>>(bytes: T, start_bit: u32, num_bits: u32) -> u128 {
    if num_bits <= 64 {
        slice_u64from_bytes_full(bytes, start_bit, num_bits) as u128
    } else {
        let num_bits_high = num_bits - 64;
        let high = slice_u64from_bytes_full(bytes.as_ref(), start_bit, num_bits_high);
        let low = slice_u64from_bytes_full(bytes.as_ref(), start_bit + num_bits_high, 64);
        ((high as u128) << 64) | low as u128
    }
}
