use bytes::Buf;
use log::warn;
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::io::{Cursor, Error, ErrorKind, Read};
use std::str::FromStr;

pub fn hash_256(input: impl AsRef<[u8]>) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hasher.finalize().to_vec()
}

#[derive(Default, Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub enum ChiaProtocolVersion {
    Chia0_0_34 = 34, //Pre 2.0.0
    #[default]
    Chia0_0_35 = 35, //2.0.0
    Chia0_0_36 = 36, //2.2.0
}
impl Display for ChiaProtocolVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ChiaProtocolVersion::Chia0_0_34 => f.write_str("0.0.34"),
            ChiaProtocolVersion::Chia0_0_35 => f.write_str("0.0.35"),
            ChiaProtocolVersion::Chia0_0_36 => f.write_str("0.0.36"),
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
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized;
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized;
}

impl ChiaSerialize for String {
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend((self.len() as u32).to_be_bytes());
        bytes.extend(self.as_bytes());
        bytes
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
            warn!("Serializing Large Vec: {vec_len}")
        }
        let mut buf = vec![0u8; vec_len];
        bytes.read_exact(&mut buf[0..vec_len])?;
        String::from_utf8(buf).map_err(|e| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Failed to parse Utf-8 String from Bytes: {:?}", e),
            )
        })
    }
}

impl ChiaSerialize for bool {
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        vec![*self as u8]
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
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        match &self {
            Some(t) => {
                bytes.push(1u8);
                bytes.extend(t.to_bytes(version));
            }
            None => {
                bytes.push(0u8);
            }
        }
        bytes
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
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.0.to_bytes(version));
        bytes.extend(self.1.to_bytes(version));
        bytes
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
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.0.to_bytes(version));
        bytes.extend(self.1.to_bytes(version));
        bytes.extend(self.2.to_bytes(version));
        bytes
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
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8>
    where
        Self: Sized,
    {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend((self.len() as u32).to_be_bytes());
        for e in self {
            bytes.extend(e.to_bytes(version));
        }
        bytes
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
            warn!("Serializing Large Vec: {vec_len}")
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
                fn to_bytes(&self, _version: ChiaProtocolVersion) -> Vec<u8> {
                    self.to_be_bytes().to_vec()
                }
                fn from_bytes<T: AsRef<[u8]>>(bytes: &mut Cursor<T>, _version: ChiaProtocolVersion) -> Result<Self, std::io::Error> where Self: Sized,
                {
                    if bytes.remaining() < $size {
                        Err(Error::new(std::io::ErrorKind::InvalidInput, format!("Failed to Parse $name, expected length $size, found {}",  bytes.remaining())))
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
