use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::sized_bytes::{
    Bytes100, Bytes32, Bytes4, Bytes48, Bytes480, Bytes8, Bytes96, SizedBytes,
};
use crate::clvm::assemble::is_hex;
use crate::clvm::assemble::keywords::KEYWORD_FROM_ATOM;
use crate::clvm::program::Program;
use crate::clvm::utils::{number_from_u8, u64_from_bigint};
use hex::encode;
use num_bigint::BigInt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::{Error, ErrorKind};
use std::mem::replace;

pub static NULL: Lazy<SExp> = Lazy::new(|| SExp::Atom(vec![].into()));
pub static ONE: Lazy<SExp> = Lazy::new(|| SExp::Atom(vec![1u8].into()));

#[derive(Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum SExp {
    Atom(AtomBuf),
    Pair(PairBuf),
}
impl<'a> IntoIterator for &'a SExp {
    type Item = &'a SExp;
    type IntoIter = SExpIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl SExp {
    pub fn atom(&self) -> Result<&AtomBuf, Error> {
        match self {
            SExp::Atom(a) => Ok(a),
            SExp::Pair(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "Expected Atom, got Pair",
            )),
        }
    }
    pub fn pair(&self) -> Result<&PairBuf, Error> {
        match self {
            SExp::Atom(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "Expected Pair, got Atom",
            )),
            SExp::Pair(p) => Ok(p),
        }
    }
    pub fn first(&self) -> Result<&SExp, Error> {
        match self {
            SExp::Atom(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "Expected Pair, got Atom",
            )),
            SExp::Pair(p) => Ok(&p.first),
        }
    }
    pub fn rest(&self) -> Result<&SExp, Error> {
        match self {
            SExp::Atom(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "Expected Pair, got Atom",
            )),
            SExp::Pair(p) => Ok(&p.rest),
        }
    }
    #[must_use]
    pub fn cons(self, other: SExp) -> SExp {
        SExp::Pair(PairBuf {
            first: Box::new(self),
            rest: Box::new(other),
        })
    }
    pub fn split(&self) -> Result<(&SExp, &SExp), Error> {
        match self {
            SExp::Atom(_) => Err(Error::new(
                ErrorKind::Unsupported,
                "Expected Pair, got Atom",
            )),
            SExp::Pair(p) => Ok((&*p.first, &*p.rest)),
        }
    }

    #[must_use]
    pub fn nullp(&self) -> bool {
        match &self {
            SExp::Atom(a) => a.data.is_empty(),
            SExp::Pair(_) => false,
        }
    }

    #[must_use]
    pub fn as_atom_list(&self) -> Vec<Vec<u8>> {
        match self {
            SExp::Atom(_) => {
                vec![]
            }
            SExp::Pair(pair) => match pair.first.as_ref() {
                SExp::Atom(buf) => {
                    let mut rtn: Vec<Vec<u8>> = vec![buf.data.clone()];
                    rtn.extend(pair.rest.as_atom_list());
                    rtn
                }
                SExp::Pair(_) => {
                    vec![]
                }
            },
        }
    }

    pub fn to_map(self) -> Result<HashMap<SExp, SExp>, Error> {
        let mut rtn: HashMap<SExp, SExp> = HashMap::new();
        let mut cur_node = self;
        loop {
            match cur_node {
                SExp::Atom(_) => break,
                SExp::Pair(pair) => {
                    cur_node = *pair.rest;
                    match *pair.first {
                        SExp::Atom(_) => {
                            rtn.insert(*pair.first, NULL.clone());
                        }
                        SExp::Pair(inner_pair) => {
                            rtn.insert(*inner_pair.first, *inner_pair.rest);
                        }
                    }
                }
            }
        }
        Ok(rtn)
    }

    #[must_use]
    pub fn arg_count_is(&self, mut count: usize) -> bool {
        let mut ptr = self;
        loop {
            if count == 0 {
                return ptr.nullp();
            }
            match ptr {
                SExp::Pair(pair) => {
                    ptr = &pair.rest;
                }
                SExp::Atom(_) => return false,
            }
            count -= 1;
        }
    }

    #[must_use]
    pub fn iter(&self) -> SExpIter {
        SExpIter { c: self }
    }

    #[must_use]
    pub fn as_bool(&self) -> bool {
        match self.atom() {
            Ok(v0) => !v0.data.is_empty(),
            _ => true,
        }
    }

    #[must_use]
    pub fn from_bool(b: bool) -> &'static SExp {
        if b {
            &ONE
        } else {
            &NULL
        }
    }

    #[must_use]
    pub fn proper_list(self, store: bool) -> Option<Vec<SExp>> {
        let mut args = vec![];
        let mut args_sexp = self;
        loop {
            match args_sexp {
                SExp::Atom(_) => {
                    return if args_sexp.non_nil() {
                        None
                    } else {
                        Some(args)
                    };
                }
                SExp::Pair(buf) => {
                    if store {
                        args.push(*buf.first);
                    }
                    args_sexp = *buf.rest;
                }
            }
        }
    }

    #[must_use]
    pub fn non_nil(&self) -> bool {
        match self {
            SExp::Pair(_) => true,
            SExp::Atom(b) => !b.data.is_empty(),
        }
    }
}

