use crate::blockchain::sized_bytes::{
    Bytes4, Bytes8, Bytes32, Bytes48, Bytes96, Bytes100, Bytes480,
};
use crate::clvm::assemble::assemble_text;
use crate::clvm::curry_utils::curry;
use crate::clvm::dialect::NO_UNKNOWN_OPS;
use crate::clvm::parser::{sexp_from_bytes, sexp_from_bytes_backrefs, sexp_to_bytes};
use crate::clvm::runtime::ClvmRuntime;
use crate::clvm::sexp::AtomBuf;
use crate::clvm::sexp::{SExp, SExpSource};
use crate::clvm::utils::MEMPOOL_MODE;
use crate::constants::NULL_PROGRAM;
use crate::errors::ClvmError;
use crate::formatting::hex_to_bytes;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hex::encode;
use log::warn;
use num_bigint::BigInt;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::hash::Hasher;
use std::io::{Cursor, Error};
use std::path::Path;

pub struct Program<'a> {
    sexp: SExpSource<'a>,
}
impl Program<'static> {
    pub const fn new_const(sexp: SExp<'static>) -> Self {
        Program {
            sexp: SExpSource::Owned(sexp),
        }
    }
    pub const fn sexp_const(&'static self) -> &'static SExp<'static> {
        match self.sexp {
            SExpSource::Owned(ref sexp) => sexp,
            SExpSource::Borrowed(sexp) => sexp,
        }
    }
    pub const fn new_static(sexp: &'static SExp) -> Self {
        Program {
            sexp: SExpSource::Borrowed(sexp),
        }
    }
    pub async fn from_file(path: &Path) -> Result<Program<'static>, ClvmError> {
        if path.ends_with("bin") {
            let serial_program = SerializedProgram::from_bytes(
                &tokio::fs::read(path).await.map_err(ClvmError::IoError)?,
            );
            Ok(Program::from_serial(&serial_program)?.to_owned())
        } else if path.ends_with("hex") {
            let serial_program = SerializedProgram::from_hex(
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(ClvmError::IoError)?
                    .trim(),
            )?;
            Ok(Program::from_serial(&serial_program)?.to_owned())
        } else if path.ends_with("clvm") {
            assemble_text(
                tokio::fs::read_to_string(&path)
                    .await
                    .map_err(ClvmError::IoError)?
                    .trim(),
            )
        } else {
            Err(ClvmError::InvalidInput(format!(
                "Invalid File type, Expected Hex or Bin: {path:?}"
            )))
        }
    }
}

