use log::warn;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::io::{Cursor, Error, ErrorKind, Read, Write};
use std::str::FromStr;
use time::OffsetDateTime;

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
    #[default]
    Chia0_0_36 = 36, //2.2.0
    Chia0_0_37 = 37, //2.2.0
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
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized;
}
impl ChiaSerialize for OffsetDateTime {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        (self.unix_timestamp() as u64).to_bytes(version)
    }
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let timestamp: u64 = u64::from_bytes(bytes, version)?;
        OffsetDateTime::from_unix_timestamp(timestamp as i64)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))
    }
}

impl ChiaSerialize for String {
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
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        _version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_len_ary: [u8; 4] = [0; 4];
        bytes.read_exact(&mut u32_len_ary)?;
        let vec_len = u32::from_be_bytes(u32_len_ary) as usize;
        if vec_len > 2048 {
            warn!("Serializing Large Vec: {vec_len}");
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
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        Ok(vec![u8::from(*self)])
    }
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        _version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
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
    fn from_bytes<B: AsRef<[u8]>>(
        bytes: &mut Cursor<B>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut bool_buf: [u8; 1] = [0; 1];
        bytes.read_exact(&mut bool_buf)?;
        if bool_buf[0] > 0 {
            Ok(Some(T::from_bytes(bytes, version)?))
        } else {
            Ok(None)
        }
    }
}

impl<T, U> ChiaSerialize for (T, U)
where
    T: ChiaSerialize,
    U: ChiaSerialize,
{
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.0.to_bytes(version)?);
        bytes.extend(self.1.to_bytes(version)?);
        Ok(bytes)
    }
    fn from_bytes<B: AsRef<[u8]>>(
        bytes: &mut Cursor<B>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
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
    fn from_bytes<B: AsRef<[u8]>>(
        bytes: &mut Cursor<B>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
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
    fn from_bytes<B: AsRef<[u8]>>(
        bytes: &mut Cursor<B>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_buf: [u8; 4] = [0; 4];
        bytes.read_exact(&mut u32_buf)?;
        let buf: Vec<T> = Vec::new();
        let vec_len = u32::from_be_bytes(u32_buf);
        if vec_len > 2048 {
            warn!("Serializing Large Vec: {vec_len}");
        }
        (0..vec_len).try_fold(buf, |mut vec, _| {
            vec.push(T::from_bytes(bytes, version)?);
            Ok(vec)
        })
    }
}

macro_rules! impl_primitives {
    ($($name: ident, $size:expr);*) => {
        $(
            impl ChiaSerialize for $name {
                fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error> {
                    Ok(self.to_be_bytes().to_vec())
                }
                fn from_bytes<T: AsRef<[u8]>>(bytes: &mut Cursor<T>, _version: ChiaProtocolVersion) -> Result<Self, std::io::Error> where Self: Sized,
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

const MAX_DECODE_SIZE: u64 = 0x0004_0000_0000;

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

    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let mut u32_buf: [u8; 4] = [0; 4];
        bytes.read_exact(&mut u32_buf)?;
        let map_len = u32::from_be_bytes(u32_buf);
        if map_len > 2048 {
            warn!("Serializing Large Map: {map_len}");
        }
        let buf: HashMap<K, V> = HashMap::with_capacity(map_len as usize);
        (0..map_len).try_fold(buf, |mut map, _| {
            let key = K::from_bytes(bytes, version)?;
            let value = V::from_bytes(bytes, version)?;
            map.insert(key, value);
            Ok(map)
        })
    }
}
