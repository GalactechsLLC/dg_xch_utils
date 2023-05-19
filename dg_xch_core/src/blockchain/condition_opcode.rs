use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, Hash, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ConditionOpcode {
    UNKNOWN = 48,
    AggSigUnsafe = 49,
    AggSigMe = 50,
    CreateCoin = 51,
    ReserveFee = 52,
    CreateCoinAnnouncement = 60,
    AssertCoinAnnouncement = 61,
    CreatePuzzleAnnouncement = 62,
    AssertPuzzleAnnouncement = 63,
    AssertMyCoinId = 70,
    AssertMyParentId = 71,
    AssertMyPuzzlehash = 72,
    AssertMyAmount = 73,
    AssertSecondsRelative = 80,
    AssertSecondsAbsolute = 81,
    AssertHeightRelative = 82,
    AssertHeightAbsolute = 83,
}
impl From<u8> for ConditionOpcode {
    fn from(value: u8) -> Self {
        match value {
            48u8 => ConditionOpcode::UNKNOWN,
            49u8 => ConditionOpcode::AggSigUnsafe,
            50u8 => ConditionOpcode::AggSigMe,
            51u8 => ConditionOpcode::CreateCoin,
            52u8 => ConditionOpcode::ReserveFee,
            60u8 => ConditionOpcode::CreateCoinAnnouncement,
            61u8 => ConditionOpcode::AssertCoinAnnouncement,
            62u8 => ConditionOpcode::CreatePuzzleAnnouncement,
            63u8 => ConditionOpcode::AssertPuzzleAnnouncement,
            70u8 => ConditionOpcode::AssertMyCoinId,
            71u8 => ConditionOpcode::AssertMyParentId,
            72u8 => ConditionOpcode::AssertMyPuzzlehash,
            73u8 => ConditionOpcode::AssertMyAmount,
            80u8 => ConditionOpcode::AssertSecondsRelative,
            81u8 => ConditionOpcode::AssertSecondsAbsolute,
            82u8 => ConditionOpcode::AssertHeightRelative,
            83u8 => ConditionOpcode::AssertHeightAbsolute,
            _ => ConditionOpcode::UNKNOWN,
        }
    }
}