impl<'a> Program<'a> {
    pub fn new(sexp: SExp<'a>) -> Self {
        Program {
            sexp: SExpSource::Owned(sexp),
        }
    }
    pub fn to<T: Into<SExp<'a>>>(vals: T) -> Self {
        Program::new(vals.into())
    }
    pub fn from_serial(serial: &'a SerializedProgram) -> Result<Self, ClvmError> {
        let mut cursor = Cursor::new(serial.buffer.as_ref());
        Ok(Self::new(sexp_from_bytes(&mut cursor)?))
    }
    pub fn from_serial_backrefs(serial: &'a SerializedProgram) -> Result<Self, ClvmError> {
        let mut cursor = Cursor::new(serial.buffer.as_ref());
        Ok(Self::new(sexp_from_bytes_backrefs(&mut cursor)?))
    }
    pub fn new_ref(sexp: &'a SExp) -> Program<'a> {
        Program {
            sexp: SExpSource::Borrowed(sexp),
        }
    }
    pub fn to_owned(&self) -> Program<'static> {
        Program::new(self.sexp.to_owned())
    }
    pub fn sexp(&'a self) -> &'a SExp<'a> {
        &self.sexp
    }
    pub fn serialized(&self) -> Result<SerializedProgram, ClvmError> {
        sexp_to_bytes(&self.sexp).map_err(ClvmError::IoError)
    }
    pub fn first(&'a self) -> Result<Program<'a>, ClvmError> {
        Ok(Program::new_ref(self.sexp.first()?))
    }
    pub fn rest(&'a self) -> Result<Program<'a>, ClvmError> {
        Ok(Program::new_ref(self.sexp.rest()?))
    }
    pub fn at(&'a self, path: &str) -> Result<Program<'a>, ClvmError> {
        let mut rtn = self.sexp();
        for c in path.chars() {
            if c == 'f' || c == 'F' {
                rtn = rtn.first()?;
            } else if c == 'r' || c == 'R' {
                rtn = rtn.rest()?;
            } else {
                return Err(ClvmError::InvalidInput(format!(
                    "`at` got illegal character `{c}`. Only `f` & `r` allowed"
                )));
            }
        }
        Ok(Program::new_ref(rtn))
    }
    #[must_use]
    pub fn tree_hash(&self) -> Bytes32 {
        self.sexp.tree_hash()
    }
    pub fn curry(&'_ self, args: &[Program<'_>]) -> Program<'static> {
        curry(self, args)
    }

    pub fn uncurry(&'a self) -> Result<(Program<'a>, Program<'a>), ClvmError> {
        fn inner_match(o: &SExp, expected: &[u8]) -> Result<(), ClvmError> {
            if o.atom()? == *expected {
                Ok(())
            } else {
                Err(ClvmError::InvalidInput(format!(
                    "expected: {}",
                    encode(expected)
                )))
            }
        }
        //(2 (1 . <mod>) <args>)
        let as_list = self.sexp().ref_list();
        inner_match(as_list[0] /*ev*/, b"\x02")?;
        let q_pair = as_list[1].pair()?;
        inner_match(q_pair.first(), b"\x01")?;
        let mut args = vec![];
        let mut args_list = as_list[2];
        while let SExp::Pair(_) = args_list {
            //(4(1. < arg >) < rest >)
            let as_list = args_list.ref_list();
            inner_match(as_list[0], b"\x04")?;
            let q_pair = as_list[1].pair()?;
            inner_match(q_pair.first(), b"\x01")?;
            args.push(q_pair.rest());
            args_list = as_list[2];
        }
        inner_match(args_list, b"\x01")?;
        Ok((
            Program::new_ref(q_pair.rest()),
            Program::to(args.as_slice()),
        ))
    }

    #[must_use]
    pub fn as_list(&'a self) -> Vec<Program<'a>> {
        let mut args = vec![];
        let mut args_sexp = self.sexp();
        loop {
            match args_sexp {
                SExp::Atom(_) => {
                    if args_sexp.non_nil() {
                        args.push(Program::new_ref(args_sexp));
                    }
                    return args;
                }
                SExp::Pair(buf) => {
                    args.push(Program::new_ref(buf.first()));
                    args_sexp = buf.rest();
                }
            }
        }
    }

    pub fn to_map(&'a self) -> Result<HashMap<Program<'a>, Program<'a>>, ClvmError> {
        Ok(self
            .sexp
            .to_map()?
            .into_iter()
            .map(|m| (Program::new_ref(m.0), Program::new_ref(m.1)))
            .collect())
    }

    #[must_use]
    pub fn is_atom(&self) -> bool {
        matches!(&*self.sexp, SExp::Atom(_))
    }

    #[must_use]
    pub fn is_pair(&self) -> bool {
        matches!(&*self.sexp, SExp::Pair(_))
    }

    #[must_use]
    pub fn as_atom(&'a self) -> Option<Program<'a>> {
        match &*self.sexp {
            SExp::Atom(_) => Some(Program::new_ref(self.sexp())),
            SExp::Pair(_) => None,
        }
    }

    #[must_use]
    pub fn as_vec(&self) -> Option<Vec<u8>> {
        self.sexp.as_vec()
    }

    #[must_use]
    pub fn as_pair(&'a self) -> Option<(Program<'a>, Program<'a>)> {
        Some((self.first().ok()?, self.rest().ok()?))
    }
    pub fn as_int(&self) -> Result<BigInt, ClvmError> {
        match &self.as_atom() {
            Some(atom) => Ok(BigInt::from_signed_bytes_be(
                atom.as_vec()
                    .ok_or_else(|| {
                        ClvmError::InvalidInput("Failed to convert Program to Atom".to_string())
                    })?
                    .as_slice(),
            )),
            None => Err(ClvmError::ExpectedAtomGotPair(
                "Program is Pair not Atom".to_string(),
            )),
        }
    }

    pub fn run_mempool_with_cost(
        &'a self,
        max_cost: u64,
        args: &'a Program,
    ) -> Result<(u64, Program<'a>), ClvmError> {
        self.run(max_cost, MEMPOOL_MODE, args)
    }

    pub fn run_with_cost(
        &'a self,
        max_cost: u64,
        args: &'a Program,
    ) -> Result<(u64, Program<'a>), ClvmError> {
        self.run(max_cost, 0, args)
    }

    pub fn run(
        &'a self,
        max_cost: u64,
        flags: u32,
        args: &'_ Program,
    ) -> Result<(u64, Program<'static>), ClvmError> {
        let mut runtime = ClvmRuntime::new(max_cost, flags | NO_UNKNOWN_OPS);
        let (cost, result) = runtime.run(self.sexp(), args.sexp())?;
        Ok((cost, Program::new(result.to_owned())))
    }
}
impl<'a> Eq for Program<'a> {}
impl<'a> PartialEq for Program<'a> {
    fn eq(&self, other: &Self) -> bool {
        *self.sexp == *other.sexp
    }
}

impl<'a> ChiaSerialize for Program<'a> {
    fn to_bytes(&self, _: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        Ok(self.serialized()?.as_ref().to_vec())
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, _: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let sexp = sexp_from_bytes(bytes)?;
        Ok(Program::new(sexp.to_owned()))
    }
}

impl<'a> Display for Program<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", *self.sexp)
    }
}

