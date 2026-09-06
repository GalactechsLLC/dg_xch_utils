use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};
use log::{trace, warn};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::io;
use std::io::{Cursor, Error, ErrorKind, Read, Write};
use std::str::FromStr;
use time::{OffsetDateTime, PrimitiveDateTime};

#[derive(
    Default,
    Debug,
    Copy,
    Clone,
    Ord,
    PartialOrd,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum ChiaProtocolVersion {
    Chia0_0_34 = 34, //Pre 2.0.0
    Chia0_0_35 = 35, //2.0.0
    Chia0_0_36 = 36, //2.2.0
    #[default]
    Chia0_0_37 = 37, //2.5.5
}
impl Display for ChiaProtocolVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ChiaProtocolVersion::Chia0_0_34 => f.write_str("0.0.34"),
            ChiaProtocolVersion::Chia0_0_35 => f.write_str("0.0.35"),
            ChiaProtocolVersion::Chia0_0_36 => f.write_str("0.0.36"),
            ChiaProtocolVersion::Chia0_0_37 => f.write_str("0.0.37"),
        }
    }
}
impl FromStr for ChiaProtocolVersion {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "0.0.34" => ChiaProtocolVersion::Chia0_0_34,
            "0.0.35" => ChiaProtocolVersion::Chia0_0_35,
            "0.0.36" => ChiaProtocolVersion::Chia0_0_36,
            "0.0.37" => ChiaProtocolVersion::Chia0_0_37,
            _ => {
                warn!(
                    "Failed to detect Protocol Version: {s}, defaulting to {}",
                    ChiaProtocolVersion::default()
                );
                ChiaProtocolVersion::default()
            }
        })
    }
}

pub trait ChiaSerialize {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized;
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error>
    where
        Self: Sized,
    {
        bytes.extend(self.to_bytes(version)?);
        Ok(())
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized;
    /// Decodes a value, requiring the entire buffer to be consumed.
    fn from_bytes_exact(bytes: &[u8], version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut cursor = Cursor::new(bytes);
        let value = Self::from_bytes(&mut cursor, version)?;
        let consumed = cursor.position();
        if consumed != bytes.len() as u64 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("{} bytes not consumed", bytes.len() as u64 - consumed),
            ));
        }
        Ok(value)
    }
}
impl ChiaSerialize for OffsetDateTime {
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error> {
        (self.unix_timestamp() as u64).append_bytes(bytes, version)
    }
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        (self.unix_timestamp() as u64).to_bytes(version)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let timestamp: u64 = u64::from_bytes(bytes, version)?;
        OffsetDateTime::from_unix_timestamp(timestamp as i64)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))
    }
}

impl ChiaSerialize for String {
    fn append_bytes(
        &self,
        bytes: &mut Vec<u8>,
        _version: ChiaProtocolVersion,
    ) -> Result<(), Error> {
        bytes.extend_from_slice(&(self.len() as u32).to_be_bytes());
        bytes.extend_from_slice(self.as_bytes());
        Ok(())
    }
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend((self.len() as u32).to_be_bytes());
        bytes.extend(self.as_bytes());
        Ok(bytes)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_len_ary: [u8; 4] = [0; 4];
        bytes.read_exact(&mut u32_len_ary)?;
        let vec_len = u32::from_be_bytes(u32_len_ary) as usize;
        if vec_len > 2048 {
            warn!("decoding large vec (len={vec_len})");
        }
        // Check the declared length against remaining bytes before allocating.
        let remaining = bytes
            .get_ref()
            .as_ref()
            .len()
            .saturating_sub(bytes.position() as usize);
        if remaining < vec_len {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("String length {vec_len} exceeds {remaining} remaining bytes"),
            ));
        }
        let mut buf = vec![0u8; vec_len];
        bytes.read_exact(&mut buf[0..vec_len])?;
        String::from_utf8(buf).map_err(|e| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Failed to parse Utf-8 String from Bytes: {e:?}"),
            )
        })
    }
}

impl ChiaSerialize for bool {
    fn append_bytes(
        &self,
        bytes: &mut Vec<u8>,
        _version: ChiaProtocolVersion,
    ) -> Result<(), Error> {
        bytes.push(u8::from(*self));
        Ok(())
    }
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        Ok(vec![u8::from(*self)])
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut bool_buf: [u8; 1] = [0; 1];
        bytes.read_exact(&mut bool_buf)?;
        match bool_buf[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Failed to parse bool, invalid value: {:?}", bool_buf[0]),
            )),
        }
    }
}

