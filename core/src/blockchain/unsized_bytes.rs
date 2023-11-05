use crate::blockchain::sized_bytes::{prep_hex_str, SizedBytes};
use crate::clvm::program::Program;
use dg_xch_serialize::ChiaSerialize;
use hex::{decode, encode};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io::{Cursor, Error, ErrorKind};

#[derive(Clone)]
pub struct UnsizedBytes {
    pub bytes: Vec<u8>,
}
impl<'a> SizedBytes<'a> for UnsizedBytes {
    fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
        }
    }

    fn as_slice(&'a self) -> &'a [u8] {
        &self.bytes
    }

    fn is_null(&self) -> bool {
        self.bytes.iter().all(|v| *v == 0)
    }
}

impl From<String> for UnsizedBytes {
    fn from(hex: String) -> Self {
        let bytes: Vec<u8> = decode(prep_hex_str(&hex)).unwrap();
        UnsizedBytes::new(&bytes)
    }
}

impl From<&String> for UnsizedBytes {
    fn from(hex: &String) -> Self {
        let bytes: Vec<u8> = decode(prep_hex_str(hex)).unwrap();
        UnsizedBytes::new(&bytes)
    }
}

impl From<&str> for UnsizedBytes {
    fn from(hex: &str) -> Self {
        let bytes: Vec<u8> = decode(prep_hex_str(hex)).unwrap();
        UnsizedBytes::new(&bytes)
    }
}

impl TryFrom<Program> for UnsizedBytes {
    type Error = Error;

    fn try_from(value: Program) -> Result<Self, Self::Error> {
        let vec = value
            .as_vec()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Program is not a valid $name"))?;
        Ok(Self::new(&vec))
    }
}

impl TryFrom<&Program> for UnsizedBytes {
    type Error = Error;

    fn try_from(value: &Program) -> Result<Self, Self::Error> {
        let vec = value
            .as_vec()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Program is not a valid $name"))?;
        Ok(Self::new(&vec))
    }
}
impl std::hash::Hash for UnsizedBytes {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl PartialEq for UnsizedBytes {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl Eq for UnsizedBytes {}

impl Serialize for UnsizedBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}

struct UnsizedBytesVisitor;

impl<'de> Visitor<'de> for UnsizedBytesVisitor {
    type Value = UnsizedBytes;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("Expecting a hex String, or byte array")
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

impl<'a> Deserialize<'a> for UnsizedBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        match deserializer.deserialize_string(UnsizedBytesVisitor) {
            Ok(hex) => Ok(hex),
            Err(er) => Err(er),
        }
    }
}

impl fmt::Display for UnsizedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.bytes))
    }
}

impl Default for UnsizedBytes {
    fn default() -> UnsizedBytes {
        UnsizedBytes::new(&[])
    }
}

impl fmt::Debug for UnsizedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.bytes))
    }
}
impl ChiaSerialize for UnsizedBytes {
    fn to_bytes(&self) -> Vec<u8> {
        self.bytes.to_bytes()
    }
    fn from_bytes<T: AsRef<[u8]>>(bytes: &mut Cursor<T>) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let bytes: Vec<u8> = Vec::from_bytes(bytes)?;
        Ok(Self { bytes })
    }
}
