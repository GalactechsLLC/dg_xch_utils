use crate::clvm::curry_utils::{curry, uncurry};
use crate::clvm::dialect::ChiaDialect;
use crate::clvm::parser::{sexp_from_bytes, sexp_to_bytes};
use crate::clvm::run_program::run_program;
use crate::clvm::sexp::AtomBuf;
use crate::clvm::sexp::{SExp, NULL};
use crate::clvm::utils::{tree_hash, MEMPOOL_MODE};
use crate::types::blockchain::sized_bytes::*;
use hex::encode;
use num_bigint::BigInt;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::hash::Hasher;
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::{fmt, fs};

#[derive(Debug)]
pub struct Program {
    pub serialized: Vec<u8>,
    sexp: SExp,
}
impl Program {
    pub fn curry(&self, args: &Vec<Program>) -> Result<Program, Error> {
        let (_cost, program) = curry(self, args)?;
        Ok(program)
    }

    pub fn uncurry(&self) -> Result<(Program, Program), Error> {
        let serial_program = SerializedProgram::from_bytes(&self.serialized);
        match uncurry(&serial_program)? {
            Some((program, args)) => Ok((program.to_program()?, args.to_program()?)),
            None => Ok((serial_program.to_program()?, 0.try_into()?)),
        }
    }

    pub fn as_list(&self) -> Vec<Program> {
        match self.as_pair() {
            None => {
                vec![]
            }
            Some((first, rest)) => {
                let mut rtn: Vec<Program> = vec![first.clone()];
                rtn.extend(rest.as_list());
                rtn
            }
        }
    }

    pub fn as_atom_list(&self) -> Vec<Vec<u8>> {
        match self.as_pair() {
            None => {
                vec![]
            }
            Some((first, rest)) => match first.as_vec() {
                None => {
                    vec![]
                }
                Some(atom) => {
                    let mut rtn: Vec<Vec<u8>> = vec![atom];
                    rtn.extend(rest.as_atom_list());
                    rtn
                }
            },
        }
    }

    pub fn to_map(self) -> Result<HashMap<Program, Program>, Error> {
        Ok(self
            .sexp
            .to_map()?
            .into_iter()
            .filter_map(|m| {
                if let (Ok(p1), Ok(p2)) = (sexp_to_bytes(&m.0), sexp_to_bytes(&m.1)) {
                    Some((Program::new(p1), Program::new(p2)))
                } else {
                    None
                }
            })
            .collect())
    }

    pub fn to_sexp(&self) -> SExp {
        self.sexp.clone()
    }

    pub fn is_atom(&self) -> bool {
        matches!(self.sexp, SExp::Atom(_))
    }

    pub fn is_pair(&self) -> bool {
        matches!(self.sexp, SExp::Pair(_))
    }

    pub fn as_atom(&self) -> Option<Program> {
        match self.sexp {
            SExp::Atom(_) => match sexp_to_bytes(&self.sexp) {
                Ok(s) => Some(Program::new(s)),
                Err(_) => None,
            },
            _ => None,
        }
    }

    pub fn as_vec(&self) -> Option<Vec<u8>> {
        match &self.sexp {
            SExp::Atom(vec) => Some(vec.data.clone()),
            _ => None,
        }
    }

    pub fn as_pair(&self) -> Option<(Program, Program)> {
        match self.to_sexp() {
            SExp::Pair(pair) => {
                let left = match sexp_to_bytes(&pair.first) {
                    Ok(serial_data) => Program::new(serial_data),
                    Err(_) => Program::new(Vec::new()),
                };
                let right = match sexp_to_bytes(&pair.rest) {
                    Ok(serial_data) => Program::new(serial_data),
                    Err(_) => Program::new(Vec::new()),
                };
                Some((left, right))
            }
            _ => None,
        }
    }

    pub fn cons(&self, other: &Program) -> Program {
        let first = match sexp_from_bytes(&self.serialized) {
            Ok(ptr) => ptr,
            Err(_) => NULL.clone(),
        };
        let rest = match sexp_from_bytes(&other.serialized) {
            Ok(ptr) => ptr,
            Err(_) => NULL.clone(),
        };
        match sexp_to_bytes(&SExp::Pair((&first, &rest).into())) {
            Ok(bytes) => Program::new(bytes),
            Err(_) => Program::null(),
        }
    }

