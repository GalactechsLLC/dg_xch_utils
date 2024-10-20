use crate::blockchain::coin_record::CoinRecord;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use crate::clvm::program::Program;
use crate::pool::{PoolState, DELAY_PUZZLEHASH_IDENTIFIER, DELAY_TIME_IDENTIFIER};
use hex::encode;
use num_traits::ToPrimitive;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncSeek};
use tokio::sync::Mutex;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub enum PlotTable {
    Table1 = 0,
    Table2 = 1,
    Table3 = 2,
    Table4 = 3,
    Table5 = 4,
    Table6 = 5,
    Table7 = 6,
    C1 = 7,
    C2 = 8,
    C3 = 9,
}
impl PlotTable {
    #[must_use]
    pub fn lower(&self) -> &Self {
        match self {
            PlotTable::Table1 | PlotTable::Table2 => &PlotTable::Table1,
            PlotTable::Table3 => &PlotTable::Table2,
            PlotTable::Table4 => &PlotTable::Table3,
            PlotTable::Table5 => &PlotTable::Table4,
            PlotTable::Table6 => &PlotTable::Table5,
            PlotTable::Table7 => &PlotTable::Table6,
            PlotTable::C1 => &PlotTable::Table7,
            PlotTable::C2 => &PlotTable::C1,
            PlotTable::C3 => &PlotTable::C2,
        }
    }
}

