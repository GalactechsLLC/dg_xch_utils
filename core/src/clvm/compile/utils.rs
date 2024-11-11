use std::io::{Error, ErrorKind};
use once_cell::sync::Lazy;
use crate::clvm::assemble::{handle_bytes, handle_hex, handle_int, handle_quote};
use crate::clvm::assemble::keywords::{APPLY, CONS, QUOTE};
use crate::clvm::casts::bigint_to_bytes;
use crate::clvm::sexp::{AtomBuf, SExp, NULL};

pub static CONS_SEXP: Lazy<SExp> = Lazy::new(|| {
    SExp::Atom(AtomBuf::new(vec![CONS]))
});
pub static APPLY_SEXP: Lazy<SExp> = Lazy::new(|| {
    SExp::Atom(AtomBuf::new(vec![APPLY]))
});
pub static QUOTE_SEXP: Lazy<SExp> = Lazy::new(|| {
    SExp::Atom(AtomBuf::new(vec![QUOTE]))
});
pub static ARGS_SEXP: Lazy<SExp> = Lazy::new(|| {
    SExp::Atom(AtomBuf::new(vec![QUOTE]))
});

pub fn parse_value(value: &[u8]) -> Result<SExp, Error> {
    if value.is_empty() {
        Ok(NULL.clone())
    } else {
        match handle_int(value) {
            Some(v) => bigint_to_bytes(&v, true).map(|v| SExp::Atom(AtomBuf::new(v))),
            None => handle_hex(value)?
                .or_else(|| handle_quote(value).or_else(|| Some(handle_bytes(value))))
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Other,
                        format!("Failed to parse Value: {value:?}"),
                    )
                }),
        }
    }
}

pub fn get_function_pointer(function_index: u8, const_count: usize, func_count: usize) -> Result<u32, Error> {
    let mut pointer = 1u32;
    pointer <<= 1;
    for _ in 0..const_count {
        pointer += 1;
        pointer <<= 1;
    }
    if func_count == 1 {
        Ok(pointer)
    } else {
        for _ in 0..function_index {
            pointer += 1;
            pointer <<= 1;
        }
        if func_count > 1 && func_count - 1 == function_index as usize {
            pointer >>= 1;
            pointer -= 2;
            pointer <<= 1;
        } else if func_count == 1 {
            pointer <<= 1;
        } else {
            pointer += 1;
            pointer <<= 1;
        }
        Ok(pointer)
    }
}

pub fn get_const_pointer(const_index: u8) -> Result<u32, Error> {
    let mut pointer = 1u32;
    for _ in 0..const_index {
        pointer += 1;
        pointer <<= 1;
    }
    pointer += 1;
    pointer <<= 1;
    Ok(pointer)
}

pub fn get_arg_pointer(arg_index: u8) -> Result<u32, Error> {
    let mut pointer = 1u32;
    for _ in 0..arg_index {
        pointer += 1;
        pointer <<= 1;
    }
    pointer += 1;
    Ok(pointer)
}

pub fn concat_args(mut entries: Vec<SExp>) -> Result<SExp, Error> {
    let mut sexp = None;
    while let Some(next) = entries.pop() {
        match sexp {
            None => {
                sexp = Some(next);
            }
            Some(existing) => {
                let new = next.cons(existing);
                sexp = Some(new);
            }
        }
    }
    sexp.ok_or(Error::new(ErrorKind::InvalidData, "No Args Provided"))
}