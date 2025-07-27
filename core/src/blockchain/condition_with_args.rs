use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::sexp::{AtomBuf, IntoSExp, SExp};
use crate::constants::NULL_SEXP;
use crate::formatting::{number_from_slice, u32_from_slice, u64_from_bigint};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::warn;
use serde::de::Error as SerialError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Error, ErrorKind};

#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Message(usize, [u8; 1024]);
impl Message {
    pub fn new(msg: Vec<u8>) -> Result<Message, Error> {
        if msg.len() > 1024 {
            Err(Error::new(ErrorKind::InvalidInput, "Message too long"))
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

impl IntoSExp for Message {
    fn to_sexp(self) -> SExp {
        SExp::Atom(AtomBuf::new(self.as_ref().to_vec()))
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

    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let vec_data: Vec<u8> = Vec::from_bytes(bytes, version)?;
        Message::new(vec_data)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    CreateCoin(Bytes32, u64, Option<Bytes32>),
    ReserveFee(u64),
    CreateCoinAnnouncement(Message),
    AssertCoinAnnouncement(Bytes32),
    CreatePuzzleAnnouncement(Message),
    AssertPuzzleAnnouncement(Bytes32),
    AssertConcurrentSpend(Bytes32),
    AssertConcurrentPuzzle(Bytes32),
    SendMessage(u8, Bytes32, Message),
    ReceiveMessage(u8, Bytes32, Message),
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
impl TryFrom<&SExp> for ConditionWithArgs {
    type Error = Error;
    fn try_from(sexp: &SExp) -> Result<Self, Self::Error> {
        let (op_code, args) = op_code_with_args_from_sexp(sexp)?;
        from_opcode_with_args(op_code, args)
    }
}
impl Display for ConditionWithArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (op_code, vars) = self.clone().op_code_with_args();
        write!(f, "{op_code} ")?;
        for var in &vars {
            write!(f, "{var} ")?;
        }
        Ok(())
    }
}

impl IntoSExp for &ConditionWithArgs {
    fn to_sexp(self) -> SExp {
        let mut as_sexp = NULL_SEXP.clone();
        let (op_code, vars) = self.op_code_with_args();
        for var in vars.into_iter().rev() {
            as_sexp = var.cons(as_sexp)
        }
        op_code.to_sexp().cons(as_sexp)
    }
}
impl IntoSExp for ConditionWithArgs {
    fn to_sexp(self) -> SExp {
        (&self).to_sexp()
    }
}

impl ConditionWithArgs {
    pub fn op_code_with_args(&self) -> (ConditionOpcode, Vec<SExp>) {
        match *self {
            ConditionWithArgs::Unknown => (ConditionOpcode::Unknown, vec![]),
            ConditionWithArgs::Remark(msg) => (ConditionOpcode::Remark, vec![msg.to_sexp()]),
            ConditionWithArgs::AggSigParent(key, msg) => (
                ConditionOpcode::AggSigParent,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigPuzzle(key, msg) => (
                ConditionOpcode::AggSigPuzzle,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigAmount(key, msg) => (
                ConditionOpcode::AggSigAmount,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigPuzzleAmount(key, msg) => (
                ConditionOpcode::AggSigPuzzleAmount,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigParentAmount(key, msg) => (
                ConditionOpcode::AggSigParentAmount,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigParentPuzzle(key, msg) => (
                ConditionOpcode::AggSigParentPuzzle,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigUnsafe(key, msg) => (
                ConditionOpcode::AggSigUnsafe,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AggSigMe(key, msg) => (
                ConditionOpcode::AggSigMe,
                vec![key.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::CreateCoin(puzzle_hash, amount, hint) => (
                ConditionOpcode::CreateCoin,
                vec![puzzle_hash.to_sexp(), amount.to_sexp(), hint.to_sexp()],
            ),
            ConditionWithArgs::ReserveFee(fee) => {
                (ConditionOpcode::ReserveFee, vec![fee.to_sexp()])
            }
            ConditionWithArgs::CreateCoinAnnouncement(puzzle_hash) => (
                ConditionOpcode::CreateCoinAnnouncement,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::AssertCoinAnnouncement(puzzle_hash) => (
                ConditionOpcode::AssertCoinAnnouncement,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::CreatePuzzleAnnouncement(puzzle_hash) => (
                ConditionOpcode::CreatePuzzleAnnouncement,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::AssertPuzzleAnnouncement(puzzle_hash) => (
                ConditionOpcode::AssertPuzzleAnnouncement,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::AssertConcurrentSpend(puzzle_hash) => (
                ConditionOpcode::AssertConcurrentSpend,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::AssertConcurrentPuzzle(puzzle_hash) => (
                ConditionOpcode::AssertConcurrentPuzzle,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::SendMessage(mode, puzzle_hash, msg) => (
                ConditionOpcode::SendMessage,
                vec![mode.to_sexp(), puzzle_hash.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::ReceiveMessage(puzzle_hash, mode, msg) => (
                ConditionOpcode::ReceiveMessage,
                vec![puzzle_hash.to_sexp(), mode.to_sexp(), msg.to_sexp()],
            ),
            ConditionWithArgs::AssertMyCoinId(puzzle_hash) => {
                (ConditionOpcode::AssertMyCoinId, vec![puzzle_hash.to_sexp()])
            }
            ConditionWithArgs::AssertMyParentId(puzzle_hash) => (
                ConditionOpcode::AssertMyParentId,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::AssertMyPuzzlehash(puzzle_hash) => (
                ConditionOpcode::AssertMyPuzzlehash,
                vec![puzzle_hash.to_sexp()],
            ),
            ConditionWithArgs::AssertMyAmount(amount) => {
                (ConditionOpcode::AssertMyAmount, vec![amount.to_sexp()])
            }
            ConditionWithArgs::AssertMyBirthSeconds(seconds) => (
                ConditionOpcode::AssertMyBirthSeconds,
                vec![seconds.to_sexp()],
            ),
            ConditionWithArgs::AssertMyBirthHeight(height) => {
                (ConditionOpcode::AssertMyBirthHeight, vec![height.to_sexp()])
            }
            ConditionWithArgs::AssertEphemeral => (ConditionOpcode::AssertEphemeral, vec![]),
            ConditionWithArgs::AssertSecondsRelative(seconds) => (
                ConditionOpcode::AssertSecondsRelative,
                vec![seconds.to_sexp()],
            ),
            ConditionWithArgs::AssertSecondsAbsolute(seconds) => (
                ConditionOpcode::AssertSecondsAbsolute,
                vec![seconds.to_sexp()],
            ),
            ConditionWithArgs::AssertHeightRelative(height) => (
                ConditionOpcode::AssertHeightRelative,
                vec![height.to_sexp()],
            ),
            ConditionWithArgs::AssertHeightAbsolute(height) => (
                ConditionOpcode::AssertHeightAbsolute,
                vec![height.to_sexp()],
            ),
            ConditionWithArgs::AssertBeforeSecondsRelative(seconds) => (
                ConditionOpcode::AssertBeforeSecondsRelative,
                vec![seconds.to_sexp()],
            ),
            ConditionWithArgs::AssertBeforeSecondsAbsolute(seconds) => (
                ConditionOpcode::AssertBeforeSecondsAbsolute,
                vec![seconds.to_sexp()],
            ),
            ConditionWithArgs::AssertBeforeHeightRelative(height) => (
                ConditionOpcode::AssertBeforeHeightRelative,
                vec![height.to_sexp()],
            ),
            ConditionWithArgs::AssertBeforeHeightAbsolute(height) => (
                ConditionOpcode::AssertBeforeHeightAbsolute,
                vec![height.to_sexp()],
            ),
            ConditionWithArgs::SoftFork(cost) => (ConditionOpcode::SoftFork, vec![cost.to_sexp()]),
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
            ConditionWithArgs::CreateCoin(puzzle_hash, amount, hint) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::CreateCoin,
                    version,
                )?);
                let mut vars = vec![
                    ChiaSerialize::to_bytes(puzzle_hash, version)?,
                    ChiaSerialize::to_bytes(amount, version)?,
                ];
                if let Some(hint) = hint {
                    vars.push(ChiaSerialize::to_bytes(hint, version)?);
                }
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
            ConditionWithArgs::SendMessage(mode, puzzle_hash, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::SendMessage,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(mode, version)?,
                    ChiaSerialize::to_bytes(puzzle_hash, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
                bytes.extend(ChiaSerialize::to_bytes(&vars, version)?);
                Ok(bytes)
            }
            ConditionWithArgs::ReceiveMessage(puzzle_hash, mode, msg) => {
                let mut bytes = vec![];
                bytes.extend(ChiaSerialize::to_bytes(
                    &ConditionOpcode::ReceiveMessage,
                    version,
                )?);
                let vars = vec![
                    ChiaSerialize::to_bytes(puzzle_hash, version)?,
                    ChiaSerialize::to_bytes(mode, version)?,
                    ChiaSerialize::to_bytes(msg, version)?,
                ];
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

    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
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
                let hint = if args.len() > 2 {
                    Some(Bytes32::from(args.pop().unwrap_or_default()))
                } else {
                    None
                };
                let amount_bytes = args.pop().unwrap_or_default();
                let amount = u64_from_bigint(&number_from_slice(&amount_bytes))?;
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                ConditionWithArgs::CreateCoin(puzzle_hash, amount, hint)
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
            if args.len() != 3 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for SendMessage",
                ));
            } else {
                let message = Message::new(args.pop().unwrap_or_default())?;
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                let mode = args.pop().unwrap_or_default();
                if mode.len() != 1 {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "Invalid Mode for SendMessage",
                    ));
                }
                let mode = mode[0];
                ConditionWithArgs::SendMessage(mode, puzzle_hash, message)
            }
        }
        ConditionOpcode::ReceiveMessage => {
            if args.len() != 3 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Invalid Vars for ReceiveMessage",
                ));
            } else {
                let puzzle_hash = Bytes32::from(args.pop().unwrap_or_default());
                let message = Message::new(args.pop().unwrap_or_default())?;
                let mode = args.pop().unwrap_or_default();
                if mode.len() != 1 {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "Invalid Mode for ReceiveMessage",
                    ));
                }
                let mode = mode[0];
                ConditionWithArgs::ReceiveMessage(mode, puzzle_hash, message)
            }
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

impl TryFrom<&SExp> for Vec<ConditionWithArgs> {
    type Error = Error;
    fn try_from(sexp: &SExp) -> Result<Self, Self::Error> {
        let mut results = Vec::new();
        for arg in sexp.iter() {
            let arg: Result<ConditionWithArgs, Error> = arg.try_into();
            match arg {
                Ok(condition) => {
                    results.push(condition);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(results)
    }
}

pub fn op_code_with_args_from_sexp(sexp: &SExp) -> Result<(ConditionOpcode, Vec<Vec<u8>>), Error> {
    let mut opcode = ConditionOpcode::Unknown;
    let mut vars = vec![];
    let mut first = true;
    for arg in sexp.iter().take(4) {
        match arg {
            SExp::Atom(arg) => {
                if first {
                    first = false;
                    if arg.data.len() != 1 {
                        return Err(Error::new(
                            ErrorKind::InvalidData,
                            "Invalid OpCode for Condition",
                        ));
                    }
                    opcode = ConditionOpcode::from(arg.data[0]);
                } else {
                    vars.push(arg.data.clone());
                }
            }
            SExp::Pair(_) => {
                if opcode == ConditionOpcode::Remark {
                    vars.push(sexp_to_bytes(arg)?);
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
            "Invalid Condition No Vars",
        ))
    } else {
        Ok((opcode, vars))
    }
}
