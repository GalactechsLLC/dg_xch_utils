use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum WalletType {
    StandardWallet = 0,
    RateLimited = 1,
    AtomicSwap = 2,
    AuthorizedPayee = 3,
    MultiSig = 4,
    Custody = 5,
    ColouredCoin = 6,
    RECOVERABLE = 7,
    DistributedId = 8,
    PoolingWallet = 9,
    Unknown = u8::MAX as isize,
}
impl From<u8> for WalletType {
    fn from(value: u8) -> Self {
        match value {
            0 => WalletType::StandardWallet,
            1 => WalletType::RateLimited,
            2 => WalletType::AtomicSwap,
            3 => WalletType::AuthorizedPayee,
            4 => WalletType::MultiSig,
            5 => WalletType::Custody,
            6 => WalletType::ColouredCoin,
            7 => WalletType::RECOVERABLE,
            8 => WalletType::DistributedId,
            9 => WalletType::PoolingWallet,
            _ => WalletType::Unknown,
        }
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct AmountWithPuzzleHash {
    pub amount: u64,
    pub puzzle_hash: Bytes32,
    pub memos: Vec<Vec<u8>>,
}