    pub fn as_int(&self) -> Result<BigInt, Error> {
        match &self.as_atom() {
            Some(atom) => Ok(BigInt::from_signed_bytes_be(
                atom.as_vec()
                    .ok_or_else(|| {
                        Error::new(ErrorKind::InvalidData, "Failed to convert Program to Atom")
                    })?
                    .as_slice(),
            )),
            None => {
                log::debug!("BAD INT: {:?}", self.serialized);
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "Program is Pair not Atom",
                ))
            }
        }
    }

    pub fn first(&self) -> Result<Program, Error> {
        match self.as_pair() {
            Some((p1, _)) => Ok(p1),
            _ => Err(Error::new(ErrorKind::Unsupported, "first of non-cons")),
        }
    }

    pub fn rest(&self) -> Result<Program, Error> {
        match self.as_pair() {
            Some((_, p2)) => Ok(p2),
            _ => Err(Error::new(ErrorKind::Unsupported, "rest of non-cons")),
        }
    }
}

impl TryFrom<Vec<u8>> for Program {
    type Error = Error;
    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let atom = SExp::Atom(AtomBuf::from(bytes));
        Ok(Program {
            serialized: sexp_to_bytes(&atom)?,
            sexp: atom,
        })
    }
}

impl TryFrom<&Vec<u8>> for Program {
    type Error = Error;
    fn try_from(bytes: &Vec<u8>) -> Result<Self, Self::Error> {
        let atom = SExp::Atom(AtomBuf::from(bytes));
        Ok(Program {
            serialized: sexp_to_bytes(&atom)?,
            sexp: atom,
        })
    }
}

impl TryFrom<(Program, Program)> for Program {
    type Error = Error;
    fn try_from((first, second): (Program, Program)) -> Result<Self, Self::Error> {
        let first = sexp_from_bytes(first.serialized)?;
        let rest = sexp_from_bytes(second.serialized)?;
        let sexp = SExp::Pair((&first, &rest).into());
        let bytes = sexp_to_bytes(&sexp)?;
        Ok(Program {
            serialized: bytes,
            sexp,
        })
    }
}

impl Clone for Program {
    fn clone(&self) -> Self {
        Program::new(self.serialized.clone())
    }
}

impl Hash for Program {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.serialized.hash(state);
    }
}

impl PartialEq for Program {
    fn eq(&self, other: &Self) -> bool {
        self.serialized == other.serialized
    }
}
impl Eq for Program {}

impl Display for Program {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "({})", encode(&self.serialized))
    }
}

impl Program {
    pub fn new(serialized: Vec<u8>) -> Self {
        match sexp_from_bytes(&serialized) {
            Ok(sexp) => Program { serialized, sexp },
            Err(_) => Program {
                serialized: vec![],
                sexp: NULL.clone(),
            },
        }
    }
    pub fn null() -> Self {
        let serial = match sexp_to_bytes(&NULL) {
            Ok(bytes) => bytes,
            Err(_) => vec![],
        };
        Program {
            serialized: serial,
            sexp: NULL.clone(),
        }
    }

    pub fn tree_hash(&self) -> Bytes32 {
        let sexp = match sexp_from_bytes(&self.serialized) {
            Ok(node) => node,
            Err(e) => {
                println!("ERROR: {:?}", e);
                NULL.clone()
            },
        };
        Bytes32::new(tree_hash(&sexp))
    }
}

macro_rules! impl_sized_bytes {
    ($($name: ident);*) => {
        $(
            impl TryFrom<$name> for Program {
                type Error = std::io::Error;
                fn try_from(bytes: $name) -> Result<Self, Self::Error> {
                    bytes.to_bytes().try_into()
                }
            }
            impl TryFrom<&$name> for Program {
                type Error = std::io::Error;
                fn try_from(bytes: &$name) -> Result<Self, Self::Error> {
                    bytes.to_bytes().try_into()
                }
            }
        )*
    };
    ()=>{};
}

impl_sized_bytes!(
    UnsizedBytes;
    Bytes4;
    Bytes8;
    Bytes16;
    Bytes32;
    Bytes48;
    Bytes96;
    Bytes192
);

