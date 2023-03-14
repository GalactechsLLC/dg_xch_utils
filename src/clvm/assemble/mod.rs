pub mod keywords;
pub mod reader;

use crate::clvm::assemble::keywords::KEYWORD_TO_ATOM;
use crate::clvm::assemble::reader::{Reader, ReaderToken};
use crate::clvm::parser::sexp_to_bytes;
use crate::clvm::program::SerializedProgram;
use crate::clvm::sexp::{SExp, NULL};
use std::io::{Cursor, Error};

pub fn assemble_text(s: &str) -> Result<SerializedProgram, Error> {
    let stream = Cursor::new(s.as_bytes().to_vec());
    let mut reader = Reader::new(stream);
    let token = reader.read_object()?;
    let sexp = assemble_from_reader(&token)?;

    Ok(SerializedProgram::from_bytes(&sexp_to_bytes(&sexp)?))
}

pub fn assemble_from_reader(token: &ReaderToken) -> Result<SExp, Error> {
    Ok(match token {
        ReaderToken::Null => NULL.clone(),
        ReaderToken::Quotes(b) => SExp::Atom(b.into()),
        ReaderToken::Int(b, _signed) => SExp::Atom(b.into()),
        ReaderToken::Hex(b) => SExp::Atom(b.into()),
        ReaderToken::Symbol(s) => {
            let mut s_real_name = s.clone();
            if let Some(stripped) = s.strip_prefix('#') {
                s_real_name = stripped.to_string();
            }
            match KEYWORD_TO_ATOM.get(&s_real_name) {
                Some(b) => SExp::Atom(b.into()),
                None => SExp::Atom(s_real_name.as_bytes().into()),
            }
        }
        ReaderToken::Cons(l, r) => {
            SExp::Pair((assemble_from_reader(l)?, assemble_from_reader(r)?).into())
        }
    })
}
