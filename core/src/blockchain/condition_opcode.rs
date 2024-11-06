use crate::clvm::program::Program;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ConditionCost {
    AggSig = 1_200_000, // the cost of one G1 subgroup check + aggregated signature validation
    CreateCoin = 1_800_000,
    Unknown = 0,
}
impl From<u64> for ConditionCost {
    fn from(value: u64) -> Self {
        match value {
            1_200_000 => ConditionCost::AggSig,
            1_800_000 => ConditionCost::CreateCoin,
            _ => ConditionCost::Unknown,
        }
    }
}
#[derive(ChiaSerial, Copy, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ConditionOpcode {
    Unknown = 0,
    Remark = 1,
    AggSigParent = 43,
    AggSigPuzzle = 44,
    AggSigAmount = 45,
    AggSigPuzzleAmount = 46,
    AggSigParentAmount = 47,
    AggSigParentPuzzle = 48,
    AggSigUnsafe = 49,
    AggSigMe = 50,
    CreateCoin = 51,
    ReserveFee = 52,
    CreateCoinAnnouncement = 60,
    AssertCoinAnnouncement = 61,
    CreatePuzzleAnnouncement = 62,
    AssertPuzzleAnnouncement = 63,
    AssertConcurrentSpend = 64,
    AssertConcurrentPuzzle = 65,
    SendMessage = 66,
    ReceiveMessage = 67,
    AssertMyCoinId = 70,
    AssertMyParentId = 71,
    AssertMyPuzzlehash = 72,
    AssertMyAmount = 73,
    AssertMyBirthSeconds = 74,
    AssertMyBirthHeight = 75,
    AssertEphemeral = 76,
    AssertSecondsRelative = 80,
    AssertSecondsAbsolute = 81,
    AssertHeightRelative = 82,
    AssertHeightAbsolute = 83,
    AssertBeforeSecondsRelative = 84,
    AssertBeforeSecondsAbsolute = 85,
    AssertBeforeHeightRelative = 86,
    AssertBeforeHeightAbsolute = 87,
    SoftFork = 90,
}
impl From<u8> for ConditionOpcode {
    fn from(value: u8) -> Self {
        match value {
            1u8 => ConditionOpcode::Remark,
            43u8 => ConditionOpcode::AggSigParent,
            44u8 => ConditionOpcode::AggSigPuzzle,
            45u8 => ConditionOpcode::AggSigAmount,
            46u8 => ConditionOpcode::AggSigPuzzleAmount,
            47u8 => ConditionOpcode::AggSigParentAmount,
            48u8 => ConditionOpcode::AggSigParentPuzzle,
            49u8 => ConditionOpcode::AggSigUnsafe,
            50u8 => ConditionOpcode::AggSigMe,
            51u8 => ConditionOpcode::CreateCoin,
            52u8 => ConditionOpcode::ReserveFee,
            60u8 => ConditionOpcode::CreateCoinAnnouncement,
            61u8 => ConditionOpcode::AssertCoinAnnouncement,
            62u8 => ConditionOpcode::CreatePuzzleAnnouncement,
            63u8 => ConditionOpcode::AssertPuzzleAnnouncement,
            64u8 => ConditionOpcode::AssertConcurrentSpend,
            65u8 => ConditionOpcode::AssertConcurrentPuzzle,
            70u8 => ConditionOpcode::AssertMyCoinId,
            71u8 => ConditionOpcode::AssertMyParentId,
            72u8 => ConditionOpcode::AssertMyPuzzlehash,
            73u8 => ConditionOpcode::AssertMyAmount,
            74u8 => ConditionOpcode::AssertMyBirthSeconds,
            75u8 => ConditionOpcode::AssertMyBirthHeight,
            76u8 => ConditionOpcode::AssertEphemeral,
            80u8 => ConditionOpcode::AssertSecondsRelative,
            81u8 => ConditionOpcode::AssertSecondsAbsolute,
            82u8 => ConditionOpcode::AssertHeightRelative,
            83u8 => ConditionOpcode::AssertHeightAbsolute,
            84u8 => ConditionOpcode::AssertBeforeSecondsRelative,
            85u8 => ConditionOpcode::AssertBeforeSecondsAbsolute,
            86u8 => ConditionOpcode::AssertBeforeHeightRelative,
            87u8 => ConditionOpcode::AssertBeforeHeightAbsolute,
            90u8 => ConditionOpcode::SoftFork,
            _ => ConditionOpcode::Unknown,
        }
    }
}

impl From<&Program> for ConditionOpcode {
    fn from(value: &Program) -> Self {
        value
            .sexp
            .atom()
            .map(|a| {
                if let Some(v) = a.data.first() {
                    ConditionOpcode::from(*v)
                } else {
                    ConditionOpcode::Unknown
                }
            })
            .unwrap_or(ConditionOpcode::Unknown)
    }
}
