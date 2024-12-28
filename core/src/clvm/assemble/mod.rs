pub mod reader;

use crate::clvm::assemble::reader::{Reader, Token};
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::{AtomBuf, SExp};
use crate::constants::{DOT_CONS, END_CONS, KEYWORD_TO_ATOM, NULL_SEXP, START_CONS};
use crate::formatting::bigint_to_bytes;
use hex::decode;
use num_bigint::BigInt;
use once_cell::sync::Lazy;
use regex::Regex;
use std::io::{Error, ErrorKind};

pub fn assemble_text(s: &str) -> Result<SerializedProgram, Error> {
    let mut reader = Reader::new(s.as_bytes());
    let sexp = tokenize_exp(&mut reader)?;
    Ok(SerializedProgram::from_bytes(&sexp_to_bytes(&sexp)?))
}

pub fn tokenize_exp(tokens: &mut Reader) -> Result<SExp, Error> {
    if let Some(token) = tokens.next() {
        handle_token(&token, tokens)
    } else {
        Ok(NULL_SEXP.clone())
    }
}

pub fn tokenize_cons(tokens: &mut Reader) -> Result<SExp, Error> {
    if let Some(token) = tokens.next() {
        handle_cons(&token, tokens)
    } else {
        Ok(NULL_SEXP.clone())
    }
}
pub fn handle_cons(token: &Token, tokens: &mut Reader) -> Result<SExp, Error> {
    if token == &END_CONS {
        Ok(NULL_SEXP.clone())
    } else {
        let first = handle_token(token, tokens)?;
        if let Some(token) = tokens.next() {
            if token == DOT_CONS {
                let rest = tokenize_exp(tokens)?;
                if let Some(token) = tokens.next() {
                    if token == END_CONS {
                        Ok(first.cons(rest))
                    } else {
                        Err(Error::new(
                            ErrorKind::InvalidData,
                            format!("Illegal dot expression at position: {}", token.index),
                        ))
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
                Ok(first.cons(handle_cons(&token, tokens)?))
            }
        } else {
            Err(Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Unexpected end of source while parsing cons at: {}",
                    &token.index
                ),
            ))
        }
    }
}

pub fn handle_token(token: &Token, tokens: &mut Reader) -> Result<SExp, Error> {
    if token == &START_CONS {
        tokenize_cons(tokens)
    } else if token.bytes.is_empty() {
        Ok(NULL_SEXP.clone())
    } else {
        let bytes = token.bytes;
        match handle_int(bytes) {
            Some(v) => bigint_to_bytes(&v, true).map(|v| SExp::Atom(AtomBuf::new(v))),
            None => handle_hex(bytes)?
                .or_else(|| handle_quote(bytes).or_else(|| Some(handle_bytes(bytes))))
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Other,
                        format!("Failed to parse Token: {token:?}"),
                    )
                }),
        }
    }
}

#[must_use]
pub fn is_hex(chars: &[u8]) -> bool {
    chars.len() > 2 && chars[0] == b'0' && (chars[1] == b'x' || chars[1] == b'X')
}

#[must_use]
pub fn is_quote(chars: &[u8]) -> bool {
    chars.len() > 2
        && ((chars.first() == Some(&b'"') && chars.last() == Some(&b'"'))
            || (chars.first() == Some(&b'\'') && chars.last() == Some(&b'\'')))
}

#[must_use]
pub fn handle_bytes(bytes: &[u8]) -> SExp {
    let mut bytes = bytes;
    if bytes[0] == b'#' {
        bytes = &bytes[1..];
    }
    if let Ok(Some(kw)) = String::from_utf8(bytes.to_vec()).map(|s| KEYWORD_TO_ATOM.get(&s)) {
        SExp::Atom(AtomBuf::new(kw.clone()))
    } else {
        SExp::Atom(AtomBuf::new(bytes.to_vec()))
    }
}

#[must_use]
pub fn handle_quote(token: &[u8]) -> Option<SExp> {
    if is_quote(token) {
        Some(SExp::Atom(AtomBuf::new(
            token[1..(token.len() - 1)].to_vec(),
        )))
    } else {
        None
    }
}

pub fn handle_hex(bytes: &[u8]) -> Result<Option<SExp>, Error> {
    if is_hex(bytes) {
        let mut bytes = if bytes.len() % 2 > 0 {
            vec![b'0']
        } else {
            vec![]
        };
        bytes.extend(bytes[2..].to_vec());
        let as_hex = String::from_utf8(bytes).map_err(|e| {
            Error::new(ErrorKind::InvalidInput, format!("Invalid Hex Value: {e:?}"))
        })?;
        Ok(Some(SExp::Atom(AtomBuf::new(decode(as_hex).map_err(
            |e| Error::new(ErrorKind::InvalidInput, format!("Invalid Hex Value: {e:?}")),
        )?))))
    } else {
        Ok(None)
    }
}

pub fn handle_int(bytes: &[u8]) -> Option<BigInt> {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[+\-]?[0-9]+(?:_[0-9]+)*$").unwrap());
    let as_str = String::from_utf8_lossy(bytes);
    if RE.is_match(&as_str) {
        as_str.replace('_', "").parse::<BigInt>().ok()
    } else {
        None
    }
}
