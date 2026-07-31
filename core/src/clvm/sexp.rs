use crate::blockchain::sized_bytes::{
    Bytes4, Bytes8, Bytes32, Bytes48, Bytes64, Bytes96, Bytes100, Bytes480,
};
use crate::clvm::assemble::is_hex;
use crate::clvm::parser::{sexp_from_bytes, sexp_to_bytes};
use crate::clvm::program::Program;
use crate::clvm::sexp_ext::SExpNumber;
use crate::constants::{
    ADD, APPLY, CONS, DIV, DIVMOD, KEYWORD_FROM_ATOM, MUL, NULL_SEXP, ONE_SEXP, QUOTE, SUB,
};
use crate::errors::ClvmError;
use crate::formatting::{bigint_to_bytes, number_from_slice, u32_from_slice, u64_from_bigint};
use crate::traits::SizedBytes;
use crate::utils::hash_256;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hex::encode;
use num_bigint::{BigInt, Sign};
use num_traits::Zero;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Error};
use std::mem::replace;
use std::ops::Deref;
use std::sync::Arc;
use time::{OffsetDateTime, PrimitiveDateTime};

pub enum SExpSource<'a> {
    Owned(SExp<'a>),
    Borrowed(&'a SExp<'a>),
}
impl<'a> Deref for SExpSource<'a> {
    type Target = SExp<'a>;
    fn deref(&self) -> &Self::Target {
        match self {
            SExpSource::Owned(owned) => owned,
            SExpSource::Borrowed(borrowed) => borrowed,
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum SExp<'a> {
    Atom(AtomBuf<'a>),
    Pair(PairBuf<'a>),
}
impl<'a> Default for SExp<'a> {
    fn default() -> SExp<'a> {
        NULL_SEXP
    }
}

impl<'a> SExp<'a> {
    pub fn to_owned(&self) -> SExp<'static> {
        match self {
            SExp::Atom(atom) => SExp::Atom(atom.to_owned()),
            SExp::Pair(pair) => SExp::Pair(pair.to_owned()),
        }
    }
    #[inline(always)]
    pub fn maybe_atom(&self) -> Option<&AtomBuf<'_>> {
        match self {
            SExp::Atom(a) => Some(a),
            SExp::Pair(_) => None,
        }
    }
    #[inline(always)]
    pub fn atom(&self) -> Result<&AtomBuf<'_>, ClvmError> {
        match self {
            SExp::Atom(a) => Ok(a),
            SExp::Pair(_) => Err(ClvmError::ExpectedAtomGotPair(self.to_string())),
        }
    }
    #[inline(always)]
    pub fn maybe_pair(&self) -> Option<&PairBuf<'_>> {
        match self {
            SExp::Atom(_) => None,
            SExp::Pair(p) => Some(p),
        }
    }
    #[inline(always)]
    pub fn pair(&self) -> Result<&PairBuf<'_>, ClvmError> {
        match self {
            SExp::Atom(_) => Err(ClvmError::ExpectedPairGotAtom(self.to_string())),
            SExp::Pair(p) => Ok(p),
        }
    }
    #[inline(always)]
    pub fn first(&'a self) -> Result<&'a SExp<'a>, ClvmError> {
        match self {
            SExp::Atom(_) => Err(ClvmError::ExpectedPairGotAtom(self.to_string())),
            SExp::Pair(p) => Ok(p.first()),
        }
    }
    #[inline(always)]
    pub fn rest(&'a self) -> Result<&'a SExp<'a>, ClvmError> {
        match self {
            SExp::Atom(_) => Err(ClvmError::ExpectedPairGotAtom(self.to_string())),
            SExp::Pair(p) => Ok(p.rest()),
        }
    }
    #[must_use]
    pub fn as_vec(&self) -> Option<Vec<u8>> {
        match &self {
            SExp::Atom(vec) => Some(vec.as_ref().to_vec()),
            SExp::Pair(_) => None,
        }
    }

    pub fn as_int(&self) -> Result<SExpNumber, ClvmError> {
        match self {
            SExp::Atom(atom) => Ok(SExpNumber::from(atom)),
            SExp::Pair(_) => Err(ClvmError::ExpectedAtomGotPair(self.to_string())),
        }
    }
    #[must_use]
    pub fn cons(self, other: SExp<'a>) -> SExp<'a> {
        SExp::Pair(PairBuf::Owned((
            Arc::new(self.to_owned()),
            Arc::new(other.to_owned()),
        )))
    }
    pub fn split(&'a self) -> Result<(&'a SExp<'a>, &'a SExp<'a>), ClvmError> {
        match self {
            SExp::Atom(_) => Err(ClvmError::ExpectedPairGotAtom(self.to_string())),
            SExp::Pair(p) => Ok((p.first(), p.rest())),
        }
    }

    #[must_use]
    pub fn nullp(&self) -> bool {
        match &self {
            SExp::Atom(a) => a.as_ref().is_empty(),
            SExp::Pair(_) => false,
        }
    }

    #[must_use]
    pub fn as_atom_list(&self) -> Vec<Vec<u8>> {
        match self {
            SExp::Atom(_) => {
                vec![]
            }
            SExp::Pair(pair) => match pair.first() {
                SExp::Atom(buf) => {
                    let mut rtn: Vec<Vec<u8>> = vec![buf.as_ref().to_vec()];
                    rtn.extend(pair.rest().as_atom_list());
                    rtn
                }
                SExp::Pair(_) => {
                    vec![]
                }
            },
        }
    }

    pub fn to_map(&self) -> Result<HashMap<&SExp<'_>, &SExp<'_>>, ClvmError> {
        let mut rtn: HashMap<&SExp, &SExp> = HashMap::new();
        let mut cur_node = self;
        loop {
            match cur_node {
                SExp::Atom(_) => break,
                SExp::Pair(pair) => {
                    cur_node = pair.rest();
                    match pair.first() {
                        SExp::Atom(_) => {
                            rtn.insert(pair.first(), &NULL_SEXP);
                        }
                        SExp::Pair(inner_pair) => {
                            rtn.insert(inner_pair.first(), inner_pair.rest());
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
                    ptr = pair.rest();
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
        while let SExp::Pair(pair) = ptr {
            ptr = pair.rest();
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
                byte_buf.extend(pair.first().tree_hash());
                byte_buf.extend(pair.rest().tree_hash());
                hash_256(&byte_buf).into()
            }
            SExp::Atom(atom) => {
                let mut byte_buf = Vec::new();
                byte_buf.push(1);
                byte_buf.extend(atom.as_ref());
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
        self.non_nil()
    }

    #[must_use]
    pub fn from_bool(b: bool) -> &'static SExp<'static> {
        if b { &ONE_SEXP } else { &NULL_SEXP }
    }

    #[must_use]
    pub fn ref_list(&self) -> Vec<&SExp<'_>> {
        let mut args = Vec::with_capacity(8);
        let mut args_sexp = self;
        loop {
            match args_sexp {
                SExp::Atom(_) => {
                    if args_sexp.non_nil() {
                        args.push(args_sexp);
                    }
                    return args;
                }
                SExp::Pair(buf) => {
                    args.push(buf.first());
                    args_sexp = buf.rest();
                }
            }
        }
    }

    #[must_use]
    pub fn owned_list(&self, store: bool) -> Vec<SExp<'_>> {
        let mut args = Vec::with_capacity(8);
        let mut args_sexp = self;
        loop {
            match args_sexp {
                SExp::Atom(_) => {
                    return if args_sexp.non_nil() {
                        return vec![];
                    } else {
                        args
                    };
                }
                SExp::Pair(buf) => {
                    if store {
                        args.push(buf.first().clone());
                    }
                    args_sexp = buf.rest();
                }
            }
        }
    }

    #[must_use]
    pub fn non_nil(&self) -> bool {
        match self {
            SExp::Pair(_) => true,
            SExp::Atom(b) => !b.as_ref().is_empty(),
        }
    }

    pub fn substr(&self, start: usize, end: usize) -> Result<SExp<'_>, ClvmError> {
        let atom = &self.atom()?.as_ref();
        if start > atom.len() {
            return Err(ClvmError::InvalidInput(format!(
                "substr start out of bounds: {start} is > {}",
                atom.len()
            )));
        }
        if end > atom.len() {
            return Err(ClvmError::InvalidInput(format!(
                "substr end out of bounds: {end} is > {}",
                atom.len()
            )));
        }
        if end < start {
            return Err(ClvmError::InvalidInput(format!(
                "substr invalid bounds: {start} is > {end}"
            )));
        }
        let sub = SExp::Atom(AtomBuf::Owned(atom[start..end].to_vec().into()));
        Ok(sub)
    }

    pub fn concat(nodes: &'_ [&'_ SExp]) -> Result<SExp<'static>, ClvmError> {
        let mut buf = vec![];
        for node in nodes {
            let atom = node.atom()?;
            buf.extend(atom.as_ref());
        }
        let new_atom = SExp::Atom(AtomBuf::Owned(buf.into()));
        Ok(new_atom)
    }
}

const PRINTABLE: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ#!$%&'()*+,-./:;<=>?@[\\]^_`{|}~\"\r\n ";

impl<'a> Display for SExp<'a> {
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

impl<'a> Debug for SExp<'a> {
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

impl<'a> IntoIterator for &'a SExp<'a> {
    type Item = &'a SExp<'a>;
    type IntoIter = SExpIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
pub struct SExpIter<'a> {
    c: &'a SExp<'a>,
}

impl<'a> Iterator for SExpIter<'a> {
    type Item = &'a SExp<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.c.nullp() {
            None
        } else {
            match self.c {
                SExp::Atom(a) => {
                    if a.as_ref().is_empty() {
                        None
                    } else {
                        let rtn = replace(&mut self.c, &NULL_SEXP);
                        Some(rtn)
                    }
                }
                SExp::Pair(pair) => {
                    self.c = pair.rest();
                    Some(pair.first())
                }
            }
        }
    }
}