impl<'a> Debug for Program<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", *self.sexp)
    }
}

impl<'a> TryFrom<Vec<u8>> for Program<'a> {
    type Error = ClvmError;
    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        (&bytes).try_into()
    }
}

impl<'a> TryFrom<&Vec<u8>> for Program<'a> {
    type Error = ClvmError;

    fn try_from(bytes: &Vec<u8>) -> Result<Self, Self::Error> {
        let atom = SExp::Atom(AtomBuf::from(bytes));
        Ok(Program {
            sexp: SExpSource::Owned(atom),
        })
    }
}

impl<'a> TryFrom<&'a [u8]> for Program<'a> {
    type Error = ClvmError;
    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        let atom = SExp::Atom(AtomBuf::from(bytes));
        Ok(Program {
            sexp: SExpSource::Owned(atom),
        })
    }
}

impl Clone for Program<'static> {
    fn clone(&'_ self) -> Program<'static> {
        Program::new(self.sexp().to_owned())
    }
}

impl<'a> Hash for Program<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.sexp.hash(state);
    }
}

impl<'a> Default for Program<'a> {
    fn default() -> Self {
        Self {
            sexp: SExpSource::Owned(Default::default()),
        }
    }
}

macro_rules! impl_sized_bytes {
    ($($name: ident);*) => {
        $(
            impl<'a> From<$name> for Program<'a> {
                fn from(bytes: $name) -> Self {
                    Program::to(bytes)
                }
            }
        )*
    };
    ()=>{};
}

impl_sized_bytes!(
    Bytes4;
    Bytes8;
    Bytes32;
    Bytes48;
    Bytes96;
    Bytes100;
    Bytes480
);

macro_rules! impl_ints {
    ($($name: ident, $size: expr);*) => {
        $(
            impl<'a> TryFrom<$name> for Program<'a> {
                type Error = ClvmError;
                fn try_from(int_val: $name) -> Result<Self, Self::Error> {
                    if int_val == 0 {
                        return Ok(NULL_PROGRAM);
                    }
                    let as_ary = int_val.to_be_bytes();
                    let mut as_bytes = as_ary.as_slice();
                    while as_bytes.len() > 1 && as_bytes[0] == ( if as_bytes[1] & 0x80 > 0{0xFF} else {0}) {
                        as_bytes = &as_bytes[1..];
                    }
                    as_bytes.to_vec().try_into()
                }
            }
            impl<'a> TryInto<$name> for &Program<'a> {
                type Error = ClvmError;

                fn try_into(self) -> Result<$name, Self::Error> {
                    let as_atom = self.as_vec().ok_or(ClvmError::InvalidInput("Invalid program for $name".to_string()))?;
                    if as_atom.len() > $size {
                        return Err(ClvmError::InvalidInput("Invalid program for $name".to_string()));
                    }
                    Ok($name::from_le_bytes(as_atom.as_slice().try_into().map_err(|e| ClvmError::InvalidInput(format!("Invalid program for $name: {:?}", e)))?))
                }
            }
            impl<'a> TryInto<$name> for Program<'a> {
                type Error = ClvmError;
                fn try_into(self) -> Result<$name, Self::Error> {
                    (&self).try_into()
                }
            }
        )*
    };
    ()=>{};
}

