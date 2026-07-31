use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::parser::{sexp_from_bytes, sexp_to_bytes};
use crate::clvm::program::Program;
use crate::clvm::sexp::{AtomBuf, SExp};
use crate::constants::NULL_SEXP;
use crate::errors::ClvmError;
use crate::formatting::{number_from_slice, u32_from_slice, u64_from_bigint};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::warn;
use serde::de::Error as SerialError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Error, ErrorKind};

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
enum MessageArgsType {
    None,
    CoinId,
    Parent,
    Puzzle,
    Amount,
    ParentPuzzle,
    ParentAmount,
    PuzzleAmount,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum MessageArgs {
    None,
    CoinId(Bytes32),
    Parent(Bytes32),
    Puzzle(Bytes32),
    Amount(u64),
    ParentPuzzle {
        parent: Bytes32,
        puzzle_hash: Bytes32,
    },
    ParentAmount {
        parent: Bytes32,
        amount: u64,
    },
    PuzzleAmount {
        puzzle_hash: Bytes32,
        amount: u64,
    },
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Message(usize, [u8; 1024]);
impl Message {
    pub fn new(msg: Vec<u8>) -> Result<Message, ClvmError> {
        if msg.len() > 1024 {
            Err(ClvmError::InvalidInput("Message too long".to_string()))
        } else {
            let mut buf = [0u8; 1024];
            let length = msg.len();
            buf[0..length].copy_from_slice(msg.as_slice());
            Ok(Message(length, buf))
        }
    }
    pub fn data(&self) -> &[u8] {
        &self.1[0..self.0]
    }
}

impl Display for Message {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.data()))
    }
}

impl Debug for Message {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", hex::encode(self.data()))
    }
}

impl Hash for Message {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl AsRef<[u8]> for Message {
    fn as_ref(&self) -> &[u8] {
        &self.1[0..self.0]
    }
}

impl From<Message> for SExp<'static> {
    fn from(msg: Message) -> SExp<'static> {
        (&msg).into()
    }
}

impl From<&Message> for SExp<'static> {
    fn from(msg: &Message) -> SExp<'static> {
        SExp::Atom(AtomBuf::new(msg.data().to_vec()))
    }
}

impl Serialize for Message {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(&self.1[0..self.0])
    }
}
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if let Ok(data) = <Vec<u8>>::deserialize(deserializer) {
            Ok(Message::new(data).map_err(|e| D::Error::custom(e.to_string()))?)
        } else {
            Err(D::Error::custom(
                "Expected Byte Array to Deserialize Message",
            ))
        }
    }
}

impl ChiaSerialize for Message {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        self.as_ref().to_vec().to_bytes(version)
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let vec_data: Vec<u8> = Vec::from_bytes(bytes, version)?;
        Message::new(vec_data).map_err(Into::into)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ConditionWithArgs {
    Unknown,
    Remark(Message),
    AggSigParent(Bytes48, Message),
    AggSigPuzzle(Bytes48, Message),
    AggSigAmount(Bytes48, Message),
    AggSigPuzzleAmount(Bytes48, Message),
    AggSigParentAmount(Bytes48, Message),
    AggSigParentPuzzle(Bytes48, Message),
    AggSigUnsafe(Bytes48, Message),
    AggSigMe(Bytes48, Message),
    CreateCoin(Bytes32, u64, Vec<Vec<u8>>),
    ReserveFee(u64),
    CreateCoinAnnouncement(Message),
    AssertCoinAnnouncement(Bytes32),
    CreatePuzzleAnnouncement(Message),
    AssertPuzzleAnnouncement(Bytes32),
    AssertConcurrentSpend(Bytes32),
    AssertConcurrentPuzzle(Bytes32),
    SendMessage(u8, Message, MessageArgs),
    ReceiveMessage(u8, Message, MessageArgs),
    AssertMyCoinId(Bytes32),
    AssertMyParentId(Bytes32),
    AssertMyPuzzlehash(Bytes32),
    AssertMyAmount(u64),
    AssertMyBirthSeconds(u64),
    AssertMyBirthHeight(u32),
    AssertEphemeral,
    AssertSecondsRelative(u64),
    AssertSecondsAbsolute(u64),
    AssertHeightRelative(u32),
    AssertHeightAbsolute(u32),
    AssertBeforeSecondsRelative(u64),
    AssertBeforeSecondsAbsolute(u64),
    AssertBeforeHeightRelative(u32),
    AssertBeforeHeightAbsolute(u32),
    SoftFork(u64),
}
impl<'a> TryFrom<&'a SExp<'a>> for ConditionWithArgs {
    type Error = ClvmError;
    fn try_from(sexp: &'a SExp<'a>) -> Result<Self, Self::Error> {
        let (op_code, args) = op_code_with_args_from_sexp(sexp)?;
        from_opcode_with_args(op_code, args).map_err(ClvmError::IoError)
    }
}
impl Display for ConditionWithArgs {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let (op_code, vars) = self.op_code_with_args();
        write!(f, "{op_code} ")?;
        for var in &vars {
            write!(f, "{var} ")?;
        }
        Ok(())
    }
}

impl<'a> From<&'a ConditionWithArgs> for SExp<'a> {
    fn from(conditions: &'a ConditionWithArgs) -> SExp<'a> {
        let mut as_sexp = NULL_SEXP;
        let (op_code, vars) = conditions.op_code_with_args();
        for var in vars.into_iter().rev() {
            as_sexp = var.cons(as_sexp)
        }
        SExp::from(op_code).cons(as_sexp)
    }
}