pub trait PlotFile<'a, F: AsyncSeek + AsyncRead> {
    fn table_address(&'a self, plot_table: PlotTable) -> u64 {
        match self.header() {
            PlotHeader::V1(h) => h.table_begin_pointers[plot_table as usize],
            PlotHeader::V2(h) => h.table_begin_pointers[plot_table as usize],
            PlotHeader::GHv2_5(_) => 0,
        }
    }
    fn table_size(&'a self, plot_table: PlotTable) -> u64 {
        let table_pointers = match self.header() {
            PlotHeader::V1(h) => &h.table_begin_pointers,
            PlotHeader::V2(h) => &h.table_begin_pointers,
            PlotHeader::GHv2_5(_) => {
                return 0;
            }
        };
        let address = table_pointers[plot_table as usize];
        if let Some(next) = table_pointers.get(plot_table as usize + 1) {
            if *next > address {
                return next - address;
            }
        }
        self.plot_size() - address
    }
    fn k(&'a self) -> u8 {
        self.header().k()
    }
    fn plot_id(&'a self) -> Bytes32 {
        self.header().id()
    }
    fn memo(&'a self) -> &'a PlotMemo {
        self.header().memo()
    }
    fn compression_level(&'a self) -> u8 {
        self.header().compression_level()
    }
    //The Interface stuff
    fn header(&'a self) -> &'a PlotHeader;
    fn plot_size(&'a self) -> &'a u64;
    fn load_p7_park(&'a self, index: u64) -> u128;
    fn file(&'a self) -> Arc<Mutex<F>>;
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct PlotMemo {
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub farmer_public_key: Bytes48,
    pub local_master_secret_key: Bytes32,
}
impl TryFrom<&[u8]> for PlotMemo {
    type Error = Error;

    fn try_from(v: &[u8]) -> Result<Self, Self::Error> {
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
            encode(self.farmer_public_key),
            encode(self.local_master_secret_key)
        )
    }
}
#[derive(Debug, Clone, serde::Serialize)]
pub enum PlotHeader {
    V1(PlotHeaderV1),
    V2(PlotHeaderV2),
    GHv2_5(PlotHeaderGHv2_5),
}
impl PlotHeader {
    #[must_use]
    pub fn magic(&self) -> Vec<u8> {
        match self {
            PlotHeader::V1(h) => h.magic.to_vec(),
            PlotHeader::V2(h) => h.magic.to_vec(),
            PlotHeader::GHv2_5(h) => h.magic.to_vec(),
        }
    }
    #[must_use]
    pub fn id(&self) -> Bytes32 {
        match self {
            PlotHeader::V1(h) => h.id,
            PlotHeader::V2(h) => h.id,
            PlotHeader::GHv2_5(h) => h.id,
        }
    }
    #[must_use]
    pub fn k(&self) -> u8 {
        match self {
            PlotHeader::V1(h) => h.k,
            PlotHeader::V2(h) => h.k,
            PlotHeader::GHv2_5(h) => h.k,
        }
    }
    #[must_use]
    pub fn memo_len(&self) -> u16 {
        match self {
            PlotHeader::V1(h) => h.memo_len,
            PlotHeader::V2(h) => h.memo_len,
            PlotHeader::GHv2_5(h) => h.memo_len,
        }
    }
    #[must_use]
    pub fn memo(&self) -> &PlotMemo {
        match self {
            PlotHeader::V1(h) => &h.memo,
            PlotHeader::V2(h) => &h.memo,
            PlotHeader::GHv2_5(h) => &h.memo,
        }
    }
    #[must_use]
    pub fn format_desc_len(&self) -> u16 {
        match self {
            PlotHeader::V1(h) => h.format_desc_len,
            PlotHeader::V2(_) => 0,
            PlotHeader::GHv2_5(h) => h.format_desc_len,
        }
    }
    #[must_use]
    pub fn format_desc(&self) -> &[u8] {
        match self {
            PlotHeader::V1(h) => &h.format_desc,
            PlotHeader::V2(_) => &[],
            PlotHeader::GHv2_5(h) => &h.format_desc,
        }
    }
    #[must_use]
    pub fn plot_flags(&self) -> u32 {
        match self {
            PlotHeader::V1(_) | PlotHeader::GHv2_5(_) => 0,
            PlotHeader::V2(h) => h.plot_flags,
        }
    }
    #[must_use]
    pub fn compression_level(&self) -> u8 {
        match self {
            PlotHeader::V1(_) => 0,
            PlotHeader::V2(h) => h.compression_level,
            PlotHeader::GHv2_5(h) => h.compression_level,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlotHeaderV1 {
    pub magic: [u8; 19],
    pub id: Bytes32,
    pub k: u8,
    pub format_desc_len: u16,
    pub format_desc: Vec<u8>,
    pub memo_len: u16,
    pub memo: PlotMemo,
    pub table_begin_pointers: [u64; 10],
}
impl PlotHeaderV1 {
    #[must_use]
    pub fn new() -> Self {
        PlotHeaderV1 {
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
            table_begin_pointers: [0; 10],
        }
    }
}

impl Default for PlotHeaderV1 {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlotHeaderV2 {
    pub magic: [u8; 4],
    pub id: Bytes32,
    pub k: u8,
    pub memo_len: u16,
    pub memo: PlotMemo,
    pub version: u32,
    pub plot_flags: u32,
    pub compression_level: u8,
    pub table_begin_pointers: [u64; 10],
    pub table_sizes: [u64; 10],
}
impl PlotHeaderV2 {
    #[must_use]
    pub fn new() -> Self {
        PlotHeaderV2 {
            magic: [0; 4],
            id: [0; 32].into(),
            k: 0,
            memo_len: 0,
            memo: PlotMemo {
                pool_public_key: None,
                pool_contract_puzzle_hash: None,
                farmer_public_key: [0; 48].into(),
                local_master_secret_key: [0; 32].into(),
            },
            version: 0,
            plot_flags: 0,
            compression_level: 0,
            table_begin_pointers: [0; 10],
            table_sizes: [0; 10],
        }
    }
}
impl Default for PlotHeaderV2 {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlotHeaderGHv2_5 {
    pub magic: [u8; 4],
    pub id: Bytes32,
    pub k: u8,
    pub format_desc_len: u16,
    pub format_desc: Vec<u8>,
    pub memo_len: u16,
    pub memo: PlotMemo,
    pub compression_level: u8,
}
impl PlotHeaderGHv2_5 {
    #[must_use]
    pub fn new() -> Self {
        PlotHeaderGHv2_5 {
            magic: [0; 4],
            id: [0; 32].into(),
            k: 0,
            memo_len: 0,
            memo: PlotMemo {
                pool_public_key: None,
                pool_contract_puzzle_hash: None,
                farmer_public_key: [0; 48].into(),
                local_master_secret_key: [0; 32].into(),
            },
            format_desc_len: 0,
            format_desc: vec![],
            compression_level: 0,
        }
    }
}
impl Default for PlotHeaderGHv2_5 {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Serialize, Deserialize)]
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
    pub fn from_program(program: &Program) -> Result<Self, Error> {
        let pool_state = PoolState::from_extra_data_program(program)?;
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
