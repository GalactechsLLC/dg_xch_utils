use crate::proof_of_space::prover::read_plot_file_header;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes48};
use hex::encode;
use log::debug;
use std::ffi::OsStr;
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};
use std::{fmt, fs};

#[derive(Debug, Clone)]
pub struct PlotMemo {
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub farmer_public_key: Bytes48,
    pub local_master_secret_key: Bytes32,
}
impl TryFrom<Vec<u8>> for PlotMemo {
    type Error = Error;

    fn try_from(v: Vec<u8>) -> Result<Self, Self::Error> {
        if v.len() == 112 {
            Ok(PlotMemo {
                pool_public_key: None,
                pool_contract_puzzle_hash: Some(v[0..32].try_into().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "Invalid byte data for PlotMemo while reading pool_contract_puzzle_hash",
                    )
                })?),
                farmer_public_key: v[32..80].try_into().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "Invalid byte data for PlotMemo while reading farmer_public_key",
                    )
                })?,
                local_master_secret_key: v[80..112].try_into().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "Invalid byte data for PlotMemo while reading local_master_secret_key",
                    )
                })?,
            })
        } else if v.len() == 128 {
            Ok(PlotMemo {
                pool_public_key: Some(v[0..48].try_into().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "Invalid byte data for PlotMemo while reading pool_public_key",
                    )
                })?),
                pool_contract_puzzle_hash: None,
                farmer_public_key: v[48..96].try_into().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "Invalid byte data for PlotMemo while reading farmer_public_key",
                    )
                })?,
                local_master_secret_key: v[96..128].try_into().map_err(|_| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        "Invalid byte data for PlotMemo while reading local_master_secret_key",
                    )
                })?,
            })
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Invalid Vector length. Length must be 112 or 128, found {}",
                    v.len()
                ),
            ))
        }
    }
}
impl Display for PlotMemo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{\n\
            \t\"pool_public_key\": {:?},\n\
            \t\"pool_contract_puzzle_hash\": {:?},\n\
            \t\"farmer_public_key\": {:?},\n\
            \t\"local_master_secret_key\": {:?}\n\
            }}",
            self.pool_public_key
                .as_ref()
                .map(encode)
                .unwrap_or_default(),
            self.pool_contract_puzzle_hash
                .as_ref()
                .map(encode)
                .unwrap_or_default(),
            encode(&self.farmer_public_key),
            encode(&self.local_master_secret_key)
        )
    }
}
#[derive(Debug, Clone)]
pub struct PlotHeader {
    pub magic: [u8; 19],
    pub id: Bytes32,
    pub k: u8,
    pub format_desc_len: u16,
    pub format_desc: Vec<u8>,
    pub memo_len: u16,
    pub memo: PlotMemo,
}
impl PlotHeader {
    pub fn new() -> Self {
        PlotHeader {
            magic: [0; 19],
            id: [0; 32].into(),
            k: 0,
            format_desc_len: 0,
            format_desc: vec![],
            memo_len: 0,
            memo: PlotMemo {
                pool_public_key: None,
                pool_contract_puzzle_hash: None,
                farmer_public_key: [0; 48].into(),
                local_master_secret_key: [0; 32].into(),
            },
        }
    }
}
impl Default for PlotHeader {
    fn default() -> Self {
        Self::new()
    }
}
impl Display for PlotHeader {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{\n\
            \t\"magic\": {:?},\n\
            \t\"id\": {:?},\n\
            \t\"k\": {},\n\
            \t\"format_desc_len\": {},\n\
            \t\"format_desc\": {:?},\n\
            \t\"memo_len\": {},\n\
            \t\"memo\": {}\n\
            }}",
            String::from_utf8(self.magic.to_vec()).map_err(|_| fmt::Error)?,
            encode(&self.id),
            self.k,
            self.format_desc_len,
            String::from_utf8(self.format_desc.to_vec()).map_err(|_| fmt::Error)?,
            self.memo_len,
            format!("{}", self.memo)
                .replace('\t', "\t\t")
                .replace('}', "\t}")
        )
    }
}

type AllPlotHeaders = (Vec<(PathBuf, PlotHeader)>, Vec<PathBuf>);

pub fn read_all_plot_headers(p: impl AsRef<Path>) -> Result<AllPlotHeaders, Error> {
    if !p.as_ref().is_dir() {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "Path must be a directory",
        ))
    } else {
        let dir = fs::read_dir(p)?;
        let mut valid_rtn = vec![];
        let mut failed_rtn = vec![];
        for c in dir {
            match c {
                Ok(c) => {
                    let path = c.path();
                    if path.extension() == Some(OsStr::new("plot")) {
                        match read_plot_file_header(&path) {
                            Ok(d) => {
                                valid_rtn.push(d);
                            }
                            Err(e) => {
                                debug!("Failed to open directory entry: {:?}", e);
                                failed_rtn.push(path.to_path_buf());
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to open directory entry: {:?}", e);
                }
            }
        }
        Ok((valid_rtn, failed_rtn))
    }
}
