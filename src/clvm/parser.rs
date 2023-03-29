use crate::clvm::sexp::AtomBuf;
use crate::clvm::sexp::{SExp, NULL};
use actix_web::web::Buf;
use std::io::Read;
use std::io::{Cursor, Write};
use std::io::{Error, ErrorKind};

const MAX_SINGLE_BYTE: u8 = 0x7f;
const CONS_BOX_MARKER: u8 = 0xff;
const MAX_DECODE_SIZE: u64 = 0x400000000;

enum ParserOp {
    Exp,
    Cons,
}

pub fn sexp_from_bytes<T: AsRef<[u8]>>(bytes: T) -> Result<SExp, Error> {
    let mut stream = Cursor::new(bytes);
    let mut byte_buf = [0; 1];
    let mut op_buf = vec![ParserOp::Exp];
    let mut val_buf = vec![];
    while let Some(op) = op_buf.pop() {
        match op {
            ParserOp::Exp => {
                stream.read_exact(&mut byte_buf)?;
                if byte_buf[0] == CONS_BOX_MARKER {
                    op_buf.push(ParserOp::Cons);
                    op_buf.push(ParserOp::Exp);
                    op_buf.push(ParserOp::Exp);
                } else if byte_buf[0] == 0x80 {
                    val_buf.push(NULL.clone());
                } else if byte_buf[0] <= MAX_SINGLE_BYTE {
                    val_buf.push(SExp::Atom(AtomBuf::new(byte_buf.to_vec())));
                } else {
                    let blob_size = decode_size(&mut stream, byte_buf[0])?;
                    if stream.remaining() < blob_size as usize {
                        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
                    }
                    let mut blob: Vec<u8> = vec![0; blob_size as usize];
                    stream.read_exact(&mut blob)?;
                    val_buf.push(SExp::Atom(AtomBuf::new(blob)));
                }
            }
            ParserOp::Cons => {
                if let Some(second) = val_buf.pop() {
                    if let Some(first) = val_buf.pop() {
                        val_buf.push(SExp::Pair((&first, &second).into()));
                    } else {
                        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
                    }
                } else {
                    return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
                }
            }
        }
    }
    val_buf
        .pop()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "Failed to Parse SExp"))
}

pub fn sexp_to_bytes(sexp: &SExp) -> std::io::Result<Vec<u8>> {
    let mut buffer = Cursor::new(Vec::new());
    let mut stack: Vec<&SExp> = vec![sexp];
    while let Some(v) = stack.pop() {
        match v {
            SExp::Atom(atom) => {
                if atom.data.is_empty() {
                    buffer.write_all(&[0x80_u8])?;
                } else if atom.data.len() == 1 && (atom.data[0] <= MAX_SINGLE_BYTE) {
                    buffer.write_all(&[atom.data[0]])?;
                } else {
                    encode_size(&mut buffer, atom.data.len() as u64)?;
                    buffer.write_all(&atom.data)?;
                }
            }
            SExp::Pair(pair) => {
                buffer.write_all(&[CONS_BOX_MARKER])?;
                stack.push(pair.rest.as_ref());
                stack.push(pair.first.as_ref());
            }
        }
    }
    Ok(buffer.into_inner())
}

fn encode_size(f: &mut dyn Write, size: u64) -> Result<(), Error> {
    if size < 0x40 {
        f.write_all(&[(0x80 | size) as u8])?;
    } else if size < 0x2000 {
        f.write_all(&[(0xc0 | (size >> 8)) as u8, ((size) & 0xff) as u8])?;
    } else if size < 0x10_0000 {
        f.write_all(&[
            (0xe0 | (size >> 16)) as u8,
            ((size >> 8) & 0xff) as u8,
            ((size) & 0xff) as u8,
        ])?;
    } else if size < 0x800_0000 {
        f.write_all(&[
            (0xf0 | (size >> 24)) as u8,
            ((size >> 16) & 0xff) as u8,
            ((size >> 8) & 0xff) as u8,
            ((size) & 0xff) as u8,
        ])?;
    } else if size < 0x4_0000_0000 {
        f.write_all(&[
            (0xf8 | (size >> 32)) as u8,
            ((size >> 24) & 0xff) as u8,
            ((size >> 16) & 0xff) as u8,
            ((size >> 8) & 0xff) as u8,
            ((size) & 0xff) as u8,
        ])?;
    } else {
        return Err(Error::new(ErrorKind::InvalidData, "atom too big"));
    }
    Ok(())
}

fn decode_size(stream: &mut dyn Read, initial_b: u8) -> Result<u64, Error> {
    if initial_b & 0x80 == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
    }
    let mut bit_count = 0;
    let mut bit_mask: u8 = 0x80;
    let mut b = initial_b;
    while b & bit_mask != 0 {
        bit_count += 1;
        b &= 0xff ^ bit_mask;
        bit_mask >>= 1;
    }
    let mut size_blob: Vec<u8> = Vec::new();
    size_blob.resize(bit_count, 0);
    size_blob[0] = b;
    if bit_count > 1 {
        stream.read_exact(&mut size_blob[1..])?;
    }
    let mut v = 0;
    if size_blob.len() > 6 {
        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
    }
    for b in &size_blob {
        v <<= 8;
        v += *b as u64;
    }
    if v >= MAX_DECODE_SIZE {
        return Err(Error::new(ErrorKind::InvalidInput, "bad encoding"));
    }
    Ok(v)
}
