use crate::clvm::assemble::{handle_bytes, handle_hex, handle_int, handle_quote};
use crate::clvm::sexp::{AtomBuf, SExp};
use crate::constants::NULL_SEXP;
use crate::errors::ClvmError;
use crate::formatting::bigint_to_bytes;
use num_bigint::BigInt;
use std::io::{Error, ErrorKind};

pub fn parse_value(value: &[u8]) -> Result<SExp<'static>, ClvmError> {
    if value.is_empty() || value == b"\"\"" || value == b"''" {
        Ok(NULL_SEXP)
    } else {
        match handle_int(value) {
            Some(v) => Ok(SExp::Atom(AtomBuf::new(bigint_to_bytes(&v, true)))),
            None => handle_hex(value)?
                .or_else(|| handle_quote(value).or_else(|| Some(handle_bytes(value))))
                .ok_or_else(|| {
                    ClvmError::InvalidInput(format!("Failed to parse Value: {value:?}"))
                }),
        }
    }
}

pub(super) fn eval_constant(entries: &[SExp<'_>]) -> Option<SExp<'static>> {
    if !entries.iter().skip(1).all(|value| {
        *value == NULL_SEXP
            || matches!(value, SExp::Pair(pair) if pair.first() == &crate::constants::QUOTE_SEXP)
    }) {
        return None;
    }
    crate::clvm::runtime::ClvmRuntime::new(10_000_000, 0)
        .run(&SExp::from(entries.to_vec()), &NULL_SEXP)
        .ok()
        .map(|(_, value)| value)
}

pub fn get_function_pointer(
    function_index: usize,
    const_count: usize,
    func_count: usize,
    balanced: bool,
) -> Result<BigInt, Error> {
    if function_index >= func_count {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid Function Index",
        ));
    }
    let mut pointer = BigInt::from(2u8);
    if balanced {
        for _ in 0..const_count {
            pointer += 1;
            pointer <<= 1;
        }
        let mut start = 0;
        let mut end = func_count;
        let mut bit = BigInt::from(1u8) << (pointer.bits() - 1);
        while end - start > 1 {
            let middle = (start + end) / 2;
            if function_index < middle {
                pointer += &bit;
                end = middle;
            } else {
                pointer += &bit << 1;
                start = middle;
            }
            bit <<= 1;
        }
        return Ok(pointer);
    }
    for _ in 0..const_count + func_count - 1 - function_index {
        pointer += 1;
        pointer <<= 1;
    }
    if function_index != 0 {
        pointer += BigInt::from(1u8) << (pointer.bits() - 1);
    }
    Ok(pointer)
}

pub fn get_const_pointer(const_index: usize) -> Result<BigInt, Error> {
    let mut pointer = BigInt::from(1u8);
    for _ in 0..const_index {
        pointer += 1;
        pointer <<= 1;
    }
    pointer += 1;
    pointer <<= 1;
    Ok(pointer)
}

pub fn get_arg_pointer(arg_index: usize) -> Result<BigInt, Error> {
    let mut pointer = BigInt::from(1u8);
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

pub fn select_path(mut value: SExp, mut path: BigInt) -> Result<SExp, Error> {
    while path > BigInt::from(1u8) {
        let right = (&path & BigInt::from(1u8)) == BigInt::from(1u8);
        value = match &value {
            SExp::Atom(atom) if atom.as_int().sign() == num_bigint::Sign::Plus => {
                let old_path = atom.as_int();
                let bit = BigInt::from(1u8) << (old_path.bits() - 1);
                SExp::from(&(old_path + if right { bit << 1 } else { bit }))
            }
            SExp::Pair(pair)
                if pair.first() == &crate::constants::QUOTE_SEXP
                    && matches!(pair.rest(), SExp::Pair(_)) =>
            {
                let data = pair.rest().pair().map_err(Error::other)?;
                crate::constants::QUOTE_SEXP.clone().cons(if right {
                    data.rest().to_owned()
                } else {
                    data.first().to_owned()
                })
            }
            _ => concat_args(vec![
                SExp::from(if right { 6u8 } else { 5u8 }),
                value,
                NULL_SEXP,
            ])?,
        };
        path >>= 1;
    }
    Ok(value)
}

pub fn get_program_size(
    value: &SExp,
    byte_atoms: &std::collections::HashMap<usize, std::sync::Arc<Vec<u8>>>,
) -> u64 {
    match value {
        SExp::Atom(AtomBuf::Owned(atom))
            if byte_atoms.contains_key(&(std::sync::Arc::as_ptr(atom) as usize)) =>
        {
            1 + atom.len() as u64
        }
        SExp::Atom(atom) => 1 + atom.as_int().bits().saturating_sub(1) / 8,
        SExp::Pair(pair) => {
            1 + get_program_size(pair.first(), byte_atoms)
                + get_program_size(pair.rest(), byte_atoms)
        }
    }
}
