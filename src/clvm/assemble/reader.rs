use crate::clvm::casts::bigint_to_bytes;
use num_bigint::BigInt;
use std::io::Read;
use std::io::{Cursor, Error, ErrorKind};
use std::mem::swap;

const EOL_CHARS: [u8; 2] = [b'\r', b'\n'];
const SPACE_CHARS: [u8; 4] = [b' ', b'\t', b'\r', b'\n'];
const COMMENT_MARKER: u8 = b';';
pub enum ReaderToken {
    Cons(Box<ReaderToken>, Box<ReaderToken>),
    Null,
    Quotes(Vec<u8>),
    Int(Vec<u8>, bool),
    Hex(Vec<u8>),
    Symbol(String),
}
pub struct Reader {
    stream: Cursor<Vec<u8>>,
}
impl Reader {
    pub fn new(stream: Cursor<Vec<u8>>) -> Self {
        Self { stream }
    }
    fn read(&mut self, n: usize) -> Result<Vec<u8>, Error> {
        let mut buf = vec![];
        buf.reserve(n);
        self.stream.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn backup(&mut self, n: u64) {
        let cur_seek = self.stream.position();
        if n > cur_seek {
            self.stream.set_position(0);
        } else {
            self.stream.set_position(cur_seek - n);
        }
    }
    pub fn read_object(&mut self) -> Result<ReaderToken, Error> {
        consume_object(self)
    }
}

pub fn consume_whitespace(s: &mut Reader) -> Result<(), Error> {
    let mut in_comment = false;
    loop {
        let b = s.read(1)?;
        if b.is_empty() {
            return Ok(());
        }
        let ch = b[0];
        if in_comment {
            if EOL_CHARS.contains(&ch) {
                in_comment = false;
            } else {
                continue;
            }
        }
        if ch == COMMENT_MARKER {
            in_comment = true;
        } else if !SPACE_CHARS.contains(&ch) {
            break;
        }
    }
    s.backup(1);
    Ok(())
}

pub fn consume_quoted(s: &mut Reader, q: char) -> Result<ReaderToken, Error> {
    let starting_at = s.stream.position() - 1;
    let mut bs = false;
    let mut qchars = vec![];
    loop {
        let b = s.read(1)?;
        if b.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "unterminated string starting at {}, {:?}",
                    starting_at, qchars
                ),
            ));
        }
        if bs {
            bs = false;
            qchars.push(b[0]);
        } else if b[0] == b'\\' {
            bs = true;
        } else if b[0] == q as u8 {
            break;
        } else {
            qchars.push(b[0]);
        }
    }

    Ok(ReaderToken::Quotes(qchars))
}

pub fn is_hex(chars: &[u8]) -> bool {
    chars.len() > 2 && chars[0] == b'0' && (chars[1] == b'x' || chars[1] == b'X')
}

pub fn interpret_atom_value(chars: &[u8]) -> ReaderToken {
    if chars.is_empty() {
        ReaderToken::Null
    } else if is_hex(chars) {
        let mut string_bytes = if chars.len() % 2 > 0 {
            vec![b'0']
        } else {
            vec![]
        };
        string_bytes.extend(chars[2..].to_vec());
        ReaderToken::Hex(string_bytes)
    } else {
        match String::from_utf8(chars.to_vec())
            .ok()
            .and_then(|s| s.parse::<BigInt>().ok())
            .and_then(|n| bigint_to_bytes(&n, true).ok())
        {
            Some(n) => ReaderToken::Int(n, true),
            None => {
                let string_bytes = chars.to_vec();
                ReaderToken::Symbol(String::from_utf8_lossy(&string_bytes).as_ref().to_string())
            }
        }
    }
}

pub fn consume_atom(s: &mut Reader, b: &[u8]) -> Result<Option<ReaderToken>, Error> {
    let mut result_vec = b.to_vec();
    loop {
        let b = s.read(1)?;
        if b.is_empty() {
            if result_vec.is_empty() {
                return Ok(None);
            } else {
                return Ok(Some(interpret_atom_value(&result_vec)));
            }
        }
        if b[0] == b'(' || b[0] == b')' || SPACE_CHARS.contains(&b[0]) {
            s.backup(1);
            return Ok(Some(interpret_atom_value(&result_vec)));
        }
        result_vec.push(b[0]);
    }
}

fn enlist_ir(vec: &mut Vec<ReaderToken>, tail: ReaderToken) -> ReaderToken {
    let mut result = tail;
    for i_reverse in 0..vec.len() {
        let i = vec.len() - i_reverse - 1;
        let mut next_head = ReaderToken::Null;
        swap(&mut vec[i], &mut next_head);
        result = ReaderToken::Cons(Box::new(next_head), Box::new(result));
    }
    result
}

pub fn consume_cons_body(s: &mut Reader) -> Result<ReaderToken, Error> {
    let mut result = vec![];
    loop {
        consume_whitespace(s)?;
        let b = s.read(1)?;
        if b.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "missing )".to_string()));
        }
        if b[0] == b')' {
            return Ok(enlist_ir(&mut result, ReaderToken::Null));
        }
        if b[0] == b'(' {
            match consume_cons_body(s) {
                Err(e) => {
                    return Err(e);
                }
                Ok(v) => {
                    result.push(v);
                    continue;
                }
            }
        }
        if b[0] == b'.' {
            consume_whitespace(s)?;
            let tail_obj = consume_object(s)?;
            consume_whitespace(s)?;
            let b = s.read(1)?;
            if b.is_empty() || b[0] != b')' {
                return Err(Error::new(ErrorKind::InvalidInput, "missing )".to_string()));
            }
            return Ok(enlist_ir(&mut result, tail_obj));
        }

        if b[0] == b'\"' || b[0] == b'\'' {
            result.push(consume_quoted(s, b[0] as char)?);
        } else if let Some(f) = consume_atom(s, &b)? {
            result.push(f);
        } else {
            return Err(Error::new(ErrorKind::InvalidInput, "missing )".to_string()));
        }
    }
}

pub fn consume_object(s: &mut Reader) -> Result<ReaderToken, Error> {
    consume_whitespace(s)?;
    let b = s.read(1)?;
    if b.is_empty() {
        Ok(ReaderToken::Null)
    } else if b[0] == b'(' {
        consume_cons_body(s)
    } else if b[0] == b'\"' || b[0] == b'\'' {
        consume_quoted(s, b[0] as char)
    } else {
        match consume_atom(s, &b)? {
            None => Err(Error::new(
                ErrorKind::InvalidInput,
                "Empty Stream".to_string(),
            )),
            Some(ir) => Ok(ir),
        }
    }
}