#[derive(Clone, Eq)]
pub enum AtomBuf<'a> {
    Owned(Arc<Vec<u8>>),
    Borrowed(&'a [u8]),
}

impl<'a> AtomBuf<'a> {
    pub fn to_owned(&self) -> AtomBuf<'static> {
        match self {
            AtomBuf::Owned(buf) => AtomBuf::Owned(buf.clone()),
            AtomBuf::Borrowed(b) => AtomBuf::Owned(b.to_vec().into()),
        }
    }
}

impl<'a> AsRef<[u8]> for AtomBuf<'a> {
    fn as_ref(&self) -> &[u8] {
        match self {
            AtomBuf::Owned(buf) => buf,
            AtomBuf::Borrowed(buf) => buf,
        }
    }
}

impl<'a> Serialize for AtomBuf<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_ref().serialize(serializer)
    }
}
struct AtomBufVisitor;
impl<'de> Visitor<'de> for AtomBufVisitor {
    type Value = AtomBuf<'static>;

    fn expecting(&self, formatter: &mut Formatter) -> fmt::Result {
        formatter.write_str("Expecting a hex String, or byte array")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let data = hex::decode(value).map_err(|e| E::custom(format!("{}", e)))?;
        Ok(Self::Value::new(data))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let data = hex::decode(value.as_str()).map_err(|e| E::custom(format!("{}", e)))?;
        Ok(Self::Value::new(data))
    }
    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Self::Value::new(v.to_vec()))
    }
    fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Self::Value::new(v.to_vec()))
    }
    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(b) = seq.next_element::<u8>()? {
            out.push(b);
        }
        Ok(AtomBuf::new(out))
    }
}

