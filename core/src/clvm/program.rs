use crate::blockchain::sized_bytes::{
    hex_to_bytes, Bytes100, Bytes32, Bytes4, Bytes48, Bytes480, Bytes8, Bytes96, SizedBytes,
};
use crate::clvm::assemble::assemble_text;
use crate::clvm::curry_utils::curry;
use crate::clvm::dialect::{ChiaDialect, NO_UNKNOWN_OPS};
use crate::clvm::parser::{sexp_from_bytes, sexp_to_bytes};
use crate::clvm::run_program::run_program;
use crate::clvm::sexp::{AtomBuf, IntoSExp};
use crate::clvm::sexp::{SExp, NULL as SNULL};
use crate::clvm::utils::{tree_hash, MEMPOOL_MODE};
use dg_xch_macros::ChiaSerial;
use hex::encode;
use num_bigint::BigInt;
use once_cell::sync::Lazy;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::hash::Hasher;
use std::io::{Error, ErrorKind};
use std::path::Path;

pub static NULL: Lazy<Program> = Lazy::new(|| Program {
    sexp: SNULL.clone(),
    serialized: vec![],
});

#[derive(Eq, Serialize, Deserialize)]
pub struct Program {
    pub serialized: Vec<u8>,
    pub sexp: SExp,
}

impl Display for Program {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", &self.sexp)
    }
}

impl Debug for Program {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", &self.sexp)
    }
}

