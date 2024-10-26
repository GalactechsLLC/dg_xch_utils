use once_cell::sync::Lazy;
use std::collections::HashMap;
pub const QUOTE: u8 = 0x01;
pub const APPLY: u8 = 0x02;
pub const IF: u8 = 0x03;
pub const CONS: u8 = 0x04;
pub const FIRST: u8 = 0x05;
pub const REST: u8 = 0x06;

const PAIRS: [(u8, &str); 32] = [
    (QUOTE, "q"),
    (APPLY, "a"),
    (IF, "i"),
    (CONS, "c"),
    (FIRST, "f"),
    (REST, "r"),
    (0x07, "l"),
    (0x08, "x"),
    (0x09, "="),
    (0x0a, ">s"),
    (0x0b, "sha256"),
    (0x0c, "substr"),
    (0x0d, "strlen"),
    (0x0e, "concat"),
    (0x10, "+"),
    (0x11, "-"),
    (0x12, "*"),
    (0x13, "/"),
    (0x14, "divmod"),
    (0x15, ">"),
    (0x16, "ash"),
    (0x17, "lsh"),
    (0x18, "logand"),
    (0x19, "logior"),
    (0x1a, "logxor"),
    (0x1b, "lognot"),
    (0x1d, "point_add"),
    (0x1e, "pubkey_for_exp"),
    (0x20, "not"),
    (0x21, "any"),
    (0x22, "all"),
    (0x24, "softfork"),
];

pub static KEYWORD_FROM_ATOM: Lazy<HashMap<Vec<u8>, String>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (vec![k], v.to_string()))));
pub static KEYWORD_TO_ATOM: Lazy<HashMap<String, Vec<u8>>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (v.to_string(), vec![k]))));
pub static B_KEYWORD_TO_ATOM: Lazy<HashMap<&[u8], Vec<u8>>> =
    Lazy::new(|| HashMap::from(PAIRS.map(|(k, v)| (v.as_bytes(), vec![k]))));