impl<'de> Deserialize<'de> for AtomBuf<'_> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match deserializer.deserialize_any(AtomBufVisitor) {
            Ok(hex) => Ok(hex),
            Err(er) => Err(er),
        }
    }
}

impl<'a> Debug for AtomBuf<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl<'a> Display for AtomBuf<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        display_atom(self.as_ref(), f)
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

impl AtomBuf<'static> {
    #[must_use]
    pub fn new(v: Vec<u8>) -> Self {
        AtomBuf::Owned(v.into())
    }
}

impl<'a> AtomBuf<'a> {
    #[must_use]
    pub fn new_ref(v: &'a [u8]) -> Self {
        AtomBuf::Borrowed(v)
    }
    pub fn as_int(&self) -> BigInt {
        number_from_slice(self.as_ref())
    }
    pub fn as_u64(&self) -> Result<u64, ClvmError> {
        u64_from_bigint(&number_from_slice(self.as_ref())).map_err(ClvmError::IoError)
    }
    pub fn as_u32(&self) -> Option<u32> {
        u32_from_slice(self.as_ref())
    }
    pub fn as_i32(&self) -> Option<u32> {
        u32_from_slice(self.as_ref())
    }
}
impl From<Vec<u8>> for AtomBuf<'static> {
    fn from(v: Vec<u8>) -> Self {
        Self::new(v)
    }
}
impl From<&Vec<u8>> for AtomBuf<'static> {
    fn from(v: &Vec<u8>) -> Self {
        Self::new(v.clone())
    }
}
impl<'a> From<&'a [u8]> for AtomBuf<'a> {
    fn from(v: &'a [u8]) -> Self {
        Self::new_ref(v)
    }
}
impl<'a, T: AsRef<[u8]>> PartialEq<T> for AtomBuf<'a> {
    fn eq(&self, other: &T) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl<'a> PartialEq<[u8]> for &AtomBuf<'a> {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_ref() == other
    }
}
impl<'a> Hash for AtomBuf<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&[0x01]);
        self.as_ref().hash(state)
    }
}
#[derive(Clone, Eq)]
pub enum PairBuf<'a> {
    Owned((Arc<SExp<'static>>, Arc<SExp<'static>>)),
    Borrowed((&'a SExp<'a>, &'a SExp<'a>)),
}

