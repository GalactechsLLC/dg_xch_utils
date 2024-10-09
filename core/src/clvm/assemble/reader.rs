use log::error;

const EOL_CHARS: [u8; 2] = [b'\r', b'\n'];
const QUOTE_CHARS: [u8; 2] = [b'\'', b'"'];
const CONS_CHARS: [u8; 3] = [b'(', b'.', b')'];
const SPACE_CHARS: [u8; 2] = [b' ', b'\t'];

pub const START_CONS: Token = Token {
    bytes: b"(",
    index: 0,
};
pub const DOT_CONS: Token = Token {
    bytes: b".",
    index: 0,
};
pub const END_CONS: Token = Token {
    bytes: b")",
    index: 0,
};
#[derive(Debug)]
pub struct Token<'a> {
    pub bytes: &'a [u8],
    pub index: usize,
}
impl PartialEq for Token<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl Eq for Token<'_> {}
pub struct Reader<'a> {
    stream: &'a [u8],
    pub index: usize,
}
impl<'a> Reader<'a> {
    pub fn new(stream: &'a [u8]) -> Self {
        Self { stream, index: 0 }
    }
    pub fn consume_whitespace(&mut self) {
        loop {
            for c in &self.stream[self.index..] {
                if SPACE_CHARS.contains(c) {
                    self.index += 1;
                } else {
                    break;
                }
            }
            if self.index >= self.stream.len() || self.stream[self.index] != b';' {
                break;
            }
            for c in &self.stream[self.index..] {
                if EOL_CHARS.contains(c) {
                    self.index += 1;
                } else {
                    break;
                }
            }
        }
    }
    pub fn consume_until_whitespace(&mut self) {
        for c in &self.stream[self.index..] {
            match c {
                b' ' | b'\t' | b')' => {
                    break;
                }
                _ => {
                    self.index += 1;
                }
            }
        }
    }
}
impl<'a> Iterator for Reader<'a> {
    type Item = Token<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        self.consume_whitespace();
        if self.stream.len() <= self.index {
            None
        } else {
            let chr = &self.stream[self.index];
            if CONS_CHARS.contains(chr) {
                let token = Token {
                    bytes: &self.stream[self.index..self.index + 1],
                    index: self.index,
                };
                self.index += 1;
                Some(token)
            } else if QUOTE_CHARS.contains(chr) {
                let start = self.index;
                let mut bs = false;
                self.index += 1;
                loop {
                    if self.stream.len() <= self.index {
                        error!("ERROR: Unterminated String at {}, ", start);
                        return None;
                    } else if bs {
                        bs = false;
                        self.index += 1;
                    } else if self.stream[self.index] == b'\\' {
                        bs = true;
                    } else if self.stream[self.index] == *chr {
                        self.index += 1;
                        return Some(Token {
                            bytes: &self.stream[start..self.index],
                            index: start,
                        });
                    } else {
                        self.index += 1;
                    }
                }
            } else {
                let start = self.index;
                self.consume_until_whitespace();
                Some(Token {
                    bytes: &self.stream[start..self.index],
                    index: start,
                })
            }
        }
    }
}
