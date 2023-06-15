use crate::clvm::program::Program;
use blst::min_pk::{PublicKey, SecretKey, Signature};
use bytes::Buf;
use dg_xch_serialize::ChiaSerialize;
use hex::FromHexError;
use hex::{decode, encode};
use log::warn;
#[cfg(feature = "paperclip")]
use paperclip::v2::models::{DataType, DataTypeFormat};
#[cfg(feature = "paperclip")]
use paperclip::v2::schema::TypedData;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io::{Cursor, Error, ErrorKind, Read};

pub fn prep_hex_str(to_fix: &str) -> String {
    let lc = to_fix.to_lowercase();
    if let Some(s) = lc.strip_prefix("0x") {
        s.to_string()
    } else {
        lc
    }
}

pub fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, FromHexError> {
    decode(prep_hex_str(hex))
}

pub fn u64_to_bytes(v: u64) -> Vec<u8> {
    let mut rtn = Vec::new();
    if v.leading_zeros() == 0 {
        rtn.push(u8::MIN);
        let ary = v.to_be_bytes();
        rtn.extend_from_slice(&ary);
        rtn
    } else {
        let mut trim: bool = true;
        for b in v.to_be_bytes() {
            if trim {
                if b == u8::MIN {
                    continue;
                } else {
                    rtn.push(b);
                    trim = false;
                }
            } else {
                rtn.push(b);
            }
        }
        rtn
    }
}

pub trait SizedBytes<'a>: Serialize + Deserialize<'a> + fmt::Display {
    fn new(bytes: &[u8]) -> Self;
    fn as_slice(&'a self) -> &'a [u8];
    fn is_null(&self) -> bool;
}
#[cfg(feature = "paperclip")]
impl TypedData for Bytes32 {
    fn data_type() -> DataType {
        DataType::String
    }
    fn format() -> Option<DataTypeFormat> {
        Some(DataTypeFormat::Other)
    }
}
#[cfg(feature = "paperclip")]
impl TypedData for Bytes48 {
    fn data_type() -> DataType {
        DataType::String
    }
    fn format() -> Option<DataTypeFormat> {
        Some(DataTypeFormat::Other)
    }
}
macro_rules! impl_sized_bytes {

    ($($name: ident, $size:expr, $visitor:ident);*) => {
        $(
            #[derive(Copy, Clone)]
            pub struct $name {
                pub bytes: [u8; $size]
            }
            impl<'a> SizedBytes<'a> for $name {
                fn new(bytes: &[u8]) -> Self {
                    if bytes.len() > $size {
                        warn!("Too Many Bytes Sent to {}, expected {} got {}", stringify!($name), $size, bytes.len());
                        let mut buf = [0u8; $size];
                        buf.copy_from_slice(&bytes[..$size]);
                        Self {
                            bytes: buf
                        }
                    } else {
                        let mut buf = [0u8; $size];
                        for (i, v) in bytes.iter().enumerate() {
                            buf[i] = *v;
                        }
                        Self {
                            bytes: buf
                        }
                    }
                }

                fn as_slice(&'a self) -> &'a [u8] {
                    &self.as_ref()
                }

                fn is_null(&self) -> bool {
                    self.bytes.iter().all(|v| *v ==0)
                }
            }
            impl<'a> $name {
                pub fn from_sized_bytes(bytes: [u8; $size]) -> Self {
                    $name { bytes }
                }
                pub fn to_sized_bytes(&'a self) -> &'a [u8; $size] {
                    &self.bytes
                }
            }

            impl std::hash::Hash for $name {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    self.bytes.hash(state);
                }
            }

            impl PartialEq for $name {
                fn eq(&self, other: &Self) -> bool {
                    self.bytes == other.bytes
                }
            }
            impl Eq for $name {}

            impl Serialize for $name {
                fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
                where
                    S: Serializer,
                {
                    serializer.serialize_str(self.to_string().as_str())
                }
            }

            impl AsRef<[u8]> for $name {
                fn as_ref(&self) -> &[u8] {
                    &self.bytes
                }
            }

            impl From<[u8; $size]> for $name {
                fn from(bytes: [u8; $size]) -> Self {
                    $name::from_sized_bytes(bytes)
                }
            }

            impl From<&[u8; $size]> for $name {
                fn from(bytes: &[u8; $size]) -> Self {
                    $name::from_sized_bytes(*bytes)
                }
            }

            impl From<String> for $name {
                fn from(hex: String) -> Self {
                    let bytes: Vec<u8> = decode(prep_hex_str(&hex)).unwrap();
                    $name::new(&bytes)
                }
            }

            impl From<&String> for $name {
                fn from(hex: &String) -> Self {
                    let bytes: Vec<u8> = decode(prep_hex_str(hex)).unwrap();
                    $name::new(&bytes)
                }
            }

            impl From<&str> for $name {
                fn from(hex: &str) -> Self {
                    let bytes: Vec<u8> = decode(prep_hex_str(hex)).unwrap();
                    $name::new(&bytes)
                }
            }

            impl TryFrom<Program> for $name {
                type Error = Error;

                fn try_from(value: Program) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Program is not a valid $name"))?;
                    Ok(Self::new(&vec))
                }
            }

            impl TryFrom<&Program> for $name {
                type Error = Error;

                fn try_from(value: &Program) -> Result<Self, Self::Error> {
                    let vec = value.as_vec().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Program is not a valid $name"))?;
                    Ok(Self::new(&vec))
                }
            }

            struct $visitor;

            impl<'de> Visitor<'de> for $visitor {
                type Value = $name;

                fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                    formatter.write_str(format!("Expecting a hex String, or byte array of size {}", $size).as_str())
                }

                fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
                where
                    E: std::error::Error,
                {
                    Ok(value.into())
                }

                fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                where
                    E: std::error::Error,
                {
                    Ok(value.into())
                }
            }

            impl<'a> Deserialize<'a> for $name {
                fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
                where
                    D: Deserializer<'a>,
                {
                    match deserializer.deserialize_string($visitor) {
                        Ok(hex) => Ok(hex),
                        Err(er) => Err(er),
                    }
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "{}", encode(&self.bytes))
                }
            }