impl ConditionWithArgs {
    pub fn op_code_with_args(&self) -> (ConditionOpcode, Vec<SExp<'static>>) {
        match self {
            ConditionWithArgs::Unknown => (ConditionOpcode::Unknown, vec![]),
            ConditionWithArgs::Remark(msg) => (ConditionOpcode::Remark, vec![msg.into()]),
            ConditionWithArgs::AggSigParent(key, msg) => {
                (ConditionOpcode::AggSigParent, vec![key.into(), msg.into()])
            }
            ConditionWithArgs::AggSigPuzzle(key, msg) => {
                (ConditionOpcode::AggSigPuzzle, vec![key.into(), msg.into()])
            }
            ConditionWithArgs::AggSigAmount(key, msg) => {
                (ConditionOpcode::AggSigAmount, vec![key.into(), msg.into()])
            }
            ConditionWithArgs::AggSigPuzzleAmount(key, msg) => (
                ConditionOpcode::AggSigPuzzleAmount,
                vec![key.into(), msg.into()],
            ),
            ConditionWithArgs::AggSigParentAmount(key, msg) => (
                ConditionOpcode::AggSigParentAmount,
                vec![key.into(), msg.into()],
            ),
            ConditionWithArgs::AggSigParentPuzzle(key, msg) => (
                ConditionOpcode::AggSigParentPuzzle,
                vec![key.into(), msg.into()],
            ),
            ConditionWithArgs::AggSigUnsafe(key, msg) => {
                (ConditionOpcode::AggSigUnsafe, vec![key.into(), msg.into()])
            }
            ConditionWithArgs::AggSigMe(key, msg) => {
                (ConditionOpcode::AggSigMe, vec![key.into(), msg.into()])
            }
            ConditionWithArgs::CreateCoin(puzzle_hash, amount, memos) => {
                let vars = vec![puzzle_hash.into(), amount.into(), memos_to_sexp(memos)];
                (ConditionOpcode::CreateCoin, vars)
            }
            ConditionWithArgs::ReserveFee(fee) => (ConditionOpcode::ReserveFee, vec![fee.into()]),
            ConditionWithArgs::CreateCoinAnnouncement(puzzle_hash) => (
                ConditionOpcode::CreateCoinAnnouncement,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::AssertCoinAnnouncement(puzzle_hash) => (
                ConditionOpcode::AssertCoinAnnouncement,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::CreatePuzzleAnnouncement(puzzle_hash) => (
                ConditionOpcode::CreatePuzzleAnnouncement,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::AssertPuzzleAnnouncement(puzzle_hash) => (
                ConditionOpcode::AssertPuzzleAnnouncement,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::AssertConcurrentSpend(puzzle_hash) => (
                ConditionOpcode::AssertConcurrentSpend,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::AssertConcurrentPuzzle(puzzle_hash) => (
                ConditionOpcode::AssertConcurrentPuzzle,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::SendMessage(mode, msg, msg_args) => {
                let mut vars = vec![mode.into(), msg.into()];
                vars.extend(msg_args_sexp_ary(msg_args));
                (ConditionOpcode::SendMessage, vars)
            }
            ConditionWithArgs::ReceiveMessage(mode, msg, msg_args) => {
                let mut vars = vec![mode.into(), msg.into()];
                vars.extend(msg_args_sexp_ary(msg_args));
                (ConditionOpcode::ReceiveMessage, vars)
            }
            ConditionWithArgs::AssertMyCoinId(puzzle_hash) => {
                (ConditionOpcode::AssertMyCoinId, vec![puzzle_hash.into()])
            }
            ConditionWithArgs::AssertMyParentId(puzzle_hash) => {
                (ConditionOpcode::AssertMyParentId, vec![puzzle_hash.into()])
            }
            ConditionWithArgs::AssertMyPuzzlehash(puzzle_hash) => (
                ConditionOpcode::AssertMyPuzzlehash,
                vec![puzzle_hash.into()],
            ),
            ConditionWithArgs::AssertMyAmount(amount) => {
                (ConditionOpcode::AssertMyAmount, vec![amount.into()])
            }
            ConditionWithArgs::AssertMyBirthSeconds(seconds) => {
                (ConditionOpcode::AssertMyBirthSeconds, vec![seconds.into()])
            }
            ConditionWithArgs::AssertMyBirthHeight(height) => {
                (ConditionOpcode::AssertMyBirthHeight, vec![height.into()])
            }
            ConditionWithArgs::AssertEphemeral => (ConditionOpcode::AssertEphemeral, vec![]),
            ConditionWithArgs::AssertSecondsRelative(seconds) => {
                (ConditionOpcode::AssertSecondsRelative, vec![seconds.into()])
            }
            ConditionWithArgs::AssertSecondsAbsolute(seconds) => {
                (ConditionOpcode::AssertSecondsAbsolute, vec![seconds.into()])
            }
            ConditionWithArgs::AssertHeightRelative(height) => {
                (ConditionOpcode::AssertHeightRelative, vec![height.into()])
            }
            ConditionWithArgs::AssertHeightAbsolute(height) => {
                (ConditionOpcode::AssertHeightAbsolute, vec![height.into()])
            }
            ConditionWithArgs::AssertBeforeSecondsRelative(seconds) => (
                ConditionOpcode::AssertBeforeSecondsRelative,
                vec![seconds.into()],
            ),
            ConditionWithArgs::AssertBeforeSecondsAbsolute(seconds) => (
                ConditionOpcode::AssertBeforeSecondsAbsolute,
                vec![seconds.into()],
            ),
            ConditionWithArgs::AssertBeforeHeightRelative(height) => (
                ConditionOpcode::AssertBeforeHeightRelative,
                vec![height.into()],
            ),
            ConditionWithArgs::AssertBeforeHeightAbsolute(height) => (
                ConditionOpcode::AssertBeforeHeightAbsolute,
                vec![height.into()],
            ),
            ConditionWithArgs::SoftFork(cost) => (ConditionOpcode::SoftFork, vec![cost.into()]),
        }
    }

    pub fn op_code(&self) -> ConditionOpcode {
        match self {
            ConditionWithArgs::Unknown => ConditionOpcode::Unknown,
            ConditionWithArgs::Remark(_) => ConditionOpcode::Remark,
            ConditionWithArgs::AggSigParent(_, _) => ConditionOpcode::AggSigParent,
            ConditionWithArgs::AggSigPuzzle(_, _) => ConditionOpcode::AggSigPuzzle,
            ConditionWithArgs::AggSigAmount(_, _) => ConditionOpcode::AggSigAmount,
            ConditionWithArgs::AggSigPuzzleAmount(_, _) => ConditionOpcode::AggSigPuzzleAmount,
            ConditionWithArgs::AggSigParentAmount(_, _) => ConditionOpcode::AggSigParentAmount,
            ConditionWithArgs::AggSigParentPuzzle(_, _) => ConditionOpcode::AggSigParentPuzzle,
            ConditionWithArgs::AggSigUnsafe(_, _) => ConditionOpcode::AggSigUnsafe,
            ConditionWithArgs::AggSigMe(_, _) => ConditionOpcode::AggSigMe,
            ConditionWithArgs::CreateCoin(_, _, _) => ConditionOpcode::CreateCoin,
            ConditionWithArgs::ReserveFee(_) => ConditionOpcode::ReserveFee,
            ConditionWithArgs::CreateCoinAnnouncement(_) => ConditionOpcode::CreateCoinAnnouncement,
            ConditionWithArgs::AssertCoinAnnouncement(_) => ConditionOpcode::AssertCoinAnnouncement,
            ConditionWithArgs::CreatePuzzleAnnouncement(_) => {
                ConditionOpcode::CreatePuzzleAnnouncement
            }
            ConditionWithArgs::AssertPuzzleAnnouncement(_) => {
                ConditionOpcode::AssertPuzzleAnnouncement
            }
            ConditionWithArgs::AssertConcurrentSpend(_) => ConditionOpcode::AssertConcurrentSpend,
            ConditionWithArgs::AssertConcurrentPuzzle(_) => ConditionOpcode::AssertConcurrentPuzzle,
            ConditionWithArgs::SendMessage(_, _, _) => ConditionOpcode::SendMessage,
            ConditionWithArgs::ReceiveMessage(_, _, _) => ConditionOpcode::ReceiveMessage,
            ConditionWithArgs::AssertMyCoinId(_) => ConditionOpcode::AssertMyCoinId,
            ConditionWithArgs::AssertMyParentId(_) => ConditionOpcode::AssertMyParentId,
            ConditionWithArgs::AssertMyPuzzlehash(_) => ConditionOpcode::AssertMyPuzzlehash,
            ConditionWithArgs::AssertMyAmount(_) => ConditionOpcode::AssertMyAmount,
            ConditionWithArgs::AssertMyBirthSeconds(_) => ConditionOpcode::AssertMyBirthSeconds,
            ConditionWithArgs::AssertMyBirthHeight(_) => ConditionOpcode::AssertMyBirthHeight,
            ConditionWithArgs::AssertEphemeral => ConditionOpcode::AssertEphemeral,
            ConditionWithArgs::AssertSecondsRelative(_) => ConditionOpcode::AssertSecondsRelative,
            ConditionWithArgs::AssertSecondsAbsolute(_) => ConditionOpcode::AssertSecondsAbsolute,
            ConditionWithArgs::AssertHeightRelative(_) => ConditionOpcode::AssertHeightRelative,
            ConditionWithArgs::AssertHeightAbsolute(_) => ConditionOpcode::AssertHeightAbsolute,
            ConditionWithArgs::AssertBeforeSecondsRelative(_) => {
                ConditionOpcode::AssertBeforeSecondsRelative
            }
            ConditionWithArgs::AssertBeforeSecondsAbsolute(_) => {
                ConditionOpcode::AssertBeforeSecondsAbsolute
            }
            ConditionWithArgs::AssertBeforeHeightRelative(_) => {
                ConditionOpcode::AssertBeforeHeightRelative
            }
            ConditionWithArgs::AssertBeforeHeightAbsolute(_) => {
                ConditionOpcode::AssertBeforeHeightAbsolute
            }
            ConditionWithArgs::SoftFork(_) => ConditionOpcode::SoftFork,
        }
    }
}

impl ChiaSerialize for ConditionWithArgs {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        match self {
            ConditionWithArgs::Unknown => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(&ConditionOpcode::Unknown, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::Remark(msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(&ConditionOpcode::Remark, version)?);
                let vars = vec![ChiaSerialize::to_bytes(msg, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigParent(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigParent,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigPuzzle(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigPuzzle,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigAmount(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigAmount,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigPuzzleAmount(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigPuzzleAmount,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigParentAmount(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigParentAmount,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigParentPuzzle(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigParentPuzzle,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigUnsafe(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigUnsafe,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AggSigMe(key, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AggSigMe,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(key, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::CreateCoin(puzzle_hash, amount, memos) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::CreateCoin,
                    version,
                )?);
                let mut vars = vec![
                    ChiaSerialize::to_bytes(puzzle_hash, version)?,
                    ChiaSerialize::to_bytes(amount, version)?,
                ];
                vars.push(sexp_to_bytes(&memos_to_sexp(memos))?.as_ref().to_vec());
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::ReserveFee(fee) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::ReserveFee,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(fee, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::CreateCoinAnnouncement(msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::CreateCoinAnnouncement,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(msg, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertCoinAnnouncement(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertCoinAnnouncement,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::CreatePuzzleAnnouncement(msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::CreatePuzzleAnnouncement,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(msg, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertPuzzleAnnouncement(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertPuzzleAnnouncement,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertConcurrentSpend(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertConcurrentSpend,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertConcurrentPuzzle(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertConcurrentPuzzle,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::SendMessage(mode, msg, msg_args) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::SendMessage,
                    version,
                )?);
                let mut vars = vec![
                    ChiaSerialize::to_bytes(mode, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                vars.extend(message_args_bytes(msg_args, version)?);
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::ReceiveMessage(mode, msg, msg_args) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::ReceiveMessage,
                    version,
                )?);
                let mut vars = vec![
                    ChiaSerialize::to_bytes(mode, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                vars.extend(message_args_bytes(msg_args, version)?);
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertMyCoinId(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertMyCoinId,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertMyParentId(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertMyParentId,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertMyPuzzlehash(puzzle_hash) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertMyPuzzlehash,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(puzzle_hash, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertMyAmount(amount) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertMyAmount,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(amount, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertMyBirthSeconds(seconds) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertMyBirthSeconds,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(seconds, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertMyBirthHeight(height) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertMyBirthHeight,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(height, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertEphemeral => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertEphemeral,
                    version,
                )?);
                let vars: Vec<u8> = Vec::with_capacity(0);
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertSecondsRelative(seconds) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertSecondsRelative,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(seconds, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertSecondsAbsolute(seconds) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertSecondsAbsolute,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(seconds, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertHeightRelative(height) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertHeightRelative,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(height, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertHeightAbsolute(height) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertHeightAbsolute,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(height, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertBeforeSecondsRelative(seconds) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertBeforeSecondsRelative,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(seconds, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertBeforeSecondsAbsolute(seconds) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertBeforeSecondsAbsolute,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(seconds, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertBeforeHeightRelative(height) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertBeforeHeightRelative,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(height, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::AssertBeforeHeightAbsolute(height) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::AssertBeforeHeightAbsolute,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(height, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::SoftFork(cost) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::SoftFork,
                    version,
                )?);
                let vars = vec![ChiaSerialize::to_bytes(cost, version)?];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
        }
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let op_code: ConditionOpcode = ConditionOpcode::from_bytes(bytes, version)?;
        let args: Vec<Vec<u8>> = Vec::<Vec<u8>>::from_bytes(bytes, version)?;
        from_opcode_with_args(op_code, args)
    }
}

fn from_opcode_with_args(
    op_code: ConditionOpcode,
    mut args: Vec<Vec<u8>>,
) -> Result<ConditionWithArgs, Error> {
    //Length of Args is Checked for Each type, pop is used to move the memory instead of copy
    //This means args are fetched in reverse order from the array
    Ok(match op_code {
        ConditionOpcode::Unknown => ConditionWithArgs::Unknown,
        ConditionOpcode::Remark => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for Remark",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                ConditionWithArgs::Remark(message)
            }
        }
        ConditionOpcode::AggSigParent => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigParent",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigParent(public_key, message)
            }
        }
        ConditionOpcode::AggSigPuzzle => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigPuzzle",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigPuzzle(public_key, message)
            }
        }
        ConditionOpcode::AggSigAmount => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigAmount",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigAmount(public_key, message)
            }
        }
        ConditionOpcode::AggSigPuzzleAmount => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigPuzzleAmount",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigPuzzleAmount(public_key, message)
            }
        }
        ConditionOpcode::AggSigParentAmount => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigParentAmount",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigParentAmount(public_key, message)
            }
        }
        ConditionOpcode::AggSigParentPuzzle => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigParentPuzzle",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigParentPuzzle(public_key, message)
            }
        }
        ConditionOpcode::AggSigUnsafe => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigUnsafe",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigUnsafe(public_key, message)
            }
        }
        ConditionOpcode::AggSigMe => {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AggSigMe",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let public_key = Bytes48::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AggSigMe(public_key, message)
            }
        }
        ConditionOpcode::CreateCoin => {
            if args.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for CreateCoin",
                ));
            } else {
                args.reverse();
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                let amount_bytes = args.pop().unwrap_or_default();
                let amount = u64_from_bigint(&number_from_slice(&amount_bytes))?;
                let memos = if args.is_empty() {
                    vec![]
                } else if args.len() == 1 {
                    let memo_bytes = args.pop().unwrap_or_default();
                    match memos_from_bytes(&memo_bytes) {
                        Ok(memos) => memos,
                        Err(_) => vec![memo_bytes],
                    }
                } else {
                    let mut memos = vec![];
                    while let Some(val) = args.pop() {
                        memos.push(val);
                    }
                    memos
                };
                ConditionWithArgs::CreateCoin(puzzle_hash, amount, memos)
            }
        }
        ConditionOpcode::ReserveFee => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for CreateCoin",
                ));
            } else {
                let fee_bytes = args.pop().unwrap_or_default();
                let fee = u64_from_bigint(&number_from_slice(&fee_bytes))?;
                ConditionWithArgs::ReserveFee(fee)
            }
        }
        ConditionOpcode::CreateCoinAnnouncement => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for CreateCoinAnnouncement",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                ConditionWithArgs::CreateCoinAnnouncement(message)
            }
        }
        ConditionOpcode::AssertCoinAnnouncement => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertCoinAnnouncement",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertCoinAnnouncement(puzzle_hash)
            }
        }
        ConditionOpcode::CreatePuzzleAnnouncement => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for CreatePuzzleAnnouncement",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                ConditionWithArgs::CreatePuzzleAnnouncement(message)
            }
        }
        ConditionOpcode::AssertPuzzleAnnouncement => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertPuzzleAnnouncement",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertPuzzleAnnouncement(puzzle_hash)
            }
        }
        ConditionOpcode::AssertConcurrentSpend => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertConcurrentSpend",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertConcurrentSpend(puzzle_hash)
            }
        }
        ConditionOpcode::AssertConcurrentPuzzle => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertConcurrentPuzzle",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertConcurrentPuzzle(puzzle_hash)
            }
        }
        ConditionOpcode::SendMessage => {
            if args.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for SendMessage",
                ));
            }
            args.reverse();
            let mode_atom = args.pop().expect("Mode is always present - checked above");
            if mode_atom.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Mode for SendMessage",
                ));
            }
            let mode = mode_atom[0];
            if mode & 0b1100_0000 != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Mode for SendMessage",
                ));
            }
            let message = Message::new(
                args.pop()
                    .expect("Message is always present - checked above"),
            )?;
            let msg_args = message_args(send_message_mode(mode), args, "SendMessage")?;
            ConditionWithArgs::SendMessage(mode, message, msg_args)
        }
        ConditionOpcode::ReceiveMessage => {
            if args.len() < 2 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for ReceiveMessage",
                ));
            }
            args.reverse();
            let mode_atom = args.pop().expect("Mode is always present - checked above");
            if mode_atom.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Mode for ReceiveMessage",
                ));
            }
            let mode = mode_atom[0];
            if mode & 0b1100_0000 != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Mode for ReceiveMessage",
                ));
            }
            let message = Message::new(
                args.pop()
                    .expect("Message is always present - checked above"),
            )?;
            let msg_args = message_args(receive_message_mode(mode), args, "ReceiveMessage")?;
            ConditionWithArgs::ReceiveMessage(mode, message, msg_args)
        }
        ConditionOpcode::AssertMyCoinId => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertMyCoinId",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertMyCoinId(puzzle_hash)
            }
        }
        ConditionOpcode::AssertMyParentId => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertMyParentId",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertMyParentId(puzzle_hash)
            }
        }
        ConditionOpcode::AssertMyPuzzlehash => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertMyPuzzlehash",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::AssertMyPuzzlehash(puzzle_hash)
            }
        }
        ConditionOpcode::AssertMyAmount => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertMyAmount",
                ));
            } else {
                let amount_bytes = args.pop().unwrap_or_default();
                let amount = u64_from_bigint(&number_from_slice(&amount_bytes))?;
                ConditionWithArgs::AssertMyAmount(amount)
            }
        }
        ConditionOpcode::AssertMyBirthSeconds => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertMyBirthSeconds",
                ));
            } else {
                let seconds_bytes = args.pop().unwrap_or_default();
                let seconds = u64_from_bigint(&number_from_slice(&seconds_bytes))?;
                ConditionWithArgs::AssertMyBirthSeconds(seconds)
            }
        }
        ConditionOpcode::AssertMyBirthHeight => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertMyBirthHeight",
                ));
            } else {
                let height_bytes = args.pop().unwrap_or_default();
                let height = u32_from_slice(&height_bytes).ok_or(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Height for AssertMyBirthHeight",
                ))?;
                ConditionWithArgs::AssertMyBirthHeight(height)
            }
        }
        ConditionOpcode::AssertEphemeral => {
            if !args.is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertEphemeral",
                ));
            } else {
                ConditionWithArgs::AssertEphemeral
            }
        }
        ConditionOpcode::AssertSecondsRelative => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertSecondsRelative",
                ));
            } else {
                let seconds_bytes = args.pop().unwrap_or_default();
                let seconds = u64_from_bigint(&number_from_slice(&seconds_bytes))?;
                ConditionWithArgs::AssertSecondsRelative(seconds)
            }
        }
        ConditionOpcode::AssertSecondsAbsolute => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertSecondsAbsolute",
                ));
            } else {
                let seconds_bytes = args.pop().unwrap_or_default();
                let seconds = u64_from_bigint(&number_from_slice(&seconds_bytes))?;
                ConditionWithArgs::AssertSecondsAbsolute(seconds)
            }
        }
        ConditionOpcode::AssertHeightRelative => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertHeightRelative",
                ));
            } else {
                let height_bytes = args.pop().unwrap_or_default();
                let height = u32_from_slice(&height_bytes).ok_or(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Height for AssertMyBirthHeight",
                ))?;
                ConditionWithArgs::AssertHeightRelative(height)
            }
        }
        ConditionOpcode::AssertHeightAbsolute => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertHeightAbsolute",
                ));
            } else {
                let height_bytes = args.pop().unwrap_or_default();
                let height = u32_from_slice(&height_bytes).ok_or(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Height for AssertMyBirthHeight",
                ))?;
                ConditionWithArgs::AssertHeightAbsolute(height)
            }
        }
        ConditionOpcode::AssertBeforeSecondsRelative => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertBeforeSecondsRelative",
                ));
            } else {
                let seconds_bytes = args.pop().unwrap_or_default();
                let seconds = u64_from_bigint(&number_from_slice(&seconds_bytes))?;
                ConditionWithArgs::AssertBeforeSecondsRelative(seconds)
            }
        }
        ConditionOpcode::AssertBeforeSecondsAbsolute => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertBeforeSecondsAbsolute",
                ));
            } else {
                let seconds_bytes = args.pop().unwrap_or_default();
                let seconds = u64_from_bigint(&number_from_slice(&seconds_bytes))?;
                ConditionWithArgs::AssertBeforeSecondsAbsolute(seconds)
            }
        }
        ConditionOpcode::AssertBeforeHeightRelative => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertBeforeHeightRelative",
                ));
            } else {
                let height_bytes = args.pop().unwrap_or_default();
                let height = u32_from_slice(&height_bytes).ok_or(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Height for AssertMyBirthHeight",
                ))?;
                ConditionWithArgs::AssertBeforeHeightRelative(height)
            }
        }
        ConditionOpcode::AssertBeforeHeightAbsolute => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for AssertBeforeHeightAbsolute",
                ));
            } else {
                let height_bytes = args.pop().unwrap_or_default();
                let height = u32_from_slice(&height_bytes).ok_or(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Height for AssertMyBirthHeight",
                ))?;
                ConditionWithArgs::AssertBeforeHeightAbsolute(height)
            }
        }
        ConditionOpcode::SoftFork => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for SoftFork",
                ));
            } else {
                let cost_bytes = args.pop().unwrap_or_default();
                let cost = u64_from_bigint(&number_from_slice(&cost_bytes))?;
                ConditionWithArgs::SoftFork(cost * 10000)
            }
        }
    })
}

