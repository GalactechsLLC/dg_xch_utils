use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::SExp;
use crate::clvm::sexp::{AtomBuf, PairBuf};
use crate::constants::NULL_SEXP;
use crate::errors::ClvmError;
use bytes::Buf;
use dg_xch_serialize::{CONS_BOX_MARKER, MAX_SINGLE_BYTE, decode_size, encode_size};
use std::io::Read;
use std::io::{Cursor, Seek, SeekFrom, Write};

#[derive(Debug, Copy, Clone)]
enum ParserOp {
    Exp,
    Cons,
}

const BACK_REFERENCE: u8 = 0xfe;

#[allow(clippy::cast_possible_truncation)]
pub fn sexp_from_bytes(stream: &mut Cursor<&[u8]>) -> Result<SExp<'static>, ClvmError> {
    if !stream.has_remaining() {
        return Ok(NULL_SEXP);
    }
    let mut byte_buf = [0; 1];
    let mut op_buf = vec![ParserOp::Exp];
    let mut val_buf = vec![];
    while let Some(op) = op_buf.pop() {
        match op {
            ParserOp::Exp => {
                if !stream.has_remaining() {
                    return Err(ClvmError::UnexpectedEndOfValues(
                        "Unexpected End of SExp Stream".to_string(),
                    ));
                }
                stream.read_exact(&mut byte_buf)?;
                if byte_buf[0] == CONS_BOX_MARKER {
                    op_buf.push(ParserOp::Cons);
                    op_buf.push(ParserOp::Exp);
                    op_buf.push(ParserOp::Exp);
                } else if byte_buf[0] == 0x80 {
                    val_buf.push(NULL_SEXP);
                } else if byte_buf[0] <= MAX_SINGLE_BYTE {
                    val_buf.push(SExp::Atom(AtomBuf::new(byte_buf.to_vec())));
                } else {
                    let blob_size = decode_size(stream, byte_buf[0])?;
                    if stream.remaining() < blob_size as usize {
                        Err(ClvmError::BadEncoding)?;
                    }
                    let mut blob: Vec<u8> = vec![0; blob_size as usize];
                    stream.read_exact(&mut blob)?;
                    val_buf.push(SExp::Atom(AtomBuf::new(blob)));
                }
            }
            ParserOp::Cons => {
                if let Some(second) = val_buf.pop() {
                    if let Some(first) = val_buf.pop() {
                        val_buf.push(SExp::Pair(PairBuf::Owned((first.into(), second.into()))));
                    } else {
                        Err(ClvmError::BadEncoding)?;
                    }
                } else {
                    Err(ClvmError::BadEncoding)?;
                }
            }
        }
    }
    val_buf
        .pop()
        .ok_or_else(|| ClvmError::InvalidSyntax("Failed to Parse SExp".to_string()))
}

#[allow(clippy::cast_possible_truncation)]
pub fn sexp_from_bytes_backrefs(stream: &mut Cursor<&[u8]>) -> Result<SExp<'static>, ClvmError> {
    if !stream.has_remaining() {
        return Ok(NULL_SEXP);
    }
    let mut byte_buf = [0; 1];
    let mut op_buf = vec![ParserOp::Exp];
    let mut val_buf = Vec::<(SExp<'static>, Option<SExp<'static>>)>::new();
    while let Some(op) = op_buf.pop() {
        match op {
            ParserOp::Exp => {
                if !stream.has_remaining() {
                    return Err(ClvmError::UnexpectedEndOfValues(
                        "Unexpected End of SExp Stream".to_string(),
                    ));
                }
                stream.read_exact(&mut byte_buf)?;
                if byte_buf[0] == CONS_BOX_MARKER {
                    op_buf.push(ParserOp::Cons);
                    op_buf.push(ParserOp::Exp);
                    op_buf.push(ParserOp::Exp);
                } else if byte_buf[0] == BACK_REFERENCE {
                    let path = parse_backref_path(stream)?;
                    let backref = traverse_backref_path(&mut val_buf, &path)?;
                    val_buf.push((backref, None));
                } else {
                    val_buf.push((parse_atom_from_first_byte(stream, byte_buf[0])?, None));
                }
            }
            ParserOp::Cons => {
                let Some((second, _)) = val_buf.pop() else {
                    return Err(ClvmError::BadEncoding);
                };
                let Some((first, _)) = val_buf.pop() else {
                    return Err(ClvmError::BadEncoding);
                };
                val_buf.push((
                    SExp::Pair(PairBuf::Owned((first.into(), second.into()))),
                    None,
                ));
            }
        }
    }
    val_buf
        .pop()
        .map(|(sexp, _)| sexp)
        .ok_or_else(|| ClvmError::InvalidSyntax("Failed to Parse SExp".to_string()))
}

