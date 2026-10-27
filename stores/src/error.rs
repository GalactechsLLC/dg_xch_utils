use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum StoreError {
    #[cfg(feature = "rocksdb")]
    Rocksdb(rocksdb::Error),
    Backend(sqlx::Error),
    Io(std::io::Error),
    Corrupt(String),
    Batch(String),
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "rocksdb")]
            StoreError::Rocksdb(e) => write!(f, "rocksdb error: {e}"),
            StoreError::Backend(e) => write!(f, "backend error: {e}"),
            StoreError::Io(e) => write!(f, "io error: {e}"),
            StoreError::Corrupt(s) => write!(f, "corrupt store data: {s}"),
            StoreError::Batch(s) => write!(f, "batch error: {s}"),
        }
    }
}
impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(feature = "rocksdb")]
            StoreError::Rocksdb(e) => Some(e),
            StoreError::Backend(e) => Some(e),
            StoreError::Io(e) => Some(e),
            StoreError::Corrupt(_) | StoreError::Batch(_) => None,
        }
    }
}
impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Backend(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl dg_xch_core::errors::ErrorCode for StoreError {
    fn band(&self) -> dg_xch_core::errors::ErrorBand {
        match self {
            StoreError::Io(_) => dg_xch_core::errors::ErrorBand::Io,
            _ => dg_xch_core::errors::ErrorBand::Store,
        }
    }
    fn variant(&self) -> u16 {
        match self {
            #[cfg(feature = "rocksdb")]
            StoreError::Rocksdb(_) => 5,
            StoreError::Backend(_) => 1,
            StoreError::Io(_) => 2,
            StoreError::Corrupt(_) => 3,
            StoreError::Batch(_) => 4,
        }
    }
}