macro_rules! impl_ints {
    ($($name: ident, $size: expr);*) => {
        $(
            impl TryFrom<$name> for Program {
                type Error = std::io::Error;
                fn try_from(int_val: $name) -> Result<Self, Self::Error> {
                    if int_val == 0 {
                        return Ok(Program::new(Vec::new()));
                    }
                    let as_ary = int_val.to_be_bytes();
                    let mut as_bytes = as_ary.as_slice();
                    while as_bytes.len() > 1 && as_bytes[0] == ( if as_bytes[1] & 0x80 > 0{0xFF} else {0}) {
                        as_bytes = &as_bytes[1..];
                    }
                    as_bytes.to_vec().try_into()
                }
            }
            impl TryInto<$name> for &Program {
                type Error = Error;

                fn try_into(self) -> Result<$name, Self::Error> {
                    let as_atom = self.as_vec().ok_or(Error::new(ErrorKind::InvalidInput, "Invalid program for $name"))?;
                    if as_atom.len() > $size {
                        return Err(Error::new(ErrorKind::InvalidInput, "Invalid program for $name"));
                    } else {
                        Ok($name::from_le_bytes(as_atom.as_slice().try_into().map_err(|e| Error::new(ErrorKind::InvalidInput, format!("Invalid program for $name: {:?}", e)))?))
                    }
                }
            }
            impl TryInto<$name> for Program {
                type Error = Error;
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

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SerializedProgram {
    buffer: Vec<u8>,
}
impl Display for SerializedProgram {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&encode(&self.buffer))
    }
}
impl SerializedProgram {
    pub fn from_file(path: &Path) -> Result<SerializedProgram, Error> {
        Ok(SerializedProgram {
            buffer: fs::read(path)?,
        })
    }
    pub fn from_bytes(bytes: &[u8]) -> SerializedProgram {
        SerializedProgram {
            buffer: bytes.to_owned(),
        }
    }
    pub fn from_hex(hex_str: &str) -> Result<SerializedProgram, Error> {
        Ok(SerializedProgram {
            buffer: hex_to_bytes(hex_str).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidData,
                    "Failed to convert str to SerializedProgram",
                )
            })?,
        })
    }
    //pub fn uncurry(&self) -> (SerializedProgram, SerializedProgram) {}
    pub fn to_bytes(&self) -> Vec<u8> {
        self.buffer.clone()
    }

    pub fn run_mempool_with_cost(
        &self,
        max_cost: u64,
        args: &Program,
    ) -> Result<(u64, SExp), Error> {
        self.run(max_cost, MEMPOOL_MODE, args)
    }

    pub fn run_with_cost(&self, max_cost: u64, args: &Program) -> Result<(u64, SExp), Error> {
        self.run(max_cost, 0, args)
    }

    pub fn to_program(&self) -> Result<Program, Error> {
        Ok(Program::new(self.buffer.clone()))
    }

    fn run(&self, max_cost: u64, flags: u32, args: &Program) -> Result<(u64, SExp), Error> {
        let program = sexp_from_bytes(&self.buffer)?;
        let args = sexp_from_bytes(&args.serialized)?;
        let dialect = ChiaDialect::new(flags);
        let (cost, result) = match run_program(dialect, &program, &args, max_cost, None) {
            Ok(reduct) => reduct,
            Err(e) => {
                return Err(e);
            }
        };
        let bytes = sexp_to_bytes(&result)?;
        Ok((cost, sexp_from_bytes(bytes)?))
    }
}
impl TryFrom<String> for SerializedProgram {
    type Error = Error;

    fn try_from(hex: String) -> Result<SerializedProgram, Error> {
        SerializedProgram::from_hex(&hex)
    }
}

impl TryFrom<&str> for SerializedProgram {
    type Error = Error;

    fn try_from(hex: &str) -> Result<SerializedProgram, Error> {
        SerializedProgram::from_hex(hex)
    }
}

impl From<Program> for SerializedProgram {
    fn from(prog: Program) -> Self {
        SerializedProgram::from_bytes(&prog.serialized)
    }
}
struct SerializedProgramVisitor;

impl<'de> Visitor<'de> for SerializedProgramVisitor {
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