fn send_message_mode(mode: u8) -> MessageArgsType {
    let receiver_bits = mode & 0b111;
    message_mode(receiver_bits)
}

fn receive_message_mode(mode: u8) -> MessageArgsType {
    let sender_bits = (mode >> 3) & 0b111;
    message_mode(sender_bits)
}

fn message_mode(bits: u8) -> MessageArgsType {
    match bits & 0b111 {
        0b000 => MessageArgsType::None,         // none
        0b001 => MessageArgsType::Amount,       // amount
        0b010 => MessageArgsType::Puzzle,       // puzzle hash
        0b011 => MessageArgsType::PuzzleAmount, // puzzle hash + amount
        0b100 => MessageArgsType::Parent,       // parent
        0b101 => MessageArgsType::ParentAmount, // parent + amount
        0b110 => MessageArgsType::ParentPuzzle, // parent + puzzle hash
        0b111 => MessageArgsType::CoinId,       // coin id, NOT parent+puzzle+amount
        _ => unreachable!(),
    }
}

fn msg_args_sexp_ary(msg_args: &MessageArgs) -> Vec<SExp<'static>> {
    match msg_args {
        MessageArgs::None => vec![],
        MessageArgs::CoinId(coin_id) => vec![SExp::from(coin_id)],
        MessageArgs::Parent(parent) => vec![SExp::from(parent)],
        MessageArgs::Puzzle(puzzle_hash) => vec![SExp::from(puzzle_hash)],
        MessageArgs::Amount(amount) => vec![SExp::from(amount)],
        MessageArgs::ParentPuzzle {
            parent,
            puzzle_hash,
        } => vec![SExp::from(parent), SExp::from(puzzle_hash)],
        MessageArgs::ParentAmount { parent, amount } => {
            vec![SExp::from(parent), SExp::from(amount)]
        }
        MessageArgs::PuzzleAmount {
            puzzle_hash,
            amount,
        } => vec![SExp::from(puzzle_hash), SExp::from(amount)],
    }
}
fn message_args_bytes(
    msg_args: &MessageArgs,
    version: ChiaProtocolVersion,
) -> Result<Vec<Vec<u8>>, Error> {
    Ok(match msg_args {
        MessageArgs::None => vec![],
        MessageArgs::CoinId(coin_id) => vec![ChiaSerialize::to_bytes(coin_id, version)?],
        MessageArgs::Parent(parent) => vec![ChiaSerialize::to_bytes(parent, version)?],
        MessageArgs::Puzzle(puzzle_hash) => vec![ChiaSerialize::to_bytes(puzzle_hash, version)?],
        MessageArgs::Amount(amount) => vec![ChiaSerialize::to_bytes(amount, version)?],
        MessageArgs::ParentPuzzle {
            parent,
            puzzle_hash,
        } => vec![
            ChiaSerialize::to_bytes(parent, version)?,
            ChiaSerialize::to_bytes(puzzle_hash, version)?,
        ],
        MessageArgs::ParentAmount { parent, amount } => vec![
            ChiaSerialize::to_bytes(parent, version)?,
            ChiaSerialize::to_bytes(amount, version)?,
        ],
        MessageArgs::PuzzleAmount {
            puzzle_hash,
            amount,
        } => vec![
            ChiaSerialize::to_bytes(puzzle_hash, version)?,
            ChiaSerialize::to_bytes(amount, version)?,
        ],
    })
}