impl_ints!(
    u8, 1;
    u16, 2;
    u32, 4;
    u64, 8;
    u128, 16;
    i8, 1;
    i16, 2;
    i32, 4;
    i64, 8;
    i128, 16
);

#[derive(Clone, PartialEq, Eq)]
pub enum SerializedSource {
    Static(&'static [u8]),
    Heap(Vec<u8>),
}
impl<'a> From<&'a SerializedSource> for Cursor<&'a [u8]> {
    fn from(value: &'a SerializedSource) -> Self {
        Cursor::new(value.as_ref())
    }
}
impl AsRef<[u8]> for SerializedSource {
    fn as_ref(&self) -> &[u8] {
        match self {
            SerializedSource::Static(buffer) => buffer,
            SerializedSource::Heap(buffer) => buffer.as_slice(),
        }
    }
}
impl From<Vec<u8>> for SerializedProgram {
    fn from(value: Vec<u8>) -> Self {
        Self {
            buffer: SerializedSource::Heap(value),
        }
    }
}
impl Hash for SerializedProgram {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let as_bytes: &[u8] = self.as_ref();
        as_bytes.hash(state);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SerializedProgram {
    buffer: SerializedSource,
}
impl Default for SerializedProgram {
    fn default() -> Self {
        SerializedProgram {
            buffer: SerializedSource::Static(&[]),
        }
    }
}
impl From<SerializedProgram> for SExp<'static> {
    fn from(value: SerializedProgram) -> Self {
        SExp::from(&value)
    }
}
impl From<&SerializedProgram> for SExp<'static> {
    fn from(value: &SerializedProgram) -> Self {
        let mut cursor = Cursor::new(value.as_ref());
        match sexp_from_bytes(&mut cursor) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    "Failed to Parse SerializedProgram as SExp, Saving as AtomBuf. This is likely an Error: {e:?}"
                );
                match &value.buffer {
                    SerializedSource::Static(v) => SExp::Atom(AtomBuf::new(v.to_vec())),
                    SerializedSource::Heap(v) => SExp::Atom(AtomBuf::new(v.clone())),
                }
            }
        }
    }
}

impl SerializedProgram {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> SerializedProgram {
        SerializedProgram {
            buffer: SerializedSource::Heap(bytes.to_owned()),
        }
    }
    pub fn from_hex(hex_str: &str) -> Result<SerializedProgram, ClvmError> {
        Ok(SerializedProgram {
            buffer: SerializedSource::Heap(hex_to_bytes(hex_str.trim()).map_err(|_| {
                ClvmError::InvalidHex(format!("Failed to convert {hex_str} to SerializedProgram"))
            })?),
        })
    }
    pub fn to_program_backrefs(&self) -> Result<Program<'static>, ClvmError> {
        Ok(Program::from_serial_backrefs(self)?.to_owned())
    }
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        match self.buffer {
            SerializedSource::Heap(ref bytes) => bytes.clone(),
            SerializedSource::Static(bytes) => bytes.to_vec(),
        }
    }
    #[must_use]
    pub fn buffer(&self) -> &SerializedSource {
        &self.buffer
    }

    #[must_use]
    pub const fn const_from_bytes(bytes: &'static [u8]) -> SerializedProgram {
        SerializedProgram {
            buffer: SerializedSource::Static(bytes),
        }
    }
    pub fn to_owned(self) -> SerializedProgram {
        match self.buffer {
            SerializedSource::Static(s) => SerializedProgram {
                buffer: SerializedSource::Heap(s.to_vec()),
            },
            SerializedSource::Heap(s) => SerializedProgram {
                buffer: SerializedSource::Heap(s),
            },
        }
    }
    pub fn to_program(&self) -> Result<Program<'_>, ClvmError> {
        Program::from_serial(self)
    }
}
impl ChiaSerialize for SerializedProgram {
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        let mut stream: Cursor<&[u8]> = (&self.buffer).into();
        let claim_sexp = sexp_from_bytes(&mut stream)?;
        let as_bytes = sexp_to_bytes(&claim_sexp)?;
        Ok(as_bytes.as_ref().to_vec())
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let claim_sexp = sexp_from_bytes(bytes)?;
        sexp_to_bytes(&claim_sexp)
    }
}
impl Display for SerializedProgram {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.buffer))
    }
}
impl AsRef<[u8]> for SerializedProgram {
    fn as_ref(&self) -> &[u8] {
        match &self.buffer {
            SerializedSource::Heap(buffer) => buffer.as_slice(),
            SerializedSource::Static(buffer) => buffer,
        }
    }
}
// impl TryFrom<&SerializedProgram> for SExp<'_> {
//     type Error = ClvmError;
//     fn try_from(value: &SerializedProgram) -> Result<SExp<'static>, Self::Error> {
//         let mut cursor = Cursor::new(value.buffer.as_ref());
//         Ok(sexp_from_bytes(&mut cursor)?)
//     }
// }
impl Debug for SerializedProgram {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.buffer))
    }
}

