use std::fmt::{Debug, Formatter};

const EOL_CHARS: [u8; 2] = [b'\r', b'\n'];
const START_CONS_CHARS: [u8; 2] = [b'(', b'.'];
const END_CONS_CHAR: u8 = b')';
const COMMENT_CHAR: u8 = b';';
const SPACE_CHARS: [u8; 2] = [b' ', b'\t'];
#[derive(Debug, PartialOrd, PartialEq, Eq, Ord, Clone, Copy)]
pub enum TokenType {
    StartCons,
    DotCons,
    EndCons,
    Expression,
    Comment,
}
#[derive(PartialOrd, PartialEq, Eq, Ord, Clone, Copy)]
pub struct Token<'a> {
    pub bytes: &'a [u8],
    pub index: usize,
    pub t_type: TokenType,
}
impl<'a> Debug for Token<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Token {{ index: {}, type: {:?} value: {} }}",
            self.index,
            self.t_type,
            String::from_utf8_lossy(self.bytes)
        )
    }
}
#[derive(Clone, Debug, Default)]
pub struct Tokenizer<'a> {
    stream: &'a [u8],
    pub index: usize,
}
impl<'a> Tokenizer<'a> {
    #[must_use]
    pub fn new(stream: &'a [u8]) -> Self {
        Self { stream, index: 0 }
    }
    pub fn consume_whitespace(&mut self) {
        for c in &self.stream[self.index..] {
            if SPACE_CHARS.contains(c) || EOL_CHARS.contains(c) {
                self.index += 1;
            } else {
                break;
            }
        }
    }
    pub fn consume_until_whitespace(&mut self) {
        for c in &self.stream[self.index..] {
            match c {
                b' ' | b'\t' | b')' | b'\r' | b'\n' => {
                    break;
                }
                _ => {
                    self.index += 1;
                }
            }
        }
    }
    pub fn consume_until_eol(&mut self) {
        for c in &self.stream[self.index..] {
            match c {
                b'\r' | b'\n' => {
                    break;
                }
                _ => {
                    self.index += 1;
                }
            }
        }
    }
    pub fn consume_comment_chars(&mut self) {
        for c in &self.stream[self.index..] {
            if *c == b';' {
                break;
            } else {
                self.index += 1;
            }
        }
    }
}
impl<'a> Iterator for Tokenizer<'a> {
    type Item = Token<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        self.consume_whitespace();
        if self.stream.len() <= self.index {
            None
        } else {
            let chr = &self.stream[self.index];
            if START_CONS_CHARS.contains(chr) {
                let token = Token {
                    bytes: &self.stream[self.index..=self.index],
                    index: self.index,
                    t_type: TokenType::StartCons,
                };
                self.index += 1;
                Some(token)
            } else if chr == &END_CONS_CHAR {
                let token = Token {
                    bytes: &self.stream[self.index..=self.index],
                    index: self.index,
                    t_type: TokenType::EndCons,
                };
                self.index += 1;
                Some(token)
            } else if chr == &COMMENT_CHAR {
                self.consume_comment_chars();
                let start = self.index;
                self.consume_until_eol();
                let token = Token {
                    bytes: &self.stream[start..self.index],
                    index: self.index,
                    t_type: TokenType::Comment,
                };
                Some(token)
            } else {
                let start = self.index;
                self.consume_until_whitespace();
                let token = Token {
                    bytes: &self.stream[start..self.index],
                    index: start,
                    t_type: TokenType::Expression,
                };
                Some(token)
            }
        }
    }
}