fn message_args(
    mode: MessageArgsType,
    mut args: Vec<Vec<u8>>,
    send_or_recieve: &'static str,
) -> Result<MessageArgs, ClvmError> {
    Ok(match mode {
        MessageArgsType::None => MessageArgs::None,
        MessageArgsType::CoinId => {
            let coin_id = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            MessageArgs::CoinId(coin_id)
        }
        MessageArgsType::Parent => {
            let parent = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            MessageArgs::Parent(parent)
        }
        MessageArgsType::Puzzle => {
            let puzzle = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            MessageArgs::Puzzle(puzzle)
        }
        MessageArgsType::Amount => {
            let amount_vec = args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?;
            let amount = u64_from_bigint(&number_from_slice(&amount_vec))?;
            MessageArgs::Amount(amount)
        }
        MessageArgsType::ParentPuzzle => {
            let parent = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            let puzzle_hash = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            MessageArgs::ParentPuzzle {
                parent,
                puzzle_hash,
            }
        }
        MessageArgsType::ParentAmount => {
            let parent = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            let amount_vec = args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?;
            let amount = u64_from_bigint(&number_from_slice(&amount_vec))?;
            MessageArgs::ParentAmount { parent, amount }
        }
        MessageArgsType::PuzzleAmount => {
            let puzzle_hash = Bytes32::from(args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?);
            let amount_vec = args.pop().ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Too Few Vars for {send_or_recieve}"),
                )
            })?;
            let amount = u64_from_bigint(&number_from_slice(&amount_vec))?;
            MessageArgs::PuzzleAmount {
                puzzle_hash,
                amount,
            }
        }
    })
}