impl<T> ChiaSerialize for Option<T>
where
    T: ChiaSerialize,
{
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error> {
        match self {
            Some(value) => {
                bytes.push(1);
                value.append_bytes(bytes, version)?;
            }
            None => bytes.push(0),
        }
        Ok(())
    }
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        match &self {
            Some(t) => {
                bytes.push(1u8);
                bytes.extend(t.to_bytes(version)?);
            }
            None => {
                bytes.push(0u8);
            }
        }
        Ok(bytes)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut bool_buf: [u8; 1] = [0; 1];
        bytes.read_exact(&mut bool_buf)?;
        match bool_buf[0] {
            0 => Ok(None),
            1 => Ok(Some(T::from_bytes(bytes, version)?)),
            other => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Optional must be 0 or 1, found: {other}"),
            )),
        }
    }
}

impl<T, U> ChiaSerialize for (T, U)
where
    T: ChiaSerialize,
    U: ChiaSerialize,
{
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error> {
        self.0.append_bytes(bytes, version)?;
        self.1.append_bytes(bytes, version)
    }
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.0.to_bytes(version)?);
        bytes.extend(self.1.to_bytes(version)?);
        Ok(bytes)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let t = T::from_bytes(bytes, version)?;
        let u = U::from_bytes(bytes, version)?;
        Ok((t, u))
    }
}

impl<T, U, V> ChiaSerialize for (T, U, V)
where
    T: ChiaSerialize,
    U: ChiaSerialize,
    V: ChiaSerialize,
{
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error> {
        self.0.append_bytes(bytes, version)?;
        self.1.append_bytes(bytes, version)?;
        self.2.append_bytes(bytes, version)
    }
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.0.to_bytes(version)?);
        bytes.extend(self.1.to_bytes(version)?);
        bytes.extend(self.2.to_bytes(version)?);
        Ok(bytes)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let t = T::from_bytes(bytes, version)?;
        let u = U::from_bytes(bytes, version)?;
        let v = V::from_bytes(bytes, version)?;
        Ok((t, u, v))
    }
}

impl<T> ChiaSerialize for Vec<T>
where
    T: ChiaSerialize,
{
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error> {
        bytes.extend_from_slice(&(self.len() as u32).to_be_bytes());
        for value in self {
            value.append_bytes(bytes, version)?;
        }
        Ok(())
    }
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend((self.len() as u32).to_be_bytes());
        for e in self {
            bytes.extend(e.to_bytes(version)?);
        }
        Ok(bytes)
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_buf: [u8; 4] = [0; 4];
        bytes.read_exact(&mut u32_buf)?;
        let buf: Vec<T> = Vec::new();
        let vec_len = u32::from_be_bytes(u32_buf);
        if vec_len > 2048 {
            trace!("decoding large vec (len={vec_len})");
        }
        (0..vec_len).try_fold(buf, |mut vec, _| {
            vec.push(T::from_bytes(bytes, version)?);
            Ok(vec)
        })
    }
}

/// Decode a length-prefixed list, parsing at most `max_items` elements and skipping the rest.
/// A request whose list claims far more items than the handler will ever use (a
/// `RequestCoinState` claiming 1.2M coin_ids costs seconds of parse CPU on a small node) is
/// truncated DURING decode:
///   - the first `min(count, max_items)` elements are parsed normally (head kept, order kept);
///   - when `element_fixed_size` is `Some(n)` the remaining `count - max_items` elements are
///     skipped in O(1) by advancing the cursor `remaining * n` bytes — never allocated, never
///     parsed;
///   - a variable-size element type still parses the tail element-by-element, since the O(1)
///     skip is only sound for fixed-size elements.
///
/// A skip past the buffer end is tolerated: the outer full-consumption check then reads an empty
/// remainder, so a message whose CLAIMED count overstates the bytes actually present is accepted
/// with the truncated head once the claim is past the limit. The cursor is clamped to the buffer
/// end so [`ChiaSerialize::from_bytes_exact`]'s consumed check cannot underflow; trailing garbage
/// after a skip that lands INSIDE the buffer is still rejected there.
///
/// The memory half of the same bound: this decoder, like `Vec<T>::from_bytes`, never
/// pre-allocates from the untrusted count — it starts empty and grows per parsed element.
pub fn parse_vec_limited<T: ChiaSerialize>(
    bytes: &mut Cursor<&[u8]>,
    version: ChiaProtocolVersion,
    max_items: u32,
    element_fixed_size: Option<u64>,
) -> Result<Vec<T>, Error> {
    let mut u32_buf: [u8; 4] = [0; 4];
    bytes.read_exact(&mut u32_buf)?;
    let claimed = u32::from_be_bytes(u32_buf);
    let to_parse = claimed.min(max_items);
    let mut out: Vec<T> = Vec::new();
    for _ in 0..to_parse {
        out.push(T::from_bytes(bytes, version)?);
    }
    let remaining = u64::from(claimed - to_parse);
    if remaining > 0 {
        if let Some(size) = element_fixed_size {
            let end = bytes.get_ref().as_ref().len() as u64;
            let target = bytes
                .position()
                .saturating_add(remaining.saturating_mul(size));
            bytes.set_position(target.min(end));
        } else {
            for _ in 0..remaining {
                let _ = T::from_bytes(bytes, version)?;
            }
        }
    }
    Ok(out)
}