impl Program {
    pub fn new(serialized: Vec<u8>) -> Self {
        match sexp_from_bytes(&serialized) {
            Ok(sexp) => Program { serialized, sexp },
            Err(e) => {
                println!("Error building Program: {e:?}");
                Program {
                    serialized: vec![],
                    sexp: SNULL.clone(),
                }
            }
        }
    }
    pub fn null() -> Self {
        let serial = sexp_to_bytes(&SNULL).unwrap_or_default();
        Program {
            serialized: serial,
            sexp: SNULL.clone(),
        }
    }
    pub fn to<T: IntoSExp>(vals: T) -> Self {
        let sexp = vals.to_sexp();
        let serialized = sexp_to_bytes(&sexp).unwrap_or_default();
        Program { serialized, sexp }
    }
    pub fn from_sexp(sexp: SExp) -> Result<Self, Error> {
        let serialized = sexp_to_bytes(&sexp)?;
        Ok(Program { serialized, sexp })
    }
    pub fn first(&self) -> Result<Self, Error> {
        let first = self.sexp.first()?;
        let serialized = sexp_to_bytes(first).unwrap_or_default();
        Ok(Program {
            serialized,
            sexp: first.clone(),
        })
    }
    pub fn rest(&self) -> Result<Self, Error> {
        let rest = self.sexp.rest()?;
        let serialized = sexp_to_bytes(rest).unwrap_or_default();
        Ok(Program {
            serialized,
            sexp: rest.clone(),
        })
    }
    pub fn at(&self, path: &str) -> Result<Program, Error> {
        let mut rtn = &self.sexp;
        for c in path.chars() {
            if c == 'f' || c == 'F' {
                rtn = rtn.first()?;
            } else if c == 'r' || c == 'R' {
                rtn = rtn.rest()?;
            } else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("`at` got illegal character `{c}`. Only `f` & `r` allowed"),
                ));
            }
        }
        let serialized = sexp_to_bytes(rtn)?;
        Ok(Program {
            serialized,
            sexp: rtn.clone(),
        })
    }

    #[must_use]
    pub fn tree_hash(&self) -> Bytes32 {
        let sexp = sexp_from_bytes(&self.serialized).unwrap_or_else(|e| {
            println!("ERROR: {e:?}");
            SNULL.clone()
        });
        Bytes32::new(&tree_hash(&sexp))
    }
    pub fn curry(&self, args: &[Program]) -> Result<Program, Error> {
        Ok(curry(self, args))
    }

    pub fn uncurry(&self) -> Result<(Program, Program), Error> {
        fn inner_match(o: &SExp, expected: &[u8]) -> Result<(), Error> {
            if o.atom()? == expected {
                Ok(())
            } else {
                Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("expected: {}", encode(expected)),
                ))
            }
        }
        {
            //(2 (1 . <mod>) <args>)
            let as_list = self.as_list();
            inner_match(&as_list[0].clone().to_sexp() /*ev*/, b"\x02")?;
            let q_pair = as_list[1].as_pair().ok_or_else(|| {
                //quoted_inner
                Error::new(
                    ErrorKind::InvalidData,
                    format!("expected pair found atom: {}", as_list[1]),
                )
            })?;
            inner_match(&q_pair.0.to_sexp(), b"\x01")?;
            let mut args = vec![];
            let mut args_list = as_list[2].clone();
            while args_list.is_pair() {
                //(4(1. < arg >) < rest >)
                let as_list = args_list.as_list();
                inner_match(&as_list[0].clone().to_sexp(), b"\x04")?;
                let q_pair = as_list[1].as_pair().ok_or_else(|| {
                    //quoted_inner
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("expected pair found atom: {}", as_list[1]),
                    )
                })?;
                inner_match(&q_pair.0.to_sexp(), b"\x01")?;
                args.push(q_pair.1.to_sexp());
                args_list = as_list[2].clone();
            }
            inner_match(&args_list.to_sexp(), b"\x01")?;
            Ok((Program::to(q_pair.1), Program::to(args)))
        }
        .or_else(|_: Error| Ok((self.clone(), Program::to(0))))
    }

    #[must_use]
    pub fn as_list(&self) -> Vec<Program> {
        match self.as_pair() {
            None => {
                vec![]
            }
            Some((first, rest)) => {
                let mut rtn: Vec<Program> = vec![first];
                rtn.extend(rest.as_list());
                rtn
            }
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

    #[must_use]
    pub fn is_atom(&self) -> bool {
        matches!(self.sexp, SExp::Atom(_))
    }

    #[must_use]
    pub fn is_pair(&self) -> bool {
        matches!(self.sexp, SExp::Pair(_))
    }

    #[must_use]
    pub fn as_atom(&self) -> Option<Program> {
        match self.sexp {
            SExp::Atom(_) => match sexp_to_bytes(&self.sexp) {
                Ok(s) => Some(Program::new(s)),
                Err(_) => None,
            },
            SExp::Pair(_) => None,
        }
    }

    #[must_use]
    pub fn as_vec(&self) -> Option<Vec<u8>> {
        match &self.sexp {
            SExp::Atom(vec) => Some(vec.data.clone()),
            SExp::Pair(_) => None,
        }
    }

    #[must_use]
    pub fn as_pair(&self) -> Option<(Program, Program)> {
        match &self.sexp {
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
            SExp::Atom(_) => None,
        }
    }

    #[must_use]
    pub fn cons(&self, other: &Program) -> Program {
        match sexp_to_bytes(&SExp::Pair((&self.sexp, &other.sexp).into())) {
            Ok(bytes) => Program::new(bytes),
            Err(e) => {
                println!("{e:?}");
                Program::null()
            }
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

    pub fn run(&self, max_cost: u64, flags: u32, args: &Program) -> Result<(u64, Program), Error> {
        let program = sexp_from_bytes(&self.serialized)?;
        let args = sexp_from_bytes(&args.serialized)?;
        let dialect = ChiaDialect::new(flags | NO_UNKNOWN_OPS);
        let (cost, result) = match run_program(dialect, &program, &args, max_cost, None) {
            Ok(reduct) => reduct,
            Err(e) => {
                return Err(e);
            }
        };
        let serialized = sexp_to_bytes(&result)?;
        let sexp = sexp_from_bytes(&serialized)?;
        Ok((cost, Program { serialized, sexp }))
    }
}

impl TryFrom<Vec<u8>> for Program {
    type Error = Error;
    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        (&bytes).try_into()
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

impl TryFrom<&[u8]> for Program {
    type Error = Error;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
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

macro_rules! impl_sized_bytes {
    ($($name: ident);*) => {
        $(
            impl TryFrom<$name> for Program {
                type Error = std::io::Error;
                fn try_from(bytes: $name) -> Result<Self, Self::Error> {
                    bytes.as_slice().try_into()
                }
            }
            impl TryFrom<&$name> for Program {
                type Error = std::io::Error;
                fn try_from(bytes: &$name) -> Result<Self, Self::Error> {
                    bytes.as_slice().try_into()
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
                    }
                    Ok($name::from_le_bytes(as_atom.as_slice().try_into().map_err(|e| Error::new(ErrorKind::InvalidInput, format!("Invalid program for $name: {:?}", e)))?))
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

#[derive(ChiaSerial, Clone, PartialEq, Eq)]
pub struct SerializedProgram {
    buffer: Vec<u8>,
}
impl Display for SerializedProgram {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.buffer))
    }
}
impl AsRef<[u8]> for SerializedProgram {
    fn as_ref(&self) -> &[u8] {
        &self.buffer
    }
}
impl Debug for SerializedProgram {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", encode(&self.buffer))
    }
}
impl SerializedProgram {
    pub async fn from_file(path: &Path) -> Result<SerializedProgram, Error> {
        if path.ends_with("bin") {
            Ok(Self {
                buffer: tokio::fs::read(path).await?,
            })
        } else if path.ends_with("hex") {
            SerializedProgram::from_hex(tokio::fs::read_to_string(&path).await?.trim())
        } else if path.ends_with("clvm") {
            assemble_text(tokio::fs::read_to_string(&path).await?.trim())
        } else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid File type, Expected Hex or Bin: {path:?}"),
            ));
        }
    }
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> SerializedProgram {
        SerializedProgram {
            buffer: bytes.to_owned(),
        }
    }
    pub fn from_hex(hex_str: &str) -> Result<SerializedProgram, Error> {
        Ok(SerializedProgram {
            buffer: hex_to_bytes(hex_str.trim()).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidData,
                    "Failed to convert str to SerializedProgram",
                )
            })?,
        })
    }
    //pub fn uncurry(&self) -> (SerializedProgram, SerializedProgram) {}
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.buffer.clone()
    }

    pub fn run_mempool_with_cost(
        &self,
        max_cost: u64,
        args: &Program,
    ) -> Result<(u64, Program), Error> {
        self.run(max_cost, MEMPOOL_MODE, args)
    }

    pub fn run_with_cost(&self, max_cost: u64, args: &Program) -> Result<(u64, Program), Error> {
        self.run(max_cost, 0, args)
    }

    #[must_use]
    pub fn to_program(&self) -> Program {
        Program::new(self.buffer.clone())
    }

    pub fn run(&self, max_cost: u64, flags: u32, args: &Program) -> Result<(u64, Program), Error> {
        let program = sexp_from_bytes(&self.buffer)?;
        let args = sexp_from_bytes(&args.serialized)?;
        let dialect = ChiaDialect::new(flags);
        let (cost, result) = match run_program(dialect, &program, &args, max_cost, None) {
            Ok(reduct) => reduct,
            Err(e) => {
                return Err(e);
            }
        };
        let serialized = sexp_to_bytes(&result)?;
        let sexp = sexp_from_bytes(&serialized)?;
        Ok((cost, Program { serialized, sexp }))
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