impl<'a> PairBuf<'a> {
    pub fn to_owned(&'a self) -> PairBuf<'static> {
        match self {
            PairBuf::Owned((first, rest)) => PairBuf::Owned((first.clone(), rest.clone())),
            PairBuf::Borrowed((first, rest)) => {
                PairBuf::Owned(((*first).to_owned().into(), (*rest).to_owned().into()))
            }
        }
    }
}
impl<'a> PartialEq for PairBuf<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.first() == other.first() && self.rest() == other.rest()
    }
}
impl<'a> Hash for PairBuf<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(&[0x02]);
        self.first().hash(state);
        self.rest().hash(state);
    }
}
impl<'a> PairBuf<'a> {
    pub fn first(&self) -> &SExp<'_> {
        match self {
            PairBuf::Owned((first, _)) => first.as_ref(),
            PairBuf::Borrowed((first, _)) => first,
        }
    }
    pub fn rest(&self) -> &SExp<'_> {
        match self {
            PairBuf::Owned((_, rest)) => rest.as_ref(),
            PairBuf::Borrowed((_, rest)) => rest,
        }
    }
}

impl<'a> Serialize for PairBuf<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        (self.first(), self.rest()).serialize(serializer)
    }
}

impl<'a, 'de> Deserialize<'de> for PairBuf<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (first, rest): (SExp, SExp) = Deserialize::deserialize(deserializer)?;
        Ok(PairBuf::Owned((Arc::new(first), Arc::new(rest))))
    }
}

impl<'a> Display for PairBuf<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut buffer = String::from("(");
        match self.first() {
            SExp::Atom(a) => {
                if let Some(kw) = KEYWORD_FROM_ATOM.get(a.as_ref()) {
                    buffer += kw;
                } else {
                    buffer += &format!("{}", self.first());
                }
            }
            SExp::Pair(_) => {
                buffer += &format!("{}", self.first());
            }
        }
        let mut current = self.rest();
        while let SExp::Pair(p) = current {
            buffer += &format!(" {}", p.first());
            current = p.rest();
        }
        if current.non_nil() {
            buffer += &format!(" . {}", *current);
        }
        buffer += ")";
        write!(f, "{buffer}")
    }
}

impl<'a> Debug for PairBuf<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut buffer = String::from("(");
        let mut is_quote = false;
        let mut is_op_format = false;
        match self.first() {
            SExp::Atom(a) => {
                let data = a.as_ref();
                if data.len() == 1 {
                    is_quote = data.first() == Some(&QUOTE);
                    is_op_format = data.first() == Some(&CONS);
                    is_op_format = is_op_format || data.first() == Some(&APPLY);
                    is_op_format = is_op_format || data.first() == Some(&ADD);
                    is_op_format = is_op_format || data.first() == Some(&SUB);
                    is_op_format = is_op_format || data.first() == Some(&MUL);
                    is_op_format = is_op_format || data.first() == Some(&DIV);
                    is_op_format = is_op_format || data.first() == Some(&DIVMOD);
                }
                if let Some(kw) = KEYWORD_FROM_ATOM.get(data) {
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
            let mut current = self.rest();
            match current {
                SExp::Atom(a) => {
                    buffer += &format!(" . {a:?}");
                }
                SExp::Pair(_) => {
                    while let SExp::Pair(p) = current {
                        buffer += &format!(" {:?}", p.first());
                        current = p.rest();
                    }
                    match current {
                        SExp::Pair(pair) => {
                            buffer += &format!(" {:?}", pair);
                        }
                        SExp::Atom(atom) => {
                            if !atom.as_ref().is_empty() {
                                buffer += &format!(" {:?}", atom);
                            }
                        }
                    }
                }
            }
            buffer += ")";
        } else if is_op_format {
            let cons_pair = self.rest();
            match cons_pair {
                SExp::Pair(pair) => {
                    buffer += &format!(" {:?}", pair.first());
                    match pair.rest() {
                        SExp::Atom(a) => {
                            buffer += &format!(" {a:?}");
                        }
                        SExp::Pair(p) => {
                            if p.rest().nullp() {
                                buffer += &format!(" {:?}", p.first());
                            } else {
                                buffer += &format!(" {:?}", p);
                            }
                        }
                    }
                }
                SExp::Atom(_) => {
                    buffer += &format!(" {:?} {:?}", cons_pair, NULL_SEXP);
                }
            }
            buffer += ")";
        } else {
            buffer += &format!(" {:?}", self.rest());
            buffer += ")";
        }
        write!(f, "{buffer}")
    }
}