macro_rules! impl_primitives {
    ($($name: ident, $size:expr);*) => {
        $(
            impl ChiaSerialize for $name {
                fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error> {
                    Ok(self.to_be_bytes().to_vec())
                }
                fn append_bytes(&self, bytes: &mut Vec<u8>, _version: ChiaProtocolVersion) -> Result<(), Error> {
                    bytes.extend_from_slice(&self.to_be_bytes());
                    Ok(())
                }
                fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, std::io::Error> where Self: Sized,
                {
                    let remaining = bytes.get_ref().as_ref().len().saturating_sub(bytes.position() as usize);
                    if remaining < $size {
                        Err(Error::new(std::io::ErrorKind::InvalidInput, format!("Failed to Parse {}, expected length {}, found {}", stringify!($name), stringify!($size), remaining)))
                    } else {
                        let mut buffer: [u8; $size] = [0; $size];
                        bytes.read_exact(&mut buffer)?;
                        Ok($name::from_be_bytes(buffer))
                    }
                }
            }
        )*
    };
    ()=>{};
}
impl_primitives!(
    i8, 1;
    i16, 2;
    i32, 4;
    i64, 8;
    i128, 16;
    u8, 1;
    u16, 2;
    u32, 4;
    u64, 8;
    u128, 16;
    f32, 4;
    f64, 8
);

macro_rules! impl_arrays {
    ($($name: ty, $size:expr);*) => {
        $(
            impl ChiaSerialize for $name {
                fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error> {
                    Ok(self.to_vec())
                }
                fn append_bytes(&self, bytes: &mut Vec<u8>, _version: ChiaProtocolVersion) -> Result<(), Error> {
                    bytes.extend_from_slice(self);
                    Ok(())
                }
                fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, std::io::Error> where Self: Sized,
                {
                    let remaining = bytes.get_ref().as_ref().len().saturating_sub(bytes.position() as usize);
                    if remaining < $size {
                        Err(Error::new(std::io::ErrorKind::InvalidInput, format!("Failed to Parse {}, expected length {}, found {}", stringify!($name), stringify!($size), remaining)))
                    } else {
                        let mut buffer: $name = [0; $size];
                        bytes.read_exact(&mut buffer)?;
                        Ok(buffer)
                    }
                }
            }
        )*
    };
    ()=>{};
}
impl_arrays!(
    [u8; 4], 4;
    [u8; 8], 8;
    [u8; 16], 16;
    [u8; 24], 24;
    [u8; 32], 32;
    [u8; 48], 48;
    [u8; 64], 64;
    [u8; 96], 96;
    [u8; 128], 128;
    [u8; 256], 256;
    [u8; 512], 512
);

macro_rules! impl_nz_primitives {
    ($($nz:ty, $base:ty);* $(;)?) => {
        $(
            impl ChiaSerialize for $nz {
                fn append_bytes(&self, bytes: &mut Vec<u8>, _version: ChiaProtocolVersion) -> Result<(), io::Error> {
                    bytes.extend_from_slice(&self.get().to_be_bytes());
                    Ok(())
                }
                #[inline]
                fn to_bytes(&self, _v: ChiaProtocolVersion) -> Result<Vec<u8>, io::Error> {
                    Ok(self.get().to_be_bytes().to_vec())
                }

                #[inline]
                fn from_bytes(cur: &mut Cursor<&[u8]>, _v: ChiaProtocolVersion) -> Result<Self, io::Error> {
                    let mut buf = [0u8; core::mem::size_of::<$base>()];
                    cur.read_exact(&mut buf)?;
                    let v = <$base>::from_be_bytes(buf);
                    <$nz>::new(v).ok_or_else(|| io::Error::new(
                        io::ErrorKind::InvalidData,
                        concat!(stringify!($nz), " cannot be zero"),
                    ))
                }
            }
        )*
    };
}

impl_nz_primitives!(
    NonZeroU8,    u8;
    NonZeroU16,   u16;
    NonZeroU32,   u32;
    NonZeroU64,   u64;
    NonZeroU128,  u128;
    NonZeroI8,    i8;
    NonZeroI16,   i16;
    NonZeroI32,   i32;
    NonZeroI64,   i64;
    NonZeroI128,  i128;
    NonZeroUsize, usize;
    NonZeroIsize, isize;
);

