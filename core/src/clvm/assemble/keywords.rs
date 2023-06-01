use lazy_static::lazy_static;
use std::collections::HashMap;

const PAIRS: [(u8, &str); 32] = [
    (0x01, "q"),
    (0x02, "a"),
    (0x03, "i"),
    (0x04, "c"),
    (0x05, "f"),
    (0x06, "r"),
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

lazy_static! {
    pub static ref KEYWORD_FROM_ATOM: HashMap<Vec<u8>, String> =
        HashMap::from(PAIRS.map(|(k, v)| (vec![k], v.to_string())));
    pub static ref KEYWORD_TO_ATOM: HashMap<String, Vec<u8>> =
        HashMap::from(PAIRS.map(|(k, v)| (v.to_string(), vec![k])));
}
