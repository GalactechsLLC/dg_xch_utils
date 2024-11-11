use once_cell::sync::Lazy;
use std::collections::HashMap;
use crate::clvm::sexp::{IntoSExp, SExp};

pub const QUOTE: u8 = 0x01;
pub const APPLY: u8 = 0x02;
pub const IF: u8 = 0x03;
pub const CONS: u8 = 0x04;
pub const FIRST: u8 = 0x05;
pub const REST: u8 = 0x06;
pub const LIST: u8 = 0x07;
pub const EXCEPTION: u8 = 0x08;
pub const EQ: u8 = 0x09;
pub const STR_GREATER: u8 = 0x0a;
pub const SHA256: u8 = 0x0b;
pub const SUBSTR: u8 = 0x0c;
pub const STRLEN: u8 = 0x0d;
pub const CONCAT: u8 = 0x0e;
pub const ADD: u8 = 0x10;
pub const SUB: u8 = 0x11;
pub const MUL: u8 = 0x12;
pub const DIV: u8 = 0x13;
pub const DIVMOD: u8 = 0x14;
pub const GREATER: u8 = 0x15;
pub const A_SHIFT: u8 = 0x16;
pub const L_SHIFT: u8 = 0x17;
pub const LOG_AND: u8 = 0x18;
pub const LOG_OR: u8 = 0x19;
pub const LOG_XOR: u8 = 0x1a;
pub const LOG_NOT: u8 = 0x1b;
pub const POINT_ADD: u8 = 0x1d;
pub const PUB_KEY_FOR_EXP: u8 = 0x1e;
pub const NOT: u8 = 0x20;
pub const ANY: u8 = 0x21;
pub const ALL: u8 = 0x22;
pub const SOFTFORK: u8 = 0x24;

const PAIRS: [(u8, &str); 32] = [
    (QUOTE, "q"),
    (APPLY, "a"),
    (IF, "i"),
    (CONS, "c"),
    (FIRST, "f"),
    (REST, "r"),
    (LIST, "l"),
    (EXCEPTION, "x"),
    (EQ, "="),
    (STR_GREATER, ">s"),
    (SHA256, "sha256"),
    (SUBSTR, "substr"),
    (STRLEN, "strlen"),
    (CONCAT, "concat"),
    (ADD, "+"),
    (SUB, "-"),
    (MUL, "*"),
    (DIV, "/"),
    (DIVMOD, "divmod"),
    (GREATER, ">"),
    (A_SHIFT, "ash"),
    (L_SHIFT, "lsh"),
    (LOG_AND, "logand"),
    (LOG_OR, "logior"),
    (LOG_XOR, "logxor"),
    (LOG_NOT, "lognot"),
    (POINT_ADD, "point_add"),
    (PUB_KEY_FOR_EXP, "pubkey_for_exp"),
    (NOT, "not"),
    (ANY, "any"),
    (ALL, "all"),
    (SOFTFORK, "softfork"),
];

pub static KEYWORD_FROM_ATOM: Lazy<HashMap<Vec<u8>, String>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (vec![k], v.to_string()))));
pub static KEYWORD_TO_ATOM: Lazy<HashMap<String, Vec<u8>>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (v.to_string(), vec![k]))));
pub static B_KEYWORD_TO_SEXP: Lazy<HashMap<&[u8], SExp>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (v.as_bytes(), k.to_sexp()))));
pub static B_KEYWORD_TO_ATOM: Lazy<HashMap<&[u8], Vec<u8>>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (v.as_bytes(), vec![k]))));
