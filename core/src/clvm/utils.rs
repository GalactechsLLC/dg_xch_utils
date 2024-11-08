use crate::blockchain::coin::Coin;
use crate::blockchain::npc_result::NPCResult;
use crate::clvm::sexp::{AtomBuf, SExp};
use dg_xch_serialize::hash_256;
use num_bigint::BigInt;
use num_traits::{Num, Signed, Zero};
use once_cell::sync::Lazy;
use regex::Regex;
use std::io::{Error, ErrorKind};

pub const NO_NEG_DIV: u32 = 0x0001;
pub const NO_UNKNOWN_OPS: u32 = 0x0002;
pub const COND_CANON_INTS: u32 = 0x0001_0000;
pub const NO_UNKNOWN_CONDS: u32 = 0x20000;
pub const COND_ARGS_NIL: u32 = 0x40000;
pub const STRICT_ARGS_COUNT: u32 = 0x80000;
pub const MEMPOOL_MODE: u32 =
    NO_NEG_DIV | COND_CANON_INTS | NO_UNKNOWN_CONDS | NO_UNKNOWN_OPS | COND_ARGS_NIL;
pub const INFINITE_COST: u64 = 0x7FFF_FFFF_FFFF_FFFF;

#[must_use]
pub fn tree_hash(sexp: &SExp) -> Vec<u8> {
    match sexp {
        SExp::Pair(pair) => {
            let mut byte_buf = Vec::new();
            byte_buf.push(2);
            byte_buf.append(&mut tree_hash(&pair.first));
            byte_buf.append(&mut tree_hash(&pair.rest));
            hash_256(&byte_buf)
        }
        SExp::Atom(atom) => {
            let mut byte_buf = Vec::new();
            byte_buf.push(1);
            byte_buf.extend(&atom.data);
            hash_256(&byte_buf)
        }
    }
}

pub fn check_cost(cost: u64, max_cost: u64) -> Result<(), Error> {
    if cost > max_cost {
        Err(Error::new(ErrorKind::InvalidData, "cost exceeded"))
    } else {
        Ok(())
    }
}

pub fn check_arg_count(args: &SExp, expected: usize, name: &str) -> Result<(), Error> {
    if arg_count(args, expected) == expected {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "{} takes exactly {} argument{}",
                name,
                expected,
                if expected == 1 { "" } else { "s" }
            ),
        ))
    }
}

#[must_use]
pub fn arg_count(args: &SExp, return_early_if_exceeds: usize) -> usize {
    let mut count = 0;
    let mut ptr = args;
    while let Ok(pair) = ptr.pair() {
        ptr = &pair.rest;
        count += 1;
        if count > return_early_if_exceeds {
            break;
        };
    }
    count
}

pub fn int_atom<'a>(args: &'a SExp, op_name: &str) -> Result<&'a [u8], Error> {
    args.atom().map(|b| b.data.as_slice()).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{op_name} requires int args"),
        )
    })
}

pub fn atom<'a>(args: &'a SExp, op_name: &str) -> Result<&'a [u8], Error> {
    args.atom()
        .map(|b| b.data.as_slice())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, format!("{op_name} on list")))
}

pub fn two_ints(args: &SExp, op_name: &str) -> Result<(BigInt, usize, BigInt, usize), Error> {
    check_arg_count(args, 2, op_name)?;
    let a0 = args.first()?;
    let a1 = args.rest()?.first()?;
    let n0 = int_atom(a0, op_name)?;
    let n1 = int_atom(a1, op_name)?;
    Ok((number_from_u8(n0), n0.len(), number_from_u8(n1), n1.len()))
}

