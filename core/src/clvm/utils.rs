use crate::clvm::sexp::SExp;
use crate::formatting::{i32_from_slice, number_from_slice};
use num_bigint::BigInt;
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

pub fn check_cost(cost: u64, max_cost: u64) -> Result<(), Error> {
    if cost > max_cost {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!("cost {cost} exceeded {max_cost}"),
        ))
    } else {
        Ok(())
    }
}

pub fn check_arg_count(args: &SExp, expected: usize, name: &str) -> Result<(), Error> {
    if args.arg_count(expected) == expected {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "{name} takes exactly {expected} argument{}",
                if expected == 1 { "" } else { "s" }
            ),
        ))
    }
}

pub fn int_atom<'a>(args: &'a SExp, op_name: &str) -> Result<&'a [u8], Error> {
    args.atom().map(|b| b.data.as_slice()).map_err(|_| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("{op_name} requires int args: Got {args}"),
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
    Ok((
        number_from_slice(n0),
        n0.len(),
        number_from_slice(n1),
        n1.len(),
    ))
}

pub fn i32_atom(args: &SExp, op_name: &str) -> Result<i32, Error> {
    let Ok(buf) = args.atom() else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("{op_name} requires int32 args"),
        ));
    };
    match i32_from_slice(&buf.data) {
        Some(v) => Ok(v),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("{op_name} requires int32 args (with no leading zeros)"),
        )),
    }
}