const PRINTABLE: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ#!$%&'()*+,-./:;<=>?@[\\]^_`{|}~\"\r\n";

impl Display for SExp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self {
            SExp::Atom(a) => {
                write!(f, "{a}")
            }
            SExp::Pair(p) => {
                write!(f, "{p}")
            }
        }
    }
}

impl Debug for SExp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self {
            SExp::Atom(a) => {
                write!(f, "{a:?}")
            }
            SExp::Pair(p) => {
                write!(f, "{p:?}")
            }
        }
    }
}

pub struct SExpIter<'a> {
    c: &'a SExp,
}

impl<'a> Iterator for SExpIter<'a> {
    type Item = &'a SExp;

    fn next(&mut self) -> Option<Self::Item> {
        if self.c.nullp() {
            None
        } else {
            match self.c {
                SExp::Atom(a) => {
                    if a.data.is_empty() {
                        None
                    } else {
                        let rtn = replace(&mut self.c, &NULL);
                        Some(rtn)
                    }
                }
                SExp::Pair(pair) => {
                    self.c = &pair.rest;
                    Some(&pair.first)
                }
            }
        }
    }
}

#[derive(Hash, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomBuf {
    pub data: Vec<u8>,
}

impl Debug for AtomBuf {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl Display for AtomBuf {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.data.is_empty() {
            f.write_str("()")
        } else if self.data.len() > 2 {
            match String::from_utf8(self.data.clone()) {
                Ok(as_utf8) => {
                    for s in as_utf8.chars() {
                        if !PRINTABLE.contains(&s.to_string()) {
                            return f.write_str(&format!("0x{}", encode(&self.data)));
                        }
                    }
                    if as_utf8.contains('"') && as_utf8.contains('\'') {
                        f.write_str(&format!("0x{}", encode(&self.data)))
                    } else if as_utf8.contains('"') {
                        f.write_str(&format!("'{as_utf8}'"))
                    } else if as_utf8.contains('\'') {
                        f.write_str(&format!("\"{as_utf8}\""))
                    } else if is_hex(as_utf8.as_bytes()) {
                        f.write_str(&format!("0x{as_utf8}"))
                    } else {
                        f.write_str(&format!("\"{as_utf8}\""))
                    }
                }
                Err(_) => f.write_str(&format!("0x{}", encode(&self.data))),
            }
        } else if *self.data == BigInt::from_signed_bytes_be(&self.data).to_signed_bytes_be() {
            f.write_str(&format!("{}", BigInt::from_signed_bytes_be(&self.data)))
        } else {
            f.write_str(&format!("0x{}", encode(&self.data)))
        }
    }
}

impl AtomBuf {
    #[must_use]
    pub fn new(v: Vec<u8>) -> Self {
        AtomBuf { data: v }
    }
    pub fn as_bytes32(&self) -> Result<Bytes32, Error> {
        if self.data.len() == Bytes32::SIZE {
            Ok(Bytes32::new(self.data.as_slice()))
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid Length for Bytes32: {}", self.data.len()),
            ))
        }
    }
    pub fn as_int(&self) -> BigInt {
        number_from_u8(&self.data)
    }
    pub fn as_u64(&self) -> Result<u64, Error> {
        u64_from_bigint(&number_from_u8(&self.data))
    }
}

impl From<&[u8]> for AtomBuf {
    fn from(v: &[u8]) -> Self {
        Self::new(v.to_vec())
    }
}

impl From<Vec<u8>> for AtomBuf {
    fn from(v: Vec<u8>) -> Self {
        Self::new(v)
    }
}

impl From<&Vec<u8>> for AtomBuf {
    fn from(v: &Vec<u8>) -> Self {
        Self::from(v.clone())
    }
}
impl PartialEq<&[u8]> for AtomBuf {
    fn eq(&self, other: &&[u8]) -> bool {
        self.data == *other
    }
}

impl PartialEq<[u8]> for AtomBuf {
    fn eq(&self, other: &[u8]) -> bool {
        self.data == other
    }
}

impl PartialEq<Vec<u8>> for AtomBuf {
    fn eq(&self, other: &Vec<u8>) -> bool {
        &self.data == other
    }
}

#[derive(Hash, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairBuf {
    pub first: Box<SExp>,
    pub rest: Box<SExp>,
}

impl Display for PairBuf {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut buffer = String::from("(");
        match &*self.first {
            SExp::Atom(a) => {
                if let Some(kw) = KEYWORD_FROM_ATOM.get(&a.data) {
                    buffer += kw;
                } else {
                    buffer += &format!("{}", self.first);
                }
            }
            SExp::Pair(_) => {
                buffer += &format!("{}", self.first);
            }
        }
        let mut current = &self.rest;
        while let Ok(p) = current.pair() {
            buffer += &format!(" {}", &p.first.as_ref());
            current = &p.rest;
        }
        if current.non_nil() {
            buffer += &format!(" . {}", *current);
        }
        buffer += ")";
        write!(f, "{buffer}")
    }
}