fn parse_atom_from_first_byte(
    stream: &mut Cursor<&[u8]>,
    first_byte: u8,
) -> Result<SExp<'static>, ClvmError> {
    if first_byte == 0x80 {
        Ok(NULL_SEXP)
    } else if first_byte <= MAX_SINGLE_BYTE {
        Ok(SExp::Atom(AtomBuf::new(vec![first_byte])))
    } else {
        let blob_size = decode_size(stream, first_byte)?;
        if stream.remaining() < blob_size as usize {
            return Err(ClvmError::BadEncoding);
        }
        let mut blob: Vec<u8> = vec![0; blob_size as usize];
        stream.read_exact(&mut blob)?;
        Ok(SExp::Atom(AtomBuf::new(blob)))
    }
}

fn parse_backref_path(stream: &mut Cursor<&[u8]>) -> Result<Vec<u8>, ClvmError> {
    if !stream.has_remaining() {
        return Err(ClvmError::UnexpectedEndOfValues(
            "Unexpected End of backreference path".to_string(),
        ));
    }
    let mut byte_buf = [0; 1];
    stream.read_exact(&mut byte_buf)?;
    if byte_buf[0] == CONS_BOX_MARKER || byte_buf[0] == BACK_REFERENCE {
        return Err(ClvmError::SerializationError(
            "Backreference path must be an atom".to_string(),
        ));
    }
    Ok(match parse_atom_from_first_byte(stream, byte_buf[0])? {
        SExp::Atom(atom) => atom.as_ref().to_vec(),
        SExp::Pair(_) => unreachable!("parse_atom_from_first_byte only returns atoms"),
    })
}

fn traverse_backref_path(
    values: &mut [(SExp<'static>, Option<SExp<'static>>)],
    path: &[u8],
) -> Result<SExp<'static>, ClvmError> {
    let mut parsing_sexp = values.is_empty();
    let mut stack_index = values.len().saturating_sub(1);
    let first_non_zero = path
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(path.len());
    if first_non_zero >= path.len() {
        return Ok(NULL_SEXP);
    }
    let sentinel = 0x80_u8 >> path[first_non_zero].leading_zeros();
    let mut byte_index = path.len() - 1;
    let mut bitmask = 0x01_u8;
    let mut current = NULL_SEXP;

    while byte_index > first_non_zero || bitmask < sentinel {
        let take_rest = path[byte_index] & bitmask != 0;
        if parsing_sexp {
            match current {
                SExp::Pair(ref pair) => {
                    current = if take_rest {
                        pair.rest().to_owned()
                    } else {
                        pair.first().to_owned()
                    };
                }
                SExp::Atom(_) => {
                    return Err(ClvmError::SerializationError(
                        "Backreference path entered an atom".to_string(),
                    ));
                }
            }
        } else if take_rest {
            if stack_index == 0 {
                parsing_sexp = true;
                current = NULL_SEXP;
            } else {
                stack_index -= 1;
            }
        } else {
            parsing_sexp = true;
            current = values[stack_index].0.clone();
        }

        if bitmask == 0x80 {
            bitmask = 0x01;
            byte_index = byte_index.saturating_sub(1);
        } else {
            bitmask <<= 1;
        }
    }

    if parsing_sexp {
        return Ok(current);
    }

    let mut stack_list = NULL_SEXP;
    for value in values.iter_mut().take(stack_index + 1) {
        if let Some(cached) = &value.1 {
            stack_list = cached.clone();
        } else {
            stack_list = SExp::Pair(PairBuf::Owned((
                value.0.clone().into(),
                stack_list.clone().into(),
            )));
            value.1 = Some(stack_list.clone());
        }
    }
    Ok(stack_list)
}

