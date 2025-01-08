use crate::clvm::assemble::reader::Token;
use crate::clvm::program::Program;
use crate::clvm::sexp::{AtomBuf, IntoSExp, SExp};
use num_bigint::BigUint;
use once_cell::sync::Lazy;
use std::clone::Clone;
use std::collections::HashMap;

//Keywords
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

pub static CONS_SEXP: Lazy<SExp> = Lazy::new(|| SExp::Atom(AtomBuf::new(vec![CONS])));
pub static APPLY_SEXP: Lazy<SExp> = Lazy::new(|| SExp::Atom(AtomBuf::new(vec![APPLY])));
pub static QUOTE_SEXP: Lazy<SExp> = Lazy::new(|| SExp::Atom(AtomBuf::new(vec![QUOTE])));
pub static NULL_SEXP: Lazy<SExp> = Lazy::new(|| SExp::Atom(vec![].into()));
pub static ONE_SEXP: Lazy<SExp> = Lazy::new(|| SExp::Atom(AtomBuf::new(vec![1])));
pub static NULL_PROG: Lazy<Program> = Lazy::new(|| Program {
    sexp: NULL_SEXP.clone(),
    serialized: vec![],
});

//Assembler + Compiler
pub const EOL_CHARS: [u8; 2] = [b'\r', b'\n'];
pub const START_CONS_CHARS: [u8; 2] = [b'(', b'.'];
pub const END_CONS_CHAR: u8 = b')';
pub const CONS_CHARS: [u8; 3] = [b'(', b'.', b')'];
pub const QUOTE_CHARS: [u8; 2] = [b'\'', b'"'];
pub const SPACE_CHARS: [u8; 2] = [b' ', b'\t'];
pub const COMMENT_CHAR: u8 = b';';

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

//Compiler Flags
pub const INLINE_CONSTS: u32 = 0b_0000_0000_0000_0000_0000_0000_0000_0001;
pub const INLINE_DEFUNS: u32 = 0b_0000_0000_0000_0000_0000_0000_0000_0010;

//BLS SCHEMES
//const BASIC_SCHEME_DST: &[u8; 43] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";
pub const AUG_SCHEME_DST: &[u8; 43] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_AUG_";
// const POP_SCHEME_DST: &[u8; 43] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
// const AUG_SCHEME_POP_DST: &[u8; 43] = b"BLS_POP_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

//Chia Consensus
pub const POOL_SUB_SLOT_ITERS: u64 = 37_600_000_000;
// This number should be held constant and be consistent for every pool in the network. DO NOT CHANGE
pub const ITERS_LIMIT: u64 = POOL_SUB_SLOT_ITERS / 64;
pub static TWO_POW_256: Lazy<BigUint> = Lazy::new(|| BigUint::from(2u64).pow(256));

//Pool Singleton States
pub const POOL_PROTOCOL_VERSION: u8 = 1;
pub const SELF_POOLING: u8 = 1;
pub const LEAVING_POOL: u8 = 2;
pub const FARMING_TO_POOL: u8 = 3;
//Pool States
pub const POOL_STATE_IDENTIFIER: char = 'p';
pub const DELAY_TIME_IDENTIFIER: char = 't';
pub const DELAY_PUZZLEHASH_IDENTIFIER: char = 'h';

//SSL Info

pub const CHIA_CA_CRT: &str = r"-----BEGIN CERTIFICATE-----
MIIDKTCCAhGgAwIBAgIUXIpxI5MoZQ65/vhc7DK/d5ymoMUwDQYJKoZIhvcNAQEL
BQAwRDENMAsGA1UECgwEQ2hpYTEQMA4GA1UEAwwHQ2hpYSBDQTEhMB8GA1UECwwY
T3JnYW5pYyBGYXJtaW5nIERpdmlzaW9uMB4XDTIxMDEyMzA4NTEwNloXDTMxMDEy
MTA4NTEwNlowRDENMAsGA1UECgwEQ2hpYTEQMA4GA1UEAwwHQ2hpYSBDQTEhMB8G
A1UECwwYT3JnYW5pYyBGYXJtaW5nIERpdmlzaW9uMIIBIjANBgkqhkiG9w0BAQEF
AAOCAQ8AMIIBCgKCAQEAzz/L219Zjb5CIKnUkpd2julGC+j3E97KUiuOalCH9wdq
gpJi9nBqLccwPCSFXFew6CNBIBM+CW2jT3UVwgzjdXJ7pgtu8gWj0NQ6NqSLiXV2
WbpZovfrVh3x7Z4bjPgI3ouWjyehUfmK1GPIld4BfUSQtPlUJ53+XT32GRizUy+b
0CcJ84jp1XvyZAMajYnclFRNNJSw9WXtTlMUu+Z1M4K7c4ZPwEqgEnCgRc0TCaXj
180vo7mCHJQoDiNSCRATwfH+kWxOOK/nePkq2t4mPSFaX8xAS4yILISIOWYn7sNg
dy9D6gGNFo2SZ0FR3x9hjUjYEV3cPqg3BmNE3DDynQIDAQABoxMwETAPBgNVHRMB
Af8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQAEugnFQjzHhS0eeCqUwOHmP3ww
/rXPkKF+bJ6uiQgXZl+B5W3m3zaKimJeyatmuN+5ST1gUET+boMhbA/7grXAsRsk
SFTHG0T9CWfPiuimVmGCzoxLGpWDMJcHZncpQZ72dcy3h7mjWS+U59uyRVHeiprE
hvSyoNSYmfvh7vplRKS1wYeA119LL5fRXvOQNW6pSsts17auu38HWQGagSIAd1UP
5zEvDS1HgvaU1E09hlHzlpdSdNkAx7si0DMzxKHUg9oXeRZedt6kcfyEmryd52Mj
1r1R9mf4iMIUv1zc2sHVc1omxnCw9+7U4GMWLtL5OgyJyfNyoxk3tC+D3KNU
-----END CERTIFICATE-----";