impl Debug for PairBuf {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut buffer = String::from("(");
        match &*self.first {
            SExp::Atom(a) => {
                buffer += &format!("{a:?}");
            }
            SExp::Pair(p) => {
                buffer += &format!("{p:?}");
            }
        }
        let mut current = &self.rest;
        while let Ok(p) = current.pair() {
            buffer += &format!(" {:?}", &p.first.as_ref());
            current = &p.rest;
        }
        if current.non_nil() {
            buffer += &format!(" . {:?}", *current);
        }
        buffer += ")";
        write!(f, "{buffer}")
    }
}

impl From<(&SExp, &SExp)> for PairBuf {
    fn from(v: (&SExp, &SExp)) -> Self {
        PairBuf {
            first: Box::new(v.0.clone()),
            rest: Box::new(v.1.clone()),
        }
    }
}

impl From<(SExp, SExp)> for PairBuf {
    fn from(v: (SExp, SExp)) -> Self {
        PairBuf {
            first: Box::new(v.0),
            rest: Box::new(v.1),
        }
    }
}

pub trait IntoSExp {
    fn to_sexp(self) -> SExp;
}

pub trait TryIntoSExp {
    fn try_to_sexp(self) -> Result<SExp, Error>;
}

impl IntoSExp for Vec<SExp> {
    fn to_sexp(self) -> SExp {
        if let Some(sexp) = self.first().cloned() {
            let mut end = NULL.clone();
            if self.len() > 1 {
                for other in self[1..].iter().rev() {
                    end = other.clone().cons(end);
                }
            }
            sexp.cons(end)
        } else {
            NULL.clone()
        }
    }
}

impl<T: IntoSExp + Clone> IntoSExp for &[T] {
    fn to_sexp(self) -> SExp {
        self.iter()
            .cloned()
            .map(IntoSExp::to_sexp)
            .collect::<Vec<SExp>>()
            .to_sexp()
    }
}

impl<T: IntoSExp> IntoSExp for Vec<T> {
    fn to_sexp(self) -> SExp {
        self.into_iter()
            .map(IntoSExp::to_sexp)
            .collect::<Vec<SExp>>()
            .to_sexp()
    }
}

impl<T: IntoSExp> IntoSExp for Option<T> {
    fn to_sexp(self) -> SExp {
        match self {
            None => NULL.clone(),
            Some(s) => s.to_sexp(),
        }
    }
}

impl<T: IntoSExp> IntoSExp for (T, T) {
    fn to_sexp(self) -> SExp {
        self.0.to_sexp().cons(self.1.to_sexp())
    }
}

impl IntoSExp for (SExp, SExp) {
    fn to_sexp(self) -> SExp {
        self.0.cons(self.1)
    }
}

impl IntoSExp for &str {
    fn to_sexp(self) -> SExp {
        SExp::Atom(AtomBuf::new(self.as_bytes().to_vec()))
    }
}

impl IntoSExp for String {
    fn to_sexp(self) -> SExp {
        SExp::Atom(AtomBuf::new(self.as_bytes().to_vec()))
    }
}

impl IntoSExp for Program {
    fn to_sexp(self) -> SExp {
        self.sexp.clone()
    }
}

impl IntoSExp for &Program {
    fn to_sexp(self) -> SExp {
        self.sexp.clone()
    }
}

impl IntoSExp for ConditionOpcode {
    fn to_sexp(self) -> SExp {
        SExp::Atom(AtomBuf::new(vec![self as u8]))
    }
}

macro_rules! impl_to_sexp_sized_bytes {
    ($($name: ident);*) => {
        $(
            impl IntoSExp for $name {
                fn to_sexp(self) -> SExp {
                    SExp::Atom(AtomBuf::new(self.as_slice().to_vec()))
                }
            }
        )*
    };
    ()=>{};
}

impl_to_sexp_sized_bytes!(
    Bytes4;
    Bytes8;
    Bytes32;
    Bytes48;
    Bytes96;
    Bytes100;
    Bytes480
);

macro_rules! impl_ints {
    ($($name: ident);*) => {
        $(
            impl IntoSExp for $name {
                fn to_sexp(self) -> SExp {
                    if self == 0 {
                        return SExp::Atom(AtomBuf::new(vec![]));
                    }
                    let as_ary = self.to_be_bytes();
                    let mut as_bytes = as_ary.as_slice();
                    while as_bytes.len() > 1 && as_bytes[0] == ( if as_bytes[1] & 0x80 > 0{0xFF} else {0}) {
                        as_bytes = &as_bytes[1..];
                    }
                    SExp::Atom(AtomBuf::new(as_bytes.to_vec()))
                }
            }
        )*
    };
    ()=>{};
}

impl_ints!(
    u8;
    u16;
    u32;
    u64;
    u128;
    i8;
    i16;
    i32;
    i64;
    i128
);
