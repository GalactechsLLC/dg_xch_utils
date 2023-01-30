use serde::{Deserialize, Serialize};
use std::error::Error;

#[derive(Clone, Hash, PartialEq, Eq, Serialize, Deserialize, Debug)]
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
impl ConditionOpcode {
    pub fn from_u8(b: u8) -> Result<Self, Box<dyn Error>> {
        match b {
            48u8 => Ok(ConditionOpcode::UNKNOWN),
            49u8 => Ok(ConditionOpcode::AggSigUnsafe),
            50u8 => Ok(ConditionOpcode::AggSigMe),
            51u8 => Ok(ConditionOpcode::CreateCoin),
            52u8 => Ok(ConditionOpcode::ReserveFee),
            60u8 => Ok(ConditionOpcode::CreateCoinAnnouncement),
            61u8 => Ok(ConditionOpcode::AssertCoinAnnouncement),
            62u8 => Ok(ConditionOpcode::CreatePuzzleAnnouncement),
            63u8 => Ok(ConditionOpcode::AssertPuzzleAnnouncement),
            70u8 => Ok(ConditionOpcode::AssertMyCoinId),
            71u8 => Ok(ConditionOpcode::AssertMyParentId),
            72u8 => Ok(ConditionOpcode::AssertMyPuzzlehash),
            73u8 => Ok(ConditionOpcode::AssertMyAmount),
            80u8 => Ok(ConditionOpcode::AssertSecondsRelative),
            81u8 => Ok(ConditionOpcode::AssertSecondsAbsolute),
            82u8 => Ok(ConditionOpcode::AssertHeightRelative),
            83u8 => Ok(ConditionOpcode::AssertHeightAbsolute),
            _ => Err(format!("Not a Valid OpCode: {}", b).into()),
        }
    }
}
