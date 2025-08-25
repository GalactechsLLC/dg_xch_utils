use crate::blockchain::condition_opcode::ConditionOpcode;
use crate::blockchain::sized_bytes::{
    Bytes100, Bytes32, Bytes4, Bytes48, Bytes480, Bytes8, Bytes96,
};
use crate::clvm::assemble::is_hex;
use crate::clvm::parser::{sexp_from_bytes, sexp_to_bytes};
use crate::clvm::program::Program;
use crate::constants::{
    ADD, APPLY, CONS, DIV, DIVMOD, KEYWORD_FROM_ATOM, MUL, NULL_CELL, NULL_SEXP, ONE_SEXP, QUOTE,
    SUB,
};
use crate::formatting::{number_from_slice, u32_from_slice, u64_from_bigint};
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hex::encode;
use num_bigint::BigInt;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::{Cursor, Error, ErrorKind};
use std::mem::replace;
use std::sync::Arc;

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
impl Default for SExp {
    fn default() -> SExp {
        NULL_SEXP.clone()
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
    pub fn as_vec(&self) -> Option<Vec<u8>> {
        match &self {
            SExp::Atom(vec) => Some(vec.data.clone()),
            SExp::Pair(_) => None,
        }
    }

    pub fn as_int(&self) -> Result<BigInt, Error> {
        match self {
            SExp::Atom(atom) => Ok(BigInt::from_signed_bytes_be(&atom.data)),
            SExp::Pair(_) => Err(Error::new(ErrorKind::Unsupported, "SExp is Pair not Atom")),
        }
    }
    #[must_use]
    pub fn cons(self, other: SExp) -> SExp {
        SExp::Pair(PairBuf {
            first: Arc::new(self),
            rest: Arc::new(other),
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

    pub fn to_map(&self) -> Result<HashMap<Arc<SExp>, Arc<SExp>>, Error> {
        let mut rtn: HashMap<Arc<SExp>, Arc<SExp>> = HashMap::new();
        let mut cur_node = self;
        loop {
            match cur_node {
                SExp::Atom(_) => break,
                SExp::Pair(pair) => {
                    cur_node = &pair.rest;
                    match pair.first.as_ref() {
                        SExp::Atom(_) => {
                            rtn.insert(pair.first.clone(), NULL_CELL.clone());
                        }
                        SExp::Pair(inner_pair) => {
                            rtn.insert(inner_pair.first.clone(), inner_pair.rest.clone());
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
    pub fn arg_count(&self, return_early_if_exceeds: usize) -> usize {
        let mut count = 0;
        let mut ptr = self;
        while let Ok(pair) = ptr.pair() {
            ptr = &pair.rest;
            count += 1;
            if count > return_early_if_exceeds {
                break;
            };
        }
        count
    }

    #[must_use]
    pub fn tree_hash(&self) -> Bytes32 {
        match self {
            SExp::Pair(pair) => {
                let mut byte_buf = Vec::new();
                byte_buf.push(2);
                byte_buf.extend(pair.first.tree_hash());
                byte_buf.extend(pair.rest.tree_hash());
                hash_256(&byte_buf).into()
            }
            SExp::Atom(atom) => {
                let mut byte_buf = Vec::new();
                byte_buf.push(1);
                byte_buf.extend(&atom.data);
                hash_256(&byte_buf).into()
            }
        }
    }

    #[must_use]
    pub fn iter(&self) -> SExpIter<'_> {
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
            &ONE_SEXP
        } else {
            &NULL_SEXP
        }
    }

    #[must_use]
    pub fn proper_list(&self, store: bool) -> Option<Vec<SExp>> {
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
                        args.push(buf.first.as_ref().clone());
                    }
                    args_sexp = &buf.rest;
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

    pub fn substr(&self, start: usize, end: usize) -> Result<SExp, Error> {
        let atom = &self.atom()?.data;
        if start > atom.len() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("substr start out of bounds: {start} is > {}", atom.len()),
            ));
        }
        if end > atom.len() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("substr end out of bounds: {end} is > {}", atom.len()),
            ));
        }
        if end < start {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("substr invalid bounds: {start} is > {end}"),
            ));
        }
        let sub = SExp::Atom(AtomBuf {
            data: atom[start..end].to_vec(),
        });
        Ok(sub)
    }

    pub fn concat<'a>(nodes: &'a [&'a SExp]) -> Result<SExp, Error> {
        let mut buf = vec![];
        for node in nodes {
            let atom = node.atom()?;
            buf.extend(&atom.data);
        }
        let new_atom = SExp::Atom(AtomBuf { data: buf });
        Ok(new_atom)
    }
}