pub fn sexp_from_bigint(item: &BigInt) -> Result<SExp, Error> {
    let bytes: Vec<u8> = item.to_signed_bytes_be();
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

pub fn u64_from_bigint(item: &BigInt) -> Result<u64, Error> {
    if item.is_negative() {
        return Err(Error::new(ErrorKind::InvalidData, "cannot convert negative integer to u64"));
    }
    if *item > u64::MAX.into() {
        return Err(Error::new(ErrorKind::InvalidData, "u64::MAX exceeded"))
    }
    let bytes: Vec<u8> = item.to_signed_bytes_be();
    let mut slice = bytes.as_slice();
    // make number minimal by removing leading zeros
    while (!slice.is_empty()) && (slice[0] == 0) {
        if slice.len() > 1 && (slice[1] & 0x80 == 0x80) {
            break;
        }
        slice = &slice[1..];
    }
    let mut fixed_ary = [0u8; 8];
    let start = size_of::<u64>() - slice.len();
    for index in start..size_of::<u64>() {
        fixed_ary[index] = slice[index - start];
    }
    Ok(u64::from_be_bytes(fixed_ary))
}

#[must_use]
pub fn number_from_u8(v: &[u8]) -> BigInt {
    if v.is_empty() {
        0.into()
    } else {
        BigInt::from_signed_bytes_be(v)
    }
}

fn u32_from_u8_impl(buf: &[u8], signed: bool) -> Option<u32> {
    if buf.is_empty() {
        return Some(0);
    }
    // too many bytes for u32
    if buf.len() > 4 {
        return None;
    }
    let sign_extend = (buf[0] & 0x80) != 0;
    let mut ret: u32 = if signed && sign_extend {
        0xffff_ffff
    } else {
        0
    };
    for b in buf {
        ret <<= 8;
        ret |= u32::from(*b);
    }
    Some(ret)
}

#[must_use]
pub fn u32_from_u8(buf: &[u8]) -> Option<u32> {
    u32_from_u8_impl(buf, false)
}

#[allow(clippy::cast_possible_wrap)]
#[must_use]
pub fn i32_from_u8(buf: &[u8]) -> Option<i32> {
    u32_from_u8_impl(buf, true).map(|v| v as i32)
}

pub fn i32_atom(args: &SExp, op_name: &str) -> Result<i32, Error> {
    let Ok(buf) = args.atom() else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("{op_name} requires int32 args"),
        ));
    };
    match i32_from_u8(&buf.data) {
        Some(v) => Ok(v),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("{op_name} requires int32 args (with no leading zeros)"),
        )),
    }
}

pub fn new_substr(node: &SExp, start: usize, end: usize) -> Result<SExp, Error> {
    let atom = &node.atom()?.data;
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

pub fn new_concat<'a>(nodes: &'a [&'a SExp]) -> Result<SExp, Error> {
    let mut buf = vec![];
    for node in nodes {
        let atom = node.atom()?;
        buf.extend(&atom.data);
    }
    let new_atom = SExp::Atom(AtomBuf { data: buf });
    Ok(new_atom)
}

pub fn encode_bigint(int: BigInt) -> Result<Vec<u8>, Error> {
    if int == BigInt::zero() {
        Ok(vec![])
    } else {
        let length = (int.bits() + 8) >> 3;
        let bytes = int_to_bytes(int, length as usize, true)?;
        let mut slice = bytes.as_slice();
        while slice.len() > 1 && slice[0] == (if (slice[1] & 0x80) != 0 { 0xFF } else { 0 }) {
            slice = &slice[1..];
        }
        Ok(slice.to_vec())
    }
}

static RE: Lazy<Regex> = Lazy::new(|| Regex::new("[01]{8}").unwrap());

pub fn int_to_bytes(value: BigInt, size: usize, signed: bool) -> Result<Vec<u8>, Error> {
    let is_neg = value < BigInt::zero();
    if is_neg && !signed {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Cannot convert negative int to unsigned.",
        ));
    }
    let pad_len = size * 8;
    let mut binary = format!(
        "{:>0pad_len$}",
        format!("{}", if is_neg { -value } else { value }.to_str_radix(2))
    );
    if is_neg {
        binary = format!(
            "{:>0pad_len$}",
            &(BigInt::from_str_radix(&binary, 2)
                .map_err(|_| { Error::new(ErrorKind::InvalidInput, "Failed to build big int",) })?
                .to_str_radix(2)
                .chars()
                .rev()
                .collect::<String>())
        );
    }
    let bytes = RE
        .captures_iter(&binary)
        .map(|m| -> u8 { u8::from_str_radix(m.get(0).unwrap().as_str(), 2).unwrap() })
        .collect();
    Ok(bytes)
}

#[must_use]
pub fn additions_for_npc(npc_result: NPCResult) -> Vec<Coin> {
    let mut additions: Vec<Coin> = vec![];
    if let Some(conds) = npc_result.conds {
        for spend in conds.spends {
            for coin in spend.create_coin {
                additions.push(Coin {
                    parent_coin_info: spend.coin_id,
                    puzzle_hash: coin.puzzle_hash,
                    amount: coin.amount,
                });
            }
        }
    }
    additions
}
