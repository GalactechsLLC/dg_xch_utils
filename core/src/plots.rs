use crate::blockchain::coin_record::CoinRecord;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use crate::clvm::program::Program;
use crate::pool::{PoolState, DELAY_PUZZLEHASH_IDENTIFIER, DELAY_TIME_IDENTIFIER};
use hex::encode;
use num_traits::ToPrimitive;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
                pool_contract_puzzle_hash: Some(Bytes32::new(&v[0..32])),
                farmer_public_key: Bytes48::new(&v[32..80]),
                local_master_secret_key: Bytes32::new(&v[80..112]),
            })
        } else if v.len() == 128 {
            Ok(PlotMemo {
                pool_public_key: Some(Bytes48::new(&v[0..48])),
                pool_contract_puzzle_hash: None,
                farmer_public_key: Bytes48::new(&v[48..96]),
                local_master_secret_key: Bytes32::new(&v[96..128]),
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug)]
pub struct PlotNft {
    pub launcher_id: Bytes32,
    pub singleton_coin: CoinRecord,
    pub pool_state: PoolState,
    pub delay_time: i32,
    pub delay_puzzle_hash: Bytes32,
}

pub struct PlotNftExtraData {
    pub pool_state: PoolState,
    pub delay_time: i32,
    pub delay_puzzle_hash: Bytes32,
}
impl PlotNftExtraData {
    pub fn from_program(program: Program) -> Result<Self, Error> {
        let pool_state = PoolState::from_extra_data_program(&program)?;

        let extra_data_program_list = program.as_list();
        let delay_time_programs: Vec<Program> = extra_data_program_list
            .iter()
            .filter(|p| {
                if let Ok(f) = p.first() {
                    if let Ok(ai) = f.as_int() {
                        if let Some(au) = ai.to_u8() {
                            return char::from(au) == DELAY_TIME_IDENTIFIER;
                        }
                    }
                }
                false
            })
            .cloned()
            .collect();
        if delay_time_programs.is_empty() || delay_time_programs.len() > 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid PlotNFT"));
        }
        let delay_time = delay_time_programs[0].rest()?.as_int()?;

        let extra_data_programs: Vec<Program> = extra_data_program_list
            .into_iter()
            .filter(|p| {
                if let Ok(f) = p.first() {
                    if let Ok(ai) = f.as_int() {
                        if let Some(au) = ai.to_u8() {
                            return char::from(au) == DELAY_PUZZLEHASH_IDENTIFIER;
                        }
                    }
                }
                false
            })
            .collect();
        if extra_data_programs.is_empty() || extra_data_programs.len() > 1 {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid PlotNFT"));
        }
        Ok(PlotNftExtraData {
            pool_state,
            delay_time: delay_time.to_i32().unwrap_or_default(),
            delay_puzzle_hash: Bytes32::new(
                &extra_data_programs[0].rest()?.as_vec().unwrap_or_default(),
            ),
        })
    }
}