impl From<(&SExp<'static>, &SExp<'static>)> for PairBuf<'static> {
    fn from(v: (&SExp<'static>, &SExp<'static>)) -> Self {
        PairBuf::Owned((Arc::new(v.0.to_owned()), Arc::new(v.1.to_owned())))
    }
}

impl From<(SExp<'static>, SExp<'static>)> for PairBuf<'static> {
    fn from(v: (SExp<'static>, SExp<'static>)) -> Self {
        PairBuf::Owned((Arc::new(v.0.to_owned()), Arc::new(v.1.to_owned())))
    }
}

impl From<&OffsetDateTime> for SExp<'static> {
    fn from(date: &OffsetDateTime) -> SExp<'static> {
        date.to_sexp_ref()
    }
}
impl From<OffsetDateTime> for SExp<'static> {
    fn from(date: OffsetDateTime) -> SExp<'static> {
        date.to_sexp_ref()
    }
}
impl From<&PrimitiveDateTime> for SExp<'static> {
    fn from(date: &PrimitiveDateTime) -> SExp<'static> {
        date.to_sexp_ref()
    }
}

impl From<PrimitiveDateTime> for SExp<'static> {
    fn from(date: PrimitiveDateTime) -> SExp<'static> {
        date.to_sexp_ref()
    }
}
impl From<&SExp<'_>> for SExp<'static> {
    fn from(sexp_ref: &SExp<'_>) -> SExp<'static> {
        sexp_ref.to_owned()
    }
}

impl<'a> From<Vec<SExp<'a>>> for SExp<'a> {
    fn from(vec: Vec<SExp<'a>>) -> SExp<'a> {
        vec.as_slice().into()
    }
}

impl<const N: usize, T: ToSExpRef> From<&[T; N]> for SExp<'static> {
    fn from(ary: &[T; N]) -> SExp<'static> {
        ary.iter()
            .map(ToSExpRef::to_sexp_ref)
            .collect::<Vec<SExp<'_>>>()
            .into()
    }
}

impl<'a, const N: usize> From<[&'a SExp<'a>; N]> for SExp<'a> {
    fn from(ary: [&'a SExp<'a>; N]) -> SExp<'a> {
        ary.iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<SExp<'_>>>()
            .into()
    }
}
impl<'a> From<&[SExp<'a>]> for SExp<'a> {
    fn from(values: &[SExp<'a>]) -> SExp<'a> {
        if let Some(sexp) = values.first().cloned() {
            let mut end = NULL_SEXP;
            if values.len() > 1 {
                for other in values[1..].iter().rev() {
                    end = other.clone().cons(end);
                }
            }
            sexp.cons(end)
        } else {
            NULL_SEXP.clone()
        }
    }
}

impl<'a> From<&[&SExp<'a>]> for SExp<'a> {
    fn from(values: &[&SExp<'a>]) -> SExp<'a> {
        if let Some(sexp) = values.first().cloned() {
            let mut end = NULL_SEXP.clone();
            if values.len() > 1 {
                for other in values[1..].iter().rev() {
                    end = (*other).clone().cons(end);
                }
            }
            (*sexp).clone().cons(end)
        } else {
            NULL_SEXP.clone()
        }
    }
}

impl<'a> From<Vec<Vec<SExp<'a>>>> for SExp<'a> {
    fn from(mut values: Vec<Vec<SExp<'a>>>) -> SExp<'a> {
        values.reverse();
        if let Some(sexp) = values.pop() {
            let mut end = NULL_SEXP.clone();
            if !values.is_empty() {
                for other in values.into_iter() {
                    end = SExp::from(other).cons(end);
                }
            }
            SExp::from(sexp).cons(end)
        } else {
            NULL_SEXP.clone()
        }
    }
}

impl<T: ToSExpRef> From<&Vec<T>> for SExp<'static> {
    fn from(s: &Vec<T>) -> SExp<'static> {
        s.iter()
            .map(ToSExpRef::to_sexp_ref)
            .collect::<Vec<SExp<'_>>>()
            .into()
    }
}