            impl Default for $name {
                fn default() -> $name {
                    $name::from([0; $size])
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "{}", encode(&self.bytes))
                }
            }

        )*
    };
    ()=>{};
}

impl_sized_bytes!(
    Bytes4, 4, Bytes4Visitor;
    Bytes8, 8, Bytes8Visitor;
    Bytes16, 16, Bytes16Visitor;
    Bytes32, 32, Bytes32Visitor;
    Bytes48, 48, Bytes48Visitor;
    Bytes96, 96, Bytes96Visitor;
    Bytes192, 192, Bytes192Visitor
);

macro_rules! impl_sized_bytes_serial {
    ($($name: ident, $size:expr);*) => {
        $(
            impl ChiaSerialize for $name {
                fn to_bytes(&self) -> Vec<u8> {
                    self.to_sized_bytes().to_vec()
                }
                fn from_bytes<T: AsRef<[u8]>>(bytes: &mut Cursor<T>) -> Result<Self, Error> where Self: Sized,
                {
                    if bytes.remaining() < $size {
                        Err(Error::new(ErrorKind::InvalidInput, format!("Failed to Parse $name, expected length $size, found {}",  bytes.remaining())))
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
impl_sized_bytes_serial!(
    Bytes4, 4;
    Bytes8, 8;
    Bytes16, 16;
    Bytes32, 32;
    Bytes48, 48;
    Bytes96, 96;
    Bytes192, 192
);

impl From<&Bytes32> for SecretKey {
    fn from(val: &Bytes32) -> Self {
        SecretKey::from_bytes(val.to_sized_bytes()).unwrap_or_default()
    }
}
impl From<Bytes32> for SecretKey {
    fn from(val: Bytes32) -> Self {
        SecretKey::from_bytes(val.to_sized_bytes()).unwrap_or_default()
    }
}

impl From<&Bytes48> for PublicKey {
    fn from(val: &Bytes48) -> Self {
        PublicKey::from_bytes(val.to_sized_bytes()).unwrap_or_default()
    }
}
impl From<Bytes48> for PublicKey {
    fn from(val: Bytes48) -> Self {
        PublicKey::from_bytes(val.to_sized_bytes()).unwrap_or_default()
    }
}

impl TryFrom<&Bytes96> for Signature {
    type Error = Error;

    fn try_from(val: &Bytes96) -> Result<Signature, Error> {
        Signature::from_bytes(val.to_sized_bytes())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
    }
}
impl TryFrom<Bytes96> for Signature {
    type Error = Error;

    fn try_from(val: Bytes96) -> Result<Signature, Error> {
        Signature::from_bytes(val.to_sized_bytes())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
    }
}
impl From<Signature> for Bytes96 {
    fn from(val: Signature) -> Bytes96 {
        Bytes96::from_sized_bytes(val.to_bytes())
    }
}