const PRINTABLE: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ#!$%&'()*+,-./:;<=>?@[\\]^_`{|}~\"\r\n ";

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

impl TryFrom<&BigInt> for SExp {
    type Error = Error;
    fn try_from(value: &BigInt) -> Result<Self, Self::Error> {
        let bytes: Vec<u8> = value.to_signed_bytes_be();
        let mut slice = bytes.as_slice();
        // make number minimal by removing leading zeros
        while (!slice.is_empty()) && (slice[0] == 0) {
            if slice.len() > 1 && (slice[1] & 0x80 == 0x80) {
                break;
            }
            slice = &slice[1..];
        }
        Ok(SExp::Atom(slice.to_vec().into()))
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
                        let rtn = replace(&mut self.c, &NULL_SEXP);
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
        write!(f, "{self}")
    }
}

impl Display for AtomBuf {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        display_atom(&self.data, f)
    }
}

pub struct AtomRef<'a> {
    pub data: &'a [u8],
}

impl Debug for AtomRef<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl Display for AtomRef<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        display_atom(self.data, f)
    }
}

fn display_atom(data: &[u8], f: &mut Formatter<'_>) -> fmt::Result {
    if data.is_empty() {
        f.write_str("()")
    } else if data.len() > 2 {
        match String::from_utf8(data.to_vec()) {
            Ok(as_utf8) => {
                for s in as_utf8.chars() {
                    if !PRINTABLE.contains(&s.to_string()) {
                        return f.write_str(&format!("0x{}", encode(data)));
                    }
                }
                if as_utf8.contains('"') && as_utf8.contains('\'') {
                    f.write_str(&format!("0x{}", encode(data)))
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
            Err(_) => f.write_str(&format!("0x{}", encode(data))),
        }
    } else if data == BigInt::from_signed_bytes_be(data).to_signed_bytes_be() {
        f.write_str(&format!("{}", BigInt::from_signed_bytes_be(data)))
    } else {
        f.write_str(&format!("0x{}", encode(data)))
    }
}

impl AtomBuf {
    #[must_use]
    pub fn new(v: Vec<u8>) -> Self {
        AtomBuf { data: v }
    }
    pub fn as_bytes32(&self) -> Result<Bytes32, Error> {
        if self.data.len() == Bytes32::SIZE {
            Bytes32::parse(self.data.as_slice())
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid Length for Bytes32: {}", self.data.len()),
            ))
        }
    }
    pub fn as_int(&self) -> BigInt {
        number_from_slice(&self.data)
    }
    pub fn as_u64(&self) -> Result<u64, Error> {
        u64_from_bigint(&number_from_slice(&self.data))
    }
    pub fn as_u32(&self) -> Option<u32> {
        u32_from_slice(&self.data)
    }
    pub fn as_i32(&self) -> Option<u32> {
        u32_from_slice(&self.data)
    }
}

impl<'a> AtomRef<'a> {
    #[must_use]
    pub fn new(v: &'a [u8]) -> AtomRef<'a> {
        AtomRef { data: v }
    }
    pub fn as_bytes32(&self) -> Result<Bytes32, Error> {
        if self.data.len() == Bytes32::SIZE {
            Bytes32::parse(self.data)
        } else {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid Length for Bytes32: {}", self.data.len()),
            ))
        }
    }
    pub fn as_int(&self) -> BigInt {
        number_from_slice(self.data)
    }
    pub fn as_u64(&self) -> Result<u64, Error> {
        u64_from_bigint(&number_from_slice(self.data))
    }
    pub fn as_u32(&self) -> Option<u32> {
        u32_from_slice(self.data)
    }
    pub fn as_i32(&self) -> Option<u32> {
        u32_from_slice(self.data)
    }
}
impl<T: AsRef<[u8]>> From<T> for AtomBuf {
    fn from(v: T) -> Self {
        Self::new(v.as_ref().to_vec())
    }
}
impl<T: AsRef<[u8]>> PartialEq<T> for AtomBuf {
    fn eq(&self, other: &T) -> bool {
        self.data == other.as_ref()
    }
}

impl PartialEq<[u8]> for &AtomBuf {
    fn eq(&self, other: &[u8]) -> bool {
        self.data == other
    }
}
#[derive(Hash, Clone, PartialEq, Eq)]
pub struct PairBuf {
    pub first: Arc<SExp>,
    pub rest: Arc<SExp>,
}