pub const MAX_DECODE_SIZE: u64 = 0x0004_0000_0000;
pub const CONS_BOX_MARKER: u8 = 0xff;
pub const MAX_SINGLE_BYTE: u8 = 0x7f;

#[allow(clippy::cast_possible_truncation)]
pub fn encode_size(f: &mut dyn Write, size: u64) -> Result<(), Error> {
    if size < 0x40 {
        f.write_all(&[(0x80 | size) as u8])?;
    } else if size < 0x2000 {
        f.write_all(&[(0xc0 | (size >> 8)) as u8, ((size) & 0xff) as u8])?;
    } else if size < 0x10_0000 {
        f.write_all(&[
            (0xe0 | (size >> 16)) as u8,
            ((size >> 8) & 0xff) as u8,
            ((size) & 0xff) as u8,
        ])?;
    } else if size < 0x800_0000 {
        f.write_all(&[
            (0xf0 | (size >> 24)) as u8,
            ((size >> 16) & 0xff) as u8,
            ((size >> 8) & 0xff) as u8,
            ((size) & 0xff) as u8,
        ])?;
    } else if size < 0x4_0000_0000 {
        f.write_all(&[
            (0xf8 | (size >> 32)) as u8,
            ((size >> 24) & 0xff) as u8,
            ((size >> 16) & 0xff) as u8,
            ((size >> 8) & 0xff) as u8,
            ((size) & 0xff) as u8,
        ])?;
    } else {
        return Err(Error::new(ErrorKind::InvalidData, "atom too big"));
    }
    Ok(())
}

pub fn decode_size(stream: &mut dyn Read, initial_b: u8) -> Result<u64, Error> {
    if initial_b & 0x80 == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
    }
    let mut bit_count = 0;
    let mut bit_mask: u8 = 0x80;
    let mut b = initial_b;
    while b & bit_mask != 0 {
        bit_count += 1;
        b &= 0xff ^ bit_mask;
        bit_mask >>= 1;
    }
    let mut size_blob: Vec<u8> = vec![0; bit_count];
    size_blob[0] = b;
    if bit_count > 1 {
        stream.read_exact(&mut size_blob[1..])?;
    }
    let mut v = 0;
    if size_blob.len() > 6 {
        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
    }
    for b in &size_blob {
        v <<= 8;
        v += u64::from(*b);
    }
    if v >= MAX_DECODE_SIZE {
        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
    }
    Ok(v)
}

impl<K: ChiaSerialize + Eq + Hash, V: ChiaSerialize> ChiaSerialize for HashMap<K, V> {
    fn append_bytes(&self, bytes: &mut Vec<u8>, version: ChiaProtocolVersion) -> Result<(), Error> {
        bytes.extend_from_slice(&(self.len() as u32).to_be_bytes());
        for (key, value) in self {
            key.append_bytes(bytes, version)?;
            value.append_bytes(bytes, version)?;
        }
        Ok(())
    }
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        #[allow(clippy::cast_possible_truncation)]
        bytes.extend((self.len() as u32).to_be_bytes());
        for (k, v) in self {
            bytes.extend(k.to_bytes(version)?);
            bytes.extend(v.to_bytes(version)?);
        }
        Ok(bytes)
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_buf: [u8; 4] = [0; 4];
        bytes.read_exact(&mut u32_buf)?;
        let map_len = u32::from_be_bytes(u32_buf);
        if map_len > 2048 {
            warn!("Serializing Large Map: {map_len}");
        }
        // Never pre-allocate from an untrusted length prefix.
        let buf: HashMap<K, V> = HashMap::new();
        (0..map_len).try_fold(buf, |mut map, _| {
            let key = K::from_bytes(bytes, version)?;
            let value = V::from_bytes(bytes, version)?;
            map.insert(key, value);
            Ok(map)
        })
    }
}

impl ChiaSerialize for PrimitiveDateTime {
    fn append_bytes(
        &self,
        bytes: &mut Vec<u8>,
        _version: ChiaProtocolVersion,
    ) -> Result<(), Error> {
        self.assume_utc()
            .append_bytes(bytes, ChiaProtocolVersion::default())
    }
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        self.assume_utc().to_bytes(ChiaProtocolVersion::default())
    }
    fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let offset_datatime = OffsetDateTime::from_bytes(bytes, ChiaProtocolVersion::default())?;
        Ok(PrimitiveDateTime::new(
            offset_datatime.date(),
            offset_datatime.time(),
        ))
    }
}
