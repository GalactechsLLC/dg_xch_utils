use crate::constants::{COMMENT_CHAR, END_CONS_CHAR, EOL_CHARS, SPACE_CHARS, START_CONS_CHARS};
use std::borrow::Cow;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialOrd, PartialEq, Eq, Ord, Clone, Copy)]
pub enum TokenType {
    StartCons,
    DotCons,
    EndCons,
    Expression,
    Comment,
}
#[derive(PartialOrd, PartialEq, Eq, Ord, Clone)]
pub struct Token<'a> {
    pub bytes: Cow<'a, [u8]>,
    pub index: usize,
    pub t_type: TokenType,
}
impl Debug for Token<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Token {{ index: {}, type: {:?} value: {} }}",
            self.index,
            self.t_type,
            String::from_utf8_lossy(self.bytes.as_ref())
        )
    }
}
#[derive(Debug, Default)]
pub struct Tokenizer<'a> {
    stream: Cow<'a, [u8]>,
    pub index: AtomicUsize,
}
impl<'a> Tokenizer<'a> {
    #[must_use]
    pub fn new(stream: Cow<'a, [u8]>) -> Self {
        Self {
            stream,
            index: AtomicUsize::new(0),
        }
    }
    pub fn consume_whitespace(&self) {
        for c in &self.stream[self.index.load(Ordering::Relaxed)..] {
            if SPACE_CHARS.contains(c) || EOL_CHARS.contains(c) {
                self.index.fetch_add(1, Ordering::Relaxed);
            } else {
                break;
            }
        }
    }
    pub fn consume_until_whitespace(&self) {
        for c in &self.stream[self.index.load(Ordering::Relaxed)..] {
            match c {
                b' ' | b'\t' | b')' | b'\r' | b'\n' => {
                    break;
                }
                _ => {
                    self.index.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    pub fn consume_until_eol(&self) {
        for c in &self.stream[self.index.load(Ordering::Relaxed)..] {
            match c {
                b'\r' | b'\n' => {
                    break;
                }
                _ => {
                    self.index.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    pub fn consume_comment_chars(&self) {
        for c in &self.stream[self.index.load(Ordering::Relaxed)..] {
            if *c == b';' {
                break;
            } else {
                self.index.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    pub fn next_token(&'a self) -> Option<Token<'a>> {
        self.consume_whitespace();
        if self.stream.len() <= self.index.load(Ordering::Relaxed) {
            None
        } else {
            let chr = &self.stream[self.index.load(Ordering::Relaxed)];
            if START_CONS_CHARS.contains(chr) {
                let token = Token {
                    bytes: Cow::Borrowed(
                        &self.stream[self.index.load(Ordering::Relaxed)
                            ..=self.index.load(Ordering::Relaxed)],
                    ),
                    index: self.index.load(Ordering::Relaxed),
                    t_type: TokenType::StartCons,
                };
                self.index.fetch_add(1, Ordering::Relaxed);
                Some(token)
            } else if chr == &END_CONS_CHAR {
                let token = Token {
                    bytes: Cow::Borrowed(
                        &self.stream[self.index.load(Ordering::Relaxed)
                            ..=self.index.load(Ordering::Relaxed)],
                    ),
                    index: self.index.load(Ordering::Relaxed),
                    t_type: TokenType::EndCons,
                };
                self.index.fetch_add(1, Ordering::Relaxed);
                Some(token)
            } else if chr == &COMMENT_CHAR {
                self.consume_comment_chars();
                let start = self.index.load(Ordering::Relaxed);
                self.consume_until_eol();
                let token = Token {
                    bytes: Cow::Borrowed(&self.stream[start..self.index.load(Ordering::Relaxed)]),
                    index: self.index.load(Ordering::Relaxed),
                    t_type: TokenType::Comment,
                };
                Some(token)
            } else {
                let start = self.index.load(Ordering::Relaxed);
                self.consume_until_whitespace();
                let token = Token {
                    bytes: Cow::Borrowed(&self.stream[start..self.index.load(Ordering::Relaxed)]),
                    index: start,
                    t_type: TokenType::Expression,
                };
                Some(token)
            }
        }
    }
}