impl TryFrom<&SExp<'_>> for Vec<ConditionWithArgs> {
    type Error = ClvmError;
    fn try_from(sexp: &SExp) -> Result<Self, Self::Error> {
        let mut results = Vec::new();
        for arg in sexp.iter() {
            let arg: Result<ConditionWithArgs, ClvmError> = arg.try_into();
            results.push(arg?);
        }
        Ok(results)
    }
}

pub fn op_code_with_args_from_sexp(sexp: &SExp) -> Result<(ConditionOpcode, Vec<Vec<u8>>), Error> {
    let mut opcode = ConditionOpcode::Unknown;
    let mut vars = vec![];
    let mut first = true;
    for (index, arg) in sexp.iter().enumerate() {
        match arg {
            SExp::Atom(arg) => {
                if first {
                    first = false;
                    if arg.as_ref().len() != 1 {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            "Invalid OpCode for Condition",
                        ));
                    }
                    opcode = ConditionOpcode::from(arg.as_ref()[0]);
                } else {
                    vars.push(arg.as_ref().to_vec());
                }
            }
            SExp::Pair(_pairbuf) => {
                if opcode == ConditionOpcode::Remark
                    || (index == 3 && opcode == ConditionOpcode::CreateCoin)
                {
                    vars.push(sexp_to_bytes(arg)?.as_ref().to_vec());
                } else {
                    warn!("Got pair in opcode({opcode}) args: {arg:?}");
                    break;
                }
            }
        }
    }
    if vars.is_empty() {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("Invalid Condition {opcode} No Vars: - {sexp:#?}"),
        ))
    } else {
        Ok((opcode, vars))
    }
}