impl<T: Into<SExp<'static>>> From<Option<T>> for SExp<'static> {
    fn from(optional: Option<T>) -> SExp<'static> {
        match optional {
            None => NULL_SEXP.clone(),
            Some(s) => s.into(),
        }
    }
}
trait ToSExpRef {
    fn to_sexp_ref(&self) -> SExp<'static>;
}
impl<T> ToSExpRef for T
where
    SExp<'static>: for<'a> From<&'a T>,
{
    fn to_sexp_ref(&self) -> SExp<'static> {
        SExp::from(self)
    }
}
impl<T> From<&Option<T>> for SExp<'static>
where
    T: ToSExpRef,
{
    fn from(optional: &Option<T>) -> SExp<'static> {
        match optional {
            None => NULL_SEXP.clone(),
            Some(s) => s.to_sexp_ref(),
        }
    }
}

impl<T, U, V> From<&[(T, U, V)]> for SExp<'static>
where
    T: ToSExpRef,
    U: ToSExpRef,
    V: ToSExpRef,
{
    fn from(val: &[(T, U, V)]) -> SExp<'static> {
        val.iter()
            .map(|(t, u, v)| {
                (
                    t.to_sexp_ref(),
                    SExp::from((u.to_sexp_ref(), v.to_sexp_ref())),
                )
                    .into()
            })
            .collect::<Vec<SExp<'static>>>()
            .into()
    }
}

impl<'a> From<&str> for SExp<'a> {
    fn from(string: &str) -> SExp<'a> {
        SExp::Atom(AtomBuf::new(string.as_bytes().to_vec()))
    }
}

impl<'a> From<String> for SExp<'a> {
    fn from(string: String) -> SExp<'a> {
        SExp::Atom(AtomBuf::new(string.into_bytes()))
    }
}

impl<'a> From<&String> for SExp<'a> {
    fn from(string: &String) -> SExp<'a> {
        string.clone().into()
    }
}

impl<'a> From<&'a Program<'a>> for SExp<'a> {
    fn from(program: &'a Program) -> SExp<'a> {
        program.sexp().to_owned()
    }
}

impl<'a> From<Vec<u8>> for SExp<'a> {
    fn from(vec: Vec<u8>) -> SExp<'a> {
        SExp::Atom(AtomBuf::new(vec))
    }
}
impl<'a> From<Vec<Vec<u8>>> for SExp<'a> {
    fn from(vec: Vec<Vec<u8>>) -> SExp<'a> {
        vec.into_iter()
            .map(Into::into)
            .collect::<Vec<SExp<'static>>>()
            .into()
    }
}

impl<'a> From<&BigInt> for SExp<'a> {
    fn from(int: &BigInt) -> SExp<'a> {
        SExp::Atom(AtomBuf::Owned(Arc::new(bigint_to_bytes(
            int,
            int.sign() != Sign::NoSign,
        ))))
    }
}

impl<'a, 'b, T, U> From<(T, U)> for SExp<'static>
where
    T: Into<SExp<'a>>,
    U: Into<SExp<'b>>,
{
    fn from(val: (T, U)) -> SExp<'static> {
        SExp::Pair(PairBuf::Owned((
            val.0.into().to_owned().into(),
            val.1.into().to_owned().into(),
        )))
    }
}

impl<T, U> From<&(T, U)> for SExp<'static>
where
    T: ToSExpRef,
    U: ToSExpRef,
{
    fn from(val: &(T, U)) -> SExp<'static> {
        (val.0.to_sexp_ref(), val.1.to_sexp_ref()).into()
    }
}

impl<T, U, V> From<&(T, U, V)> for SExp<'static>
where
    T: ToSExpRef,
    U: ToSExpRef,
    V: ToSExpRef,
{
    fn from(val: &(T, U, V)) -> SExp<'static> {
        SExp::from((
            val.0.to_sexp_ref(),
            SExp::from((val.1.to_sexp_ref(), val.2.to_sexp_ref())),
        ))
    }
}

