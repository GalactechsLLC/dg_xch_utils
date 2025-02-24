use crate::clvm::program::Program;
use crate::formatting::prep_hex_str;
use crate::traits::SizedBytes;
use blst::min_pk::{PublicKey, SecretKey, Signature};
use bytes::Buf;
use dg_xch_serialize::ChiaProtocolVersion;
use dg_xch_serialize::ChiaSerialize;
use hex::encode;
use num_traits::AsPrimitive;
use rand::distributions::Standard;
use rand::prelude::Distribution;
use rand::{Fill, Rng};
use secrecy::zeroize::DefaultIsZeroes;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::min;
use std::io::{Cursor, Error, ErrorKind, Read};
use std::ops::{Index, IndexMut, Range};
use std::str::FromStr;

#[derive(Copy, Clone)]
pub struct SizedBytesImpl<const SIZE: usize> {
    bytes: [u8; SIZE],
}
impl<const SIZE: usize> SizedBytes<'_, SIZE> for SizedBytesImpl<SIZE> {
    const SIZE: usize = SIZE;
    fn new(bytes: [u8; SIZE]) -> Self {
        Self { bytes }
    }
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut buf = [0u8; SIZE];
        if bytes.len() > SIZE {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Too Many Bytes Sent to parse, expected {} got {}",
                    SIZE,
                    bytes.len()
                ),
            ))
        } else {
            let offset = SIZE - bytes.len();
            for (i, v) in bytes.iter().enumerate() {
                buf[offset + i] = *v;
            }
            Ok(Self { bytes: buf })
        }
    }

    fn bytes(&self) -> [u8; SIZE] {
        self.bytes
    }
}
impl<const SIZE: usize> Fill for SizedBytesImpl<SIZE> {
    fn try_fill<R: Rng + ?Sized>(&mut self, rng: &mut R) -> Result<(), rand::Error> {
        rng.fill_bytes(&mut self.bytes);
        Ok(())
    }
}
impl<const SIZE: usize> Distribution<SizedBytesImpl<SIZE>> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> SizedBytesImpl<SIZE> {
        let mut slf = SizedBytesImpl { bytes: [0u8; SIZE] };
        rng.fill(&mut slf);
        slf
    }
}
impl<const SIZE: usize> FromStr for SizedBytesImpl<SIZE> {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.try_into()
    }
}
impl<const SIZE: usize> std::hash::Hash for SizedBytesImpl<SIZE> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}
impl<const SIZE: usize> PartialEq for SizedBytesImpl<SIZE> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl<const SIZE: usize> Eq for SizedBytesImpl<SIZE> {}
impl<const SIZE: usize> Serialize for SizedBytesImpl<SIZE> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}
impl<const SIZE: usize> Index<usize> for SizedBytesImpl<SIZE> {
    type Output = u8;
    fn index(&self, index: usize) -> &Self::Output {
        &self.bytes[index]
    }
}
impl<N: AsPrimitive<usize>, const SIZE: usize> Index<Range<N>> for SizedBytesImpl<SIZE> {
    type Output = [u8];
    fn index(&self, index: Range<N>) -> &Self::Output {
        &self.bytes[index.start.as_()..index.end.as_()]
    }
}
impl<N: AsPrimitive<usize>, const SIZE: usize> IndexMut<Range<N>> for SizedBytesImpl<SIZE> {
    fn index_mut(&mut self, index: Range<N>) -> &mut Self::Output {
        &mut self.bytes[index.start.as_()..index.end.as_()]
    }
}
impl<const SIZE: usize> AsRef<[u8]> for SizedBytesImpl<SIZE> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
impl<const SIZE: usize> AsRef<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    fn as_ref(&self) -> &[u8; SIZE] {
        &self.bytes
    }
}
impl<const SIZE: usize> From<SizedBytesImpl<SIZE>> for Vec<u8> {
    fn from(slf: SizedBytesImpl<SIZE>) -> Vec<u8> {
        slf.bytes.to_vec()
    }
}
impl<const SIZE: usize> From<[u8; SIZE]> for SizedBytesImpl<SIZE> {
    fn from(bytes: [u8; SIZE]) -> SizedBytesImpl<SIZE> {
        SizedBytesImpl { bytes }
    }
}
impl<const SIZE: usize> From<Vec<u8>> for SizedBytesImpl<SIZE> {
    fn from(vec: Vec<u8>) -> SizedBytesImpl<SIZE> {
        let mut bytes = [0; SIZE];
        bytes[0..min(SIZE, vec.len())].copy_from_slice(&vec[0..min(SIZE, vec.len())]);
        SizedBytesImpl { bytes }
    }
}
impl<const SIZE: usize> IntoIterator for SizedBytesImpl<SIZE> {
    type Item = u8;
    type IntoIter = core::array::IntoIter<u8, { SIZE }>;

