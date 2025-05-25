pub mod wallets;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Error;

pub trait SizedBytes<'a, const SIZE: usize>: Serialize + Deserialize<'a> + fmt::Display {
    const SIZE: usize = SIZE;
    fn new(bytes: [u8; SIZE]) -> Self;
    fn parse(bytes: &[u8]) -> Result<Self, Error>;
    fn bytes(&self) -> [u8; SIZE];
    fn is_null(&self) -> bool {
        self.bytes() == [0u8; SIZE]
    }
}