impl<'a, 'b, 'c, T, U, V> From<(T, U, V)> for SExp<'static>
where
    T: Into<SExp<'a>>,
    U: Into<SExp<'b>>,
    V: Into<SExp<'c>>,
{
    fn from(val: (T, U, V)) -> SExp<'static> {
        SExp::Pair(PairBuf::Owned((
            val.0.into().to_owned().into(),
            SExp::Pair(PairBuf::Owned((
                val.1.into().to_owned().into(),
                val.2.into().to_owned().into(),
            )))
            .into(),
        )))
    }
}

impl<'a, 'b, T, U> From<Vec<(T, U)>> for SExp<'static>
where
    T: Into<SExp<'a>>,
    U: Into<SExp<'b>>,
{
    fn from(val: Vec<(T, U)>) -> SExp<'static> {
        val.into_iter()
            .map(Into::into)
            .collect::<Vec<SExp<'static>>>()
            .into()
    }
}

impl<'a, 'b, 'c, T, U, V> From<Vec<(T, U, V)>> for SExp<'static>
where
    T: Into<SExp<'a>>,
    U: Into<SExp<'b>>,
    V: Into<SExp<'c>>,
{
    fn from(val: Vec<(T, U, V)>) -> SExp<'static> {
        val.into_iter()
            .map(Into::into)
            .map(|s: SExp<'_>| s.to_owned())
            .collect::<Vec<SExp<'static>>>()
            .into()
    }
}

impl<'a> From<u8> for SExp<'a> {
    fn from(u: u8) -> SExp<'a> {
        if u == 0 {
            NULL_SEXP
        } else {
            SExp::Atom(AtomBuf::new(vec![u]))
        }
    }
}

impl<'a> From<&u8> for SExp<'a> {
    fn from(u: &u8) -> SExp<'a> {
        (*u).into()
    }
}

impl<'a> From<&[u8]> for SExp<'a> {
    fn from(ary: &[u8]) -> SExp<'a> {
        SExp::Atom(AtomBuf::new(ary.to_vec()))
    }
}
impl<'a, 'b, K, V> From<HashMap<K, V>> for SExp<'static>
where
    K: Into<SExp<'a>>,
    V: Into<SExp<'b>>,
{
    fn from(m: HashMap<K, V>) -> SExp<'static> {
        let pairs: Vec<SExp<'static>> = m
            .into_iter()
            .map(|(k, v)| -> SExp<'static> {
                let k: SExp<'a> = k.into();
                let v: SExp<'b> = v.into();
                (k, v).into() // pair SExp
            })
            .collect();
        pairs.into() // list SExp
    }
}

impl<K, V> From<&HashMap<K, V>> for SExp<'static>
where
    K: ToSExpRef,
    V: ToSExpRef,
{
    fn from(m: &HashMap<K, V>) -> SExp<'static> {
        let pairs: Vec<SExp<'static>> = m
            .iter()
            .map(|(k, v)| {
                let k = k.to_sexp_ref();
                let v = v.to_sexp_ref();
                (k, v).into()
            })
            .collect();
        pairs.into()
    }
}

macro_rules! impl_to_sexp_sized_bytes {
    ($($name: ident);*) => {
        $(
            impl From<$name> for SExp<'static> {
                fn from(bytes: $name) -> SExp<'static> {
                    SExp::Atom(AtomBuf::new(bytes.bytes().to_vec()))
                }
            }
            impl From<&$name> for SExp<'static> {
                fn from(bytes: &$name) -> SExp<'static> {
                    SExp::Atom(AtomBuf::new(bytes.bytes().to_vec()))
                }
            }
            impl From<Vec<$name>> for SExp<'static> {
                fn from(vals: Vec<$name>) -> SExp<'static> {
                    vals.iter()
                        .map(Into::into)
                        .collect::<Vec<SExp<'_>>>()
                        .into()
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
    Bytes64;
    Bytes96;
    Bytes100;
    Bytes480
);

