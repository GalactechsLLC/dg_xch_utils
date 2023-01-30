use std::io::Error;

pub mod blockchain;
pub mod errors;
pub mod pool;

pub trait ChiaSerialize {
    fn to_bytes(&self) -> Vec<u8>
    where
        Self: Sized;
    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized;
}
