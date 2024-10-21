pub mod keywords;
pub mod reader;

use crate::clvm::assemble::keywords::KEYWORD_TO_ATOM;
use crate::clvm::assemble::reader::{Reader, Token, DOT_CONS, END_CONS, START_CONS};
use crate::clvm::casts::bigint_to_bytes;
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp;
use crate::clvm::sexp::{AtomBuf, SExp};
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
        Ok(sexp::NULL.clone())
    }
}

pub fn tokenize_cons(tokens: &mut Reader) -> Result<SExp, Error> {
    if let Some(token) = tokens.next() {
        handle_cons(&token, tokens)
    } else {
        Ok(sexp::NULL.clone())
    }
}
pub fn handle_cons(token: &Token, tokens: &mut Reader) -> Result<SExp, Error> {
    if token == &END_CONS {
        Ok(sexp::NULL.clone())
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
        Ok(sexp::NULL.clone())
    } else {
        match handle_int(token) {
            Some(v) => bigint_to_bytes(&v, true).map(|v| SExp::Atom(AtomBuf::new(v))),
            None => handle_hex(token)?
                .or_else(|| handle_quote(token).or_else(|| Some(handle_bytes(token))))
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
pub fn handle_bytes(token: &Token) -> SExp {
    let mut bytes = token.bytes;
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
pub fn handle_quote(token: &Token) -> Option<SExp> {
    if is_quote(token.bytes) {
        Some(SExp::Atom(AtomBuf::new(
            token.bytes[1..(token.bytes.len() - 1)].to_vec(),
        )))
    } else {
        None
    }
}

pub fn handle_hex(token: &Token) -> Result<Option<SExp>, Error> {
    if is_hex(token.bytes) {
        let mut bytes = if token.bytes.len() % 2 > 0 {
            vec![b'0']
        } else {
            vec![]
        };
        bytes.extend(token.bytes[2..].to_vec());
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

pub fn handle_int(token: &Token) -> Option<BigInt> {
    static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[+\-]?[0-9]+(?:_[0-9]+)*$").unwrap());
    let as_str = String::from_utf8_lossy(token.bytes);
    if RE.is_match(&as_str) {
        as_str.replace('_', "").parse::<BigInt>().ok()
    } else {
        None
    }
}