macro_rules! impl_ints {
    ($($name: ident);*) => {
        $(
            impl From<$name> for SExp<'static> {
                fn from(num: $name) -> SExp<'static> {
                    if num == 0 {
                        return NULL_SEXP;
                    }
                    let as_ary = num.to_be_bytes();
                    let mut as_bytes = as_ary.as_slice();
                    while as_bytes.len() > 1 && as_bytes[0] == ( (as_bytes[1] & 0x80 > 0) as u8 * 0xFF) {
                        as_bytes = &as_bytes[1..];
                    }
                    SExp::Atom(AtomBuf::new(as_bytes.to_vec()))
                }
            }
            impl From<&$name> for SExp<'static> {
                fn from(num: &$name) -> SExp<'static> {
                    SExp::from(*num)
                }
            }
            impl From<Vec<$name>> for SExp<'static> {
                fn from(vals: Vec<$name>) -> SExp<'static> {
                    (&vals).into()
                }
            }
            impl From<&[Vec<$name>]> for SExp<'static> {
                fn from(vals: &[Vec<$name>]) -> SExp<'static> {
                    vals.iter().map(Into::into).collect::<Vec<SExp<'static>>>().into()
                }
            }
            impl From<&[$name]> for SExp<'static> {
                fn from(vals: &[$name]) -> SExp<'static> {
                    vals.iter().map(Into::into).collect::<Vec<SExp<'static>>>().into()
                }
            }
        )*
    };
    ()=>{};
}

impl_ints!(
    usize;
    u16;
    u32;
    u64;
    u128;
    isize;
    i8;
    i16;
    i32;
    i64;
    i128
);

impl ChiaSerialize for SExp<'_> {
    fn to_bytes(&self, _version: ChiaProtocolVersion) -> Result<Vec<u8>, Error>
    where
        Self: Sized,
    {
        sexp_to_bytes(self).map(|v| v.as_ref().to_vec())
    }

    fn from_bytes(bytes: &mut Cursor<&[u8]>, _version: ChiaProtocolVersion) -> Result<Self, Error>
    where
        Self: Sized,
    {
        sexp_from_bytes(bytes).map_err(Into::into)
    }
}
impl From<f64> for SExp<'static> {
    fn from(num: f64) -> SExp<'static> {
        if num.is_zero() {
            return NULL_SEXP;
        }
        let as_ary = num.to_be_bytes();
        let mut as_bytes = as_ary.as_slice();
        while as_bytes.len() > 1 && as_bytes[0] == ((as_bytes[1] & 0x80 > 0) as u8 * 0xFF) {
            as_bytes = &as_bytes[1..];
        }
        SExp::Atom(AtomBuf::new(as_bytes.to_vec()))
    }
}
impl From<&f64> for SExp<'static> {
    fn from(num: &f64) -> SExp<'static> {
        SExp::from(*num)
    }
}
impl From<bool> for SExp<'static> {
    fn from(num: bool) -> SExp<'static> {
        if num {
            SExp::Atom(AtomBuf::Borrowed(&[1]))
        } else {
            NULL_SEXP
        }
    }
}
impl From<&bool> for SExp<'static> {
    fn from(num: &bool) -> SExp<'static> {
        SExp::from(*num)
    }
}

#[inline]
fn trim_twos_complement_be(mut b: &[u8]) -> &[u8] {
    while b.len() > 1 && b[0] == (((b[1] & 0x80) != 0) as u8) * 0xFF {
        b = &b[1..];
    }
    b
}

macro_rules! impl_nz_ints {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<$ty> for SExp<'static> {
                #[inline]
                fn from(num: $ty) -> SExp<'static> {
                    let raw = num.get().to_be_bytes();
                    let trimmed = trim_twos_complement_be(&raw);
                    SExp::Atom(AtomBuf::new(trimmed.to_vec()))
                }
            }

            impl From<&$ty> for SExp<'static> {
                #[inline]
                fn from(num: &$ty) -> SExp<'static> {
                    SExp::from(*num)
                }
            }

            impl From<Vec<$ty>> for SExp<'static> {
                #[inline]
                fn from(vals: Vec<$ty>) -> SExp<'static> {
                    (&vals[..]).into()
                }
            }

            impl From<&[$ty]> for SExp<'static> {
                #[inline]
                fn from(vals: &[$ty]) -> SExp<'static> {
                    vals.iter().map(Into::into).collect::<Vec<SExp<'static>>>().into()
                }
            }

            impl From<&[Vec<$ty>]> for SExp<'static> {
                #[inline]
                fn from(vals: &[Vec<$ty>]) -> SExp<'static> {
                    vals.iter().map(Into::into).collect::<Vec<SExp<'static>>>().into()
                }
            }
        )+
    };
}

impl_nz_ints!(
    NonZeroUsize,
    NonZeroU8,
    NonZeroU16,
    NonZeroU32,
    NonZeroU64,
    NonZeroU128,
    NonZeroIsize,
    NonZeroI8,
    NonZeroI16,
    NonZeroI32,
    NonZeroI64,
    NonZeroI128,
);
