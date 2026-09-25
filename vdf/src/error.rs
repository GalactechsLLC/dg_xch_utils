use std::fmt::{Display, Formatter};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidDiscriminantSize,
    EmptySeed,
    InvalidDiscriminant,
    InvalidFormSize,
    InvalidProofLength,
    InvalidCompressedForm,
    FormNotReduced,
    InvalidForm,
    InvalidSegmentIterations,
    InvalidProofParameters,
    WitnessTooLarge {
        witness_type: u8,
        max_vdf_witness_size: u64,
    },
    TargetVdfMismatch,
    ProverMemoryLimit,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidDiscriminantSize => f.write_str(
                "discriminant size must be positive, no larger than 1024 bits, and a multiple of 8",
            ),
            Error::EmptySeed => f.write_str("seed cannot be empty"),
            Error::InvalidDiscriminant => {
                f.write_str("discriminant is empty or larger than 1024 bits")
            }
            Error::InvalidFormSize => f.write_str("serialized form must be exactly 100 bytes"),
            Error::InvalidProofLength => {
                f.write_str("serialized proof length does not match the recursion depth")
            }
            Error::InvalidCompressedForm => f.write_str("compressed form is malformed"),
            Error::FormNotReduced => f.write_str("form is not reduced"),
            Error::InvalidForm => f.write_str("form does not satisfy the supplied discriminant"),
            Error::InvalidSegmentIterations => {
                f.write_str("segment iterations exceed remaining iterations")
            }
            Error::InvalidProofParameters => {
                f.write_str("cannot compute proof parameters for the requested iteration count")
            }
            Error::WitnessTooLarge {
                witness_type,
                max_vdf_witness_size,
            } => write!(
                f,
                "VDF witness type {witness_type} exceeds max witness size {max_vdf_witness_size}"
            ),
            Error::TargetVdfMismatch => {
                f.write_str("VDF info does not match the expected target VDF info")
            }
            Error::ProverMemoryLimit => f.write_str("VDF prover memory budget is insufficient"),
        }
    }
}

impl std::error::Error for Error {}

impl dg_xch_core::errors::ErrorCode for Error {
    fn band(&self) -> dg_xch_core::errors::ErrorBand {
        dg_xch_core::errors::ErrorBand::Vdf
    }
    fn variant(&self) -> u16 {
        match self {
            Error::InvalidDiscriminantSize => 1,
            Error::EmptySeed => 2,
            Error::InvalidDiscriminant => 3,
            Error::InvalidFormSize => 4,
            Error::InvalidProofLength => 5,
            Error::InvalidCompressedForm => 6,
            Error::FormNotReduced => 7,
            Error::InvalidForm => 8,
            Error::InvalidSegmentIterations => 9,
            Error::InvalidProofParameters => 10,
            Error::WitnessTooLarge { .. } => 11,
            Error::TargetVdfMismatch => 12,
            Error::ProverMemoryLimit => 13,
        }
    }
}