pub const CHIA_CA_KEY: &str = r"-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAzz/L219Zjb5CIKnUkpd2julGC+j3E97KUiuOalCH9wdqgpJi
9nBqLccwPCSFXFew6CNBIBM+CW2jT3UVwgzjdXJ7pgtu8gWj0NQ6NqSLiXV2WbpZ
ovfrVh3x7Z4bjPgI3ouWjyehUfmK1GPIld4BfUSQtPlUJ53+XT32GRizUy+b0CcJ
84jp1XvyZAMajYnclFRNNJSw9WXtTlMUu+Z1M4K7c4ZPwEqgEnCgRc0TCaXj180v
o7mCHJQoDiNSCRATwfH+kWxOOK/nePkq2t4mPSFaX8xAS4yILISIOWYn7sNgdy9D
6gGNFo2SZ0FR3x9hjUjYEV3cPqg3BmNE3DDynQIDAQABAoIBAGupS4BJdx8gEAAh
2VDRqAAzhHTZb8j9uoKXJ+NotEkKrDTqUMiOu0nOqOsFWdYPo9HjxoggFuEU+Hpl
a4kj4uF3OG6Yj+jgLypjpV4PeoFM6M9R9BCp07In2i7DLLK9gvYA85SoVLBd/tW4
hFH+Qy3M+ZNZ1nLCK4pKjtaYs0dpi5zLoVvpEcEem2O+aRpUPCZqkNwU0umATCfg
ZGfFzgXI/XPJr8Uy+LVZOFp3PXXHfnZZD9T5AjO/ViBeqbMFuWQ8BpVOqapNPKj8
xDY3ovw3uiAYPC7eLib3u/WoFelMc2OMX0QljLp5Y+FScFHAMxoco3AQdWSYvSQw
b5xZmg0CgYEA6zKASfrw3EtPthkLR5NBmesI4RbbY6iFVhS5loLbzTtStvsus8EI
6RQgLgAFF14H21YSHxb6dB1Mbo45BN83gmDpUvKPREslqD3YPMKFo5GXMmv+JhNo
5Y9fhiOEnxzLJGtBB1HeGmg5NXp9mr2Ch9u8w/slfuCHckbA9AYvdxMCgYEA4ZR5
zg73+UA1a6Pm93bLYZGj+hf7OaB/6Hiw9YxCBgDfWM9dJ48iz382nojT5ui0rClV
5YAo8UCLh01Np9AbBZHuBdYm9IziuKNzTeK31UW+Tvbz+dEx7+PlYQffNOhcIgd+
9SXjoZorQksImKdMGZld1lEReHuBawq92JQvtY8CgYEAtNwUws7xQLW5CjKf9d5K
5+1Q2qYU9sG0JsmxHQhrtZoUtRjahOe/zlvnkvf48ksgh43cSYQF/Bw7lhhPyGtN
6DhVs69KdB3FS2ajTbXXxjxCpEdfHDB4zW4+6ouNhD1ECTFgxBw0SuIye+lBhSiN
o6NZuOr7nmFSRpIZ9ox7G3kCgYA4pvxMNtAqJekEpn4cChab42LGLX2nhFp7PMxc
bqQqM8/j0vg3Nihs6isCd6SYKjstvZfX8m7V3/rquQxWp9oRdQvNJXJVGojaDBqq
JdU7V6+qzzSIufQLpjV2P+7br7trxGwrDx/y9vAETynShLmE+FJrv6Jems3u3xy8
psKwmwKBgG5uLzCyMvMB2KwI+f3np2LYVGG0Pl1jq6yNXSaBosAiF0y+IgUjtWY5
EejO8oPWcb9AbqgPtrWaiJi17KiKv4Oyba5+y36IEtyjolWt0AB6F3oDK0X+Etw8
j/xlvBNuzDL6gRJHQg1+d4dO8Lz54NDUbKW8jGl+N/7afGVpGmX9
-----END RSA PRIVATE KEY-----";

pub const ALL_PRIVATE_NODE_NAMES: [&str; 8] = [
    "full_node",
    "wallet",
    "farmer",
    "harvester",
    "timelord",
    "crawler",
    "data_layer",
    "daemon",
];

pub const ALL_PUBLIC_NODE_NAMES: [&str; 6] = [
    "full_node",
    "wallet",
    "farmer",
    "introducer",
    "timelord",
    "data_layer",
];