fn memos_to_sexp(memos: &[Vec<u8>]) -> SExp<'static> {
    Program::to(
        memos
            .iter()
            .map(|memo| SExp::Atom(AtomBuf::new(memo.clone())))
            .collect::<Vec<_>>(),
    )
    .sexp()
    .to_owned()
}

fn memos_from_bytes(blob: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut cursor = Cursor::new(blob);
    let sexp = sexp_from_bytes(&mut cursor)
        .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
    if cursor.position() != blob.len() as u64 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid memo list encoding",
        ));
    }
    memos_from_sexp(&sexp)
}

fn memos_from_sexp(sexp: &SExp<'_>) -> Result<Vec<Vec<u8>>, Error> {
    let mut memos = Vec::new();
    for memo in Program::new_ref(sexp).as_list() {
        let atom = memo.sexp().atom().map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "CreateCoin memos must be a list of atoms",
            )
        })?;
        memos.push(atom.as_ref().to_vec());
    }
    Ok(memos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_message_round_trips_with_flat_receiver_args() {
        let parent = Bytes32::from([1u8; 32].to_vec());
        let puzzle_hash = Bytes32::from([2u8; 32].to_vec());
        let condition = ConditionWithArgs::SendMessage(
            0b00_000_110,
            Message::new(vec![0xaa, 0xbb]).unwrap(),
            MessageArgs::ParentPuzzle {
                parent,
                puzzle_hash,
            },
        );

        let sexp = SExp::from(&condition);
        let reparsed = ConditionWithArgs::try_from(&sexp).unwrap();

        assert_eq!(reparsed, condition);
    }

    #[test]
    fn receive_message_round_trips_with_message_before_sender_args() {
        let parent = Bytes32::from([3u8; 32].to_vec());
        let amount = 42u64;
        let condition = ConditionWithArgs::ReceiveMessage(
            0b00_101_000,
            Message::new(vec![0xcc]).unwrap(),
            MessageArgs::ParentAmount { parent, amount },
        );

        let sexp = SExp::from(&condition);
        let reparsed = ConditionWithArgs::try_from(&sexp).unwrap();

        assert_eq!(reparsed, condition);
    }

    #[test]
    fn create_coin_op_code_wraps_memos_in_list() {
        let puzzle_hash_bytes = [4u8; 32].to_vec();
        let puzzle_hash = Bytes32::from(puzzle_hash_bytes.clone());

        let condition_no_memo = ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![]);
        println!("condition_no_memo: {}", condition_no_memo);

        let condition =
            ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa], vec![0xbb, 0xcc]]);
        println!("condition_with_memo: {}", condition);

        let (opcode, vars) = condition.op_code_with_args();
        assert_eq!(opcode, ConditionOpcode::CreateCoin);
        assert_eq!(vars.len(), 3);
        assert_eq!(
            vars[0].atom().unwrap().as_ref(),
            puzzle_hash_bytes.as_slice()
        );
        assert_eq!(vars[1].atom().unwrap().as_ref(), &[123]);
        let memo_program = Program::new_ref(&vars[2]);
        let memos = memo_program
            .as_list()
            .into_iter()
            .map(|memo| memo.as_vec().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(memos, vec![vec![0xaa], vec![0xbb, 0xcc]]);
    }

    #[test]
    fn create_coin_round_trips_with_nested_memos() {
        let puzzle_hash = Bytes32::from([5u8; 32].to_vec());
        let sexp = Program::to(vec![
            SExp::from(ConditionOpcode::CreateCoin),
            SExp::from(puzzle_hash),
            SExp::from(123u64),
            Program::to(vec![
                SExp::Atom(AtomBuf::new(vec![0xaa])),
                SExp::Atom(AtomBuf::new(vec![0xbb, 0xcc])),
            ])
            .sexp()
            .to_owned(),
        ]);

        let condition = ConditionWithArgs::try_from(sexp.sexp()).unwrap();
        assert_eq!(
            condition,
            ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa], vec![0xbb, 0xcc]])
        );
    }

    #[test]
    fn create_coin_emits_explicit_empty_memo_list() {
        let puzzle_hash = Bytes32::from([6u8; 32].to_vec());
        let condition = ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![]);
        let (_, vars) = condition.op_code_with_args();

        assert_eq!(vars.len(), 3);
        assert!(!vars[2].non_nil());
    }

    #[test]
    fn create_coin_rejects_flattened_memos() {
        let puzzle_hash = Bytes32::from([7u8; 32].to_vec());
        let sexp = Program::to(vec![
            SExp::from(ConditionOpcode::CreateCoin),
            SExp::from(puzzle_hash),
            SExp::from(123u64),
            SExp::Atom(AtomBuf::new(vec![0xaa])),
            SExp::Atom(AtomBuf::new(vec![0xbb, 0xcc])),
        ]);

        assert_eq!(
            ConditionWithArgs::try_from(sexp.sexp()).unwrap(),
            ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa], vec![0xbb, 0xcc]])
        );
    }

    #[test]
    fn create_coin_accepts_single_flattened_memo_atom() {
        let puzzle_hash = Bytes32::from([8u8; 32].to_vec());
        let sexp = Program::to(vec![
            SExp::from(ConditionOpcode::CreateCoin),
            SExp::from(puzzle_hash),
            SExp::from(123u64),
            SExp::Atom(AtomBuf::new(vec![0xaa])),
        ]);

        assert_eq!(
            ConditionWithArgs::try_from(sexp.sexp()).unwrap(),
            ConditionWithArgs::CreateCoin(puzzle_hash, 123u64, vec![vec![0xaa]])
        );
    }

    #[test]
    fn send_message_with_none_does_not_emit_placeholder_arg() {
        let condition =
            ConditionWithArgs::SendMessage(0, Message::new(vec![0xdd]).unwrap(), MessageArgs::None);

        let (_, args) = op_code_with_args_from_sexp(&SExp::from(&condition)).unwrap();

        assert_eq!(args.len(), 2);
        assert_eq!(args[0], Vec::<u8>::new());
        assert_eq!(args[1], vec![0xdd]);
    }

    #[test]
    fn message_modes_reject_reserved_high_bits() {
        let condition = [
            SExp::from(66u8),
            SExp::from(0b0100_0000u8),
            SExp::from(vec![0x01]),
        ]
        .as_slice()
        .into();

        assert!(ConditionWithArgs::try_from(&condition).is_err());
    }
}