#[inline]
pub const fn strip_prefix(bytes: &[u8]) -> &[u8] {
    match bytes {
        [b'0', b'x', rest @ ..] => rest,
        _ => bytes,
    }
}

pub const fn hex_bytes_len(s: &str) -> usize {
    let b = s.as_bytes();
    let has_prefix = b.len() >= 2 && b[0] == b'0' && (b[1] | 0x20) == b'x';
    let start = if has_prefix { 2 } else { 0 };
    (b.len() - start) / 2
}
impl TryFrom<String> for SerializedProgram {
    type Error = ClvmError;

    fn try_from(hex: String) -> Result<SerializedProgram, ClvmError> {
        SerializedProgram::from_hex(&hex)
    }
}

impl TryFrom<&str> for SerializedProgram {
    type Error = ClvmError;

    fn try_from(hex: &str) -> Result<SerializedProgram, ClvmError> {
        SerializedProgram::from_hex(hex)
    }
}
struct SerializedProgramVisitor;

impl Visitor<'_> for SerializedProgramVisitor {
    type Value = SerializedProgram;

    fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
        formatter.write_str("Expecting a hex String, or byte array")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        value.try_into().map_err(serde::de::Error::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        value.try_into().map_err(serde::de::Error::custom)
    }
}

impl Serialize for SerializedProgram {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}

impl<'a> Deserialize<'a> for SerializedProgram {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        match deserializer.deserialize_string(SerializedProgramVisitor) {
            Ok(hex) => Ok(hex),
            Err(er) => Err(er),
        }
    }
}

struct ProgramVisitor;

impl Visitor<'_> for ProgramVisitor {
    type Value = Program<'static>;

    fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
        formatter.write_str("Expecting a hex String, or byte array")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let serial: SerializedProgram = value.try_into().map_err(serde::de::Error::custom)?;
        let mut cursor = Cursor::new(serial.buffer.as_ref());
        let sexp = sexp_from_bytes(&mut cursor).map_err(serde::de::Error::custom)?;
        Ok(Program::new_const(sexp.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let serial: SerializedProgram = value.try_into().map_err(serde::de::Error::custom)?;
        let mut cursor = Cursor::new(serial.buffer.as_ref());
        let sexp = sexp_from_bytes(&mut cursor).map_err(serde::de::Error::custom)?;
        Ok(Program::new_const(sexp.to_owned()))
    }
}

impl<'a> Deserialize<'a> for Program<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'a>,
    {
        match deserializer.deserialize_string(ProgramVisitor) {
            Ok(prog) => Ok(prog),
            Err(er) => Err(er),
        }
    }
}

impl<'a> Serialize for Program<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(
            self.serialized()
                .map_err(serde::ser::Error::custom)?
                .to_string()
                .as_str(),
        )
    }
}