pub fn sexp_to_bytes(sexp: &SExp) -> std::io::Result<SerializedProgram> {
    let mut buffer = Cursor::new(Vec::new());
    let mut stack: Vec<&SExp> = vec![sexp];
    while let Some(v) = stack.pop() {
        match v {
            SExp::Atom(atom) => {
                let data = atom.as_ref();
                if data.is_empty() {
                    buffer.write_all(&[0x80_u8])?;
                } else if data.len() == 1 && (data[0] <= MAX_SINGLE_BYTE) {
                    buffer.write_all(&[data[0]])?;
                } else {
                    encode_size(&mut buffer, data.len() as u64)?;
                    buffer.write_all(data)?;
                }
            }
            SExp::Pair(pair) => {
                buffer.write_all(&[CONS_BOX_MARKER])?;
                stack.push(pair.rest());
                stack.push(pair.first());
            }
        }
    }
    Ok(buffer.into_inner().into())
}

#[cfg(test)]
#[path = "../../tests/unit/clvm/parser/tests.rs"]
mod tests;

/// Canonical CLVM serialization check (the SF9 INVALID_TRANSACTIONS_GENERATOR_ENCODING
/// rule): every atom (back-reference path atoms included) must use the minimal
/// length-prefix encoding, and the stream must contain exactly one program with no
/// trailing bytes. Only length-prefix minimality is enforced — a length-1 atom whose
/// content would fit the single-byte form still passes.
#[must_use]
pub fn is_canonical_serialization(bytes: &[u8]) -> bool {
    let mut stream = Cursor::new(bytes);
    let mut counter: u64 = 1;
    let mut b = [0u8; 1];
    while counter > 0 {
        counter -= 1;
        if stream.read_exact(&mut b).is_err() {
            return false;
        }
        if b[0] == CONS_BOX_MARKER {
            counter += 2;
        } else if b[0] == BACK_REFERENCE {
            if stream.read_exact(&mut b).is_err() {
                return false;
            }
            if !is_canonical_atom(&mut stream, b[0]) {
                return false;
            }
        } else if !is_canonical_atom(&mut stream, b[0]) {
            return false;
        }
        if (bytes.len() as u64) < stream.position() {
            return false;
        }
    }
    bytes.len() as u64 == stream.position()
}

// One atom's length prefix is minimal iff the length reaches the prefix width's floor
// (a 2-byte prefix starts at 1<<6, 3-byte at 1<<13, ...).
fn is_canonical_atom(stream: &mut Cursor<&[u8]>, first_byte: u8) -> bool {
    if first_byte == 0x80 || first_byte <= MAX_SINGLE_BYTE {
        return true;
    }
    let prefix_len = first_byte.leading_ones();
    let Ok(atom_len) = decode_size(stream, first_byte) else {
        return false;
    };
    let min_value: u64 = match prefix_len {
        1 => 1,
        2 => 1 << 6,
        3 => 1 << 13,
        4 => 1 << 20,
        5 => 1 << 28,
        6 => 1 << 36,
        _ => return false,
    };
    let Ok(len_i64) = i64::try_from(atom_len) else {
        return false;
    };
    if stream.seek(SeekFrom::Current(len_i64)).is_err() {
        return false;
    }
    atom_len >= min_value
}

#[cfg(test)]
#[path = "../../tests/unit/clvm/parser/canonical_tests.rs"]
mod canonical_tests;