    fn into_iter(self) -> Self::IntoIter {
        self.bytes.into_iter()
    }
}
impl<const SIZE: usize> TryFrom<&str> for SizedBytesImpl<SIZE> {
    type Error = Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(&hex::decode(prep_hex_str(value)).map_err(|e| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Hex string {value} is not a Valid Bytes{SIZE}: {e:?}"),
            )
        })?)
    }
}
impl<const SIZE: usize> std::fmt::Display for SizedBytesImpl<SIZE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{}", encode(self.bytes))
    }
}
impl<const SIZE: usize> Default for SizedBytesImpl<SIZE> {
    fn default() -> SizedBytesImpl<SIZE> {
        SizedBytesImpl::new([0; SIZE])
    }
}
impl<const SIZE: usize> std::fmt::Debug for SizedBytesImpl<SIZE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{}", encode(self.bytes))
    }
}
struct SizedBytesImplVisitor<const SIZE: usize>;
impl<const SIZE: usize> Visitor<'_> for SizedBytesImplVisitor<SIZE> {
    type Value = SizedBytesImpl<SIZE>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter
            .write_str(format!("Expecting a hex String, or byte array of size {}", SIZE).as_str())
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Self::Value::try_from(value).map_err(|e| serde::de::Error::custom(e.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Self::Value::try_from(value.as_str()).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}
impl<'a, const SIZE: usize> Deserialize<'a> for SizedBytesImpl<SIZE> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        match deserializer.deserialize_string(SizedBytesImplVisitor) {
            Ok(hex) => Ok(hex),
            Err(er) => Err(er),
        }
    }
}

macro_rules! impl_sized_bytes {
    ($($name: ident, $size:expr);*) => {
        $(
            pub type $name = SizedBytesImpl<$size>;
            impl DefaultIsZeroes for SizedBytesImpl<$size> {}
            impl TryFrom<Program> for $name {
                type Error = Error;
                fn try_from(value: Program) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("Program is not a valid {}",  stringify!($name))))?;
                    Self::parse(&vec)
                }
            }
            impl TryFrom<&Program> for $name {
                type Error = Error;
                fn try_from(value: &Program) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("Program is not a valid {}",  stringify!($name))))?;
                    Self::parse(&vec)
                }
            }
            #[cfg(feature = "postgres")]
            impl sqlx::Type<sqlx::Postgres> for $name {
                fn type_info() -> sqlx::postgres::PgTypeInfo {
                    sqlx::postgres::PgTypeInfo::with_name(stringify!($name))
                }
            }
            impl ChiaSerialize for $name {
                fn to_bytes(&self, _version: ChiaProtocolVersion) -> Vec<u8> {
                    self.bytes().to_vec()
                }
                fn from_bytes<T: AsRef<[u8]>>(bytes: &mut Cursor<T>, _version: ChiaProtocolVersion) -> Result<Self, Error> where Self: Sized,
                {
                    if bytes.remaining() < $size {
                        Err(Error::new(ErrorKind::InvalidInput, format!("Failed to Parse {}, expected length {}, found {}", stringify!($name),  $size, bytes.remaining())))
                    } else {
                        let mut buf = [0u8; $size];
                        bytes.read_exact(&mut buf)?;
                        Ok(buf.into())
                    }
                }
            }
        )*
    };
    ()=>{};
}

impl_sized_bytes!(
    Bytes4, 4;
    Bytes8, 8;
    Bytes32, 32;
    Bytes48, 48;
    Bytes96, 96;
    Bytes100, 100;
    Bytes480, 480
);

impl From<&Bytes32> for SecretKey {
    fn from(val: &Bytes32) -> Self {
        SecretKey::from_bytes(val.as_ref()).unwrap_or_default()
    }
}
impl From<Bytes32> for SecretKey {
    fn from(val: Bytes32) -> Self {
        SecretKey::from_bytes(val.as_ref()).unwrap_or_default()
    }
}

impl From<&SecretKey> for Bytes32 {
    fn from(val: &SecretKey) -> Self {
        Bytes32::new(val.to_bytes())
    }
}
impl From<SecretKey> for Bytes32 {
    fn from(val: SecretKey) -> Self {
        Bytes32::new(val.to_bytes())
    }
}

impl From<&Bytes48> for PublicKey {
    fn from(val: &Bytes48) -> Self {
        PublicKey::from_bytes(val.as_ref()).unwrap_or_default()
    }
}
impl From<Bytes48> for PublicKey {
    fn from(val: Bytes48) -> Self {
        PublicKey::from_bytes(val.as_ref()).unwrap_or_default()
    }
}
impl From<&PublicKey> for Bytes48 {
    fn from(val: &PublicKey) -> Self {
        Bytes48::new(val.to_bytes())
    }
}
impl From<PublicKey> for Bytes48 {
    fn from(val: PublicKey) -> Self {
        (&val).into()
    }
}
impl TryFrom<&Bytes96> for Signature {
    type Error = Error;

    fn try_from(val: &Bytes96) -> Result<Signature, Error> {
        Signature::from_bytes(val.as_ref())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
    }
}

impl TryFrom<Bytes96> for Signature {
    type Error = Error;

    fn try_from(val: Bytes96) -> Result<Signature, Error> {
        Signature::from_bytes(val.as_ref())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
    }
}
impl From<&Signature> for Bytes96 {
    fn from(val: &Signature) -> Bytes96 {
        Bytes96::new(val.to_bytes())
    }
}

impl From<Signature> for Bytes96 {
    fn from(val: Signature) -> Bytes96 {
        Bytes96::new(val.to_bytes())
    }
}