impl Serialize for PairBuf {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (&*self.first, &*self.rest).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PairBuf {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (first, rest): (SExp, SExp) = Deserialize::deserialize(deserializer)?;
        Ok(PairBuf {
            first: Arc::new(first),
            rest: Arc::new(rest),
        })
    }
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
        let mut is_quote = false;
        let mut is_op_format = false;
        match &*self.first {
            SExp::Atom(a) => {
                if a.data.len() == 1 {
                    is_quote = a.data.first() == Some(&QUOTE);
                    is_op_format = a.data.first() == Some(&CONS);
                    is_op_format = is_op_format || a.data.first() == Some(&APPLY);
                    is_op_format = is_op_format || a.data.first() == Some(&ADD);
                    is_op_format = is_op_format || a.data.first() == Some(&SUB);
                    is_op_format = is_op_format || a.data.first() == Some(&MUL);
                    is_op_format = is_op_format || a.data.first() == Some(&DIV);
                    is_op_format = is_op_format || a.data.first() == Some(&DIVMOD);
                }
                if let Some(kw) = KEYWORD_FROM_ATOM.get(&a.data) {
                    buffer += kw;
                } else {
                    buffer += &format!("{a:?}");
                }
            }
            SExp::Pair(p) => {
                buffer += &format!("{p:?}");
            }
        }
        if is_quote {
            let mut current = &self.rest;
            match current.as_ref() {
                SExp::Atom(a) => {
                    buffer += &format!(" . {a:?}");
                }
                SExp::Pair(_) => {
                    while let Ok(p) = current.pair() {
                        buffer += &format!(" {:?}", &p.first.as_ref());
                        current = &p.rest;
                    }
                    match current.as_ref() {
                        SExp::Pair(pair) => {
                            buffer += &format!(" {:?}", &pair);
                        }
                        SExp::Atom(atom) => {
                            if !atom.data.is_empty() {
                                buffer += &format!(" {:?}", &atom);
                            }
                        }
                    }
                }
            }
            buffer += ")";
        } else if is_op_format {
            let cons_pair = &self.rest;
            match cons_pair.as_ref() {
                SExp::Pair(pair) => {
                    buffer += &format!(" {:?}", &pair.first);
                    match pair.rest.as_ref() {
                        SExp::Atom(a) => {
                            buffer += &format!(" {a:?}");
                        }
                        SExp::Pair(p) => {
                            if p.rest.nullp() {
                                buffer += &format!(" {:?}", &p.first);
                            } else {
                                buffer += &format!(" {:?}", &p);
                            }
                        }
                    }
                }
                SExp::Atom(_) => {
                    buffer += &format!(" {:?} {:?}", cons_pair, &*NULL_SEXP);
                }
            }
            buffer += ")";
        } else {
            buffer += &format!(" {:?}", &self.rest);
            buffer += ")";
        }
        write!(f, "{buffer}")
    }
}

impl From<(&SExp, &SExp)> for PairBuf {
    fn from(v: (&SExp, &SExp)) -> Self {
        PairBuf {
            first: Arc::new(v.0.clone()),
            rest: Arc::new(v.1.clone()),
        }
    }
}

impl From<(SExp, SExp)> for PairBuf {
    fn from(v: (SExp, SExp)) -> Self {
        PairBuf {
            first: Arc::new(v.0),
            rest: Arc::new(v.1),
        }
    }
}

pub trait IntoSExp {
    fn to_sexp(self) -> SExp;
}

pub trait TryIntoSExp {
    fn try_to_sexp(self) -> Result<SExp, Error>;
}

impl IntoSExp for Vec<&SExp> {
    fn to_sexp(self) -> SExp {
        self.into_iter().cloned().collect::<Vec<SExp>>().to_sexp()
    }
}

impl IntoSExp for Vec<SExp> {
    fn to_sexp(self) -> SExp {
        if let Some(sexp) = self.first().cloned() {
            let mut end = NULL_SEXP.clone();
            if self.len() > 1 {
                for other in self[1..].iter().rev() {
                    end = other.clone().cons(end);
                }
            }
            sexp.cons(end)
        } else {
            NULL_SEXP.clone()
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
            None => NULL_SEXP.clone(),
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
                    SExp::Atom(AtomBuf::new(self.bytes().to_vec()))
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

impl ChiaSerialize for SExp {
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        sexp_to_bytes(self)
    }

    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut Cursor<T>,
        _version: ChiaProtocolVersion,
    ) -> Result<Self, Error>
    where
        Self: Sized,
    {
        sexp_from_bytes(bytes)
    }
}
