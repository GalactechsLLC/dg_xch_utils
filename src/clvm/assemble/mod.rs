pub mod keywords;
pub mod reader;

use crate::clvm::assemble::keywords::KEYWORD_TO_ATOM;
use crate::clvm::assemble::reader::{Reader, Token};
use crate::clvm::casts::bigint_to_bytes;
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp;
use crate::clvm::sexp::{AtomBuf, SExp};
use hex::decode;
use num_bigint::BigInt;
use std::io::{Error, ErrorKind};

pub fn assemble_text(s: &str) -> Result<SerializedProgram, Error> {
    let stream = s.as_bytes().to_vec();
    let mut reader = Reader::new(&stream);
    let sexp = tokenize_exp(&mut reader)?;
    Ok(SerializedProgram::from_bytes(&sexp_to_bytes(&sexp)?))
}

pub fn is_hex(chars: &[u8]) -> bool {
    chars.len() > 2 && chars[0] == b'0' && (chars[1] == b'x' || chars[1] == b'X')
}

pub fn is_quote(chars: &[u8]) -> bool {
    chars.len() > 2 && chars[0] == b'0' && (chars[1] == b'x' || chars[1] == b'X')
}

pub fn tokenize_cons(tokens: &mut Reader) -> Result<SExp, Error> {
    if let Some(token) = tokens.next() {
        handle_cons(token, tokens)
    } else {
        Ok(sexp::NULL.clone())
    }
}

pub fn handle_cons(token: Token, tokens: &mut Reader) -> Result<SExp, Error> {
    if token.bytes == vec![b')'] {
        Ok(sexp::NULL.clone())
    } else {
        let token_idx = token.index;
        let first = handle_token(token, tokens)?;
        if let Some(token) = tokens.next() {
            let rest;
            if token.bytes == vec![b'.'] {
                rest = tokenize_exp(tokens)?;
                if let Some(token) = tokens.next() {
                    if token.bytes != vec![b')'] {
                        Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("Illegal dot expression at position: {}", token.index),
                        ))
                    } else {
                        Ok(first.cons(rest)?)
                    }
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidData,
                        format!(
                            "Unexpected end of source while parsing dot cons at: {}",
                            token.index
                        ),
                    ))
                }
            } else {
                rest = handle_cons(token, tokens)?;
                Ok(first.cons(rest)?)
            }
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Unexpected end of source while parsing cons at: {}",
                    token_idx
                ),
            ))
        }
    }
}

pub fn handle_token(token: Token, tokens: &mut Reader) -> Result<SExp, Error> {
    if token.bytes == vec![b'('] {
        tokenize_cons(tokens)
    } else if token.bytes.is_empty() {
        Ok(sexp::NULL.clone())
    } else {
        match String::from_utf8(token.bytes.to_vec())
            .ok()
            .and_then(|s| s.parse::<BigInt>().ok())
            .and_then(|n| bigint_to_bytes(&n, true).ok())
        {
            Some(n) => Ok(SExp::Atom(AtomBuf::new(n))),
            None => {
                if is_hex(token.bytes) {
                    let mut bytes = if token.bytes.len() % 2 > 0 {
                        vec![b'0']
                    } else {
                        vec![]
                    };
                    bytes.extend(token.bytes[2..].to_vec());
                    let as_hex = String::from_utf8(bytes).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            format!("Invalid Hex Value: {:?}", e),
                        )
                    })?;
                    Ok(SExp::Atom(AtomBuf::new(decode(as_hex).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            format!("Invalid Hex Value: {:?}", e),
                        )
                    })?)))
                } else if is_quote(token.bytes) {
                    Ok(SExp::Atom(AtomBuf::new(
                        token.bytes[1..(token.bytes.len() - 1)].to_vec(),
                    )))
                } else {
                    let mut bytes = token.bytes;
                    if bytes[0] == b'#' {
                        bytes = &bytes[1..];
                    }
                    if let Ok(Some(kw)) =
                        String::from_utf8(bytes.to_vec()).map(|s| KEYWORD_TO_ATOM.get(&s))
                    {
                        Ok(SExp::Atom(AtomBuf::new(kw.clone())))
                    } else {
                        Ok(SExp::Atom(AtomBuf::new(bytes.to_vec())))
                    }
                }
            }
        }
    }
}

pub fn tokenize_exp(tokens: &mut Reader) -> Result<SExp, Error> {
    if let Some(token) = tokens.next() {
        handle_token(token, tokens)
    } else {
        Ok(sexp::NULL.clone())
    }
}
