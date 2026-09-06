//! CLVM (de)serialization tests: fixture round-trips through `sexp_from_bytes`,
//! back-reference (0xfe) decoding, and decoder fuzz.
use super::*;

fn parse(hex: &str) -> SExp<'static> {
    let bytes = hex::decode(hex).unwrap();
    sexp_from_bytes(&mut Cursor::new(bytes.as_slice())).unwrap()
}

// Emits an atom size prefix claiming `size` bytes, followed by only 1000 bytes.
fn serialized_atom_overflow(size: u64) -> Vec<u8> {
    let mut size_blob: Vec<u8> = if size == 0 {
        vec![0x80]
    } else if size < 0x40 {
        vec![0x80 | size as u8]
    } else if size < 0x2000 {
        vec![0xC0 | (size >> 8) as u8, size as u8]
    } else if size < 0x0010_0000 {
        vec![0xE0 | (size >> 16) as u8, (size >> 8) as u8, size as u8]
    } else if size < 0x0800_0000 {
        vec![
            0xF0 | (size >> 24) as u8,
            (size >> 16) as u8,
            (size >> 8) as u8,
            size as u8,
        ]
    } else if size < 0x0004_0000_0000 {
        vec![
            0xF8 | (size >> 32) as u8,
            (size >> 24) as u8,
            (size >> 16) as u8,
            (size >> 8) as u8,
            size as u8,
        ]
    } else {
        vec![
            0xFC | ((size >> 40) & 0xFF) as u8,
            (size >> 32) as u8,
            (size >> 24) as u8,
            (size >> 16) as u8,
            (size >> 8) as u8,
            size as u8,
        ]
    };
    size_blob.extend(std::iter::repeat_n(0x01_u8, 1000));
    size_blob
}

// ("hello" "friend")
#[test]
fn deserialization_simple_list_round_trips() {
    let hex = "ff8568656c6c6fff86667269656e6480";
    let sexp = parse(hex);
    let reencoded = sexp_to_bytes(&sexp).unwrap();
    assert_eq!(hex::encode(reencoded.as_ref()), hex);
}

#[test]
fn deserialization_password_coin_round_trips() {
    let hex = "ff04ffff0affff0bff0280ffff01ffa02cf24dba5fb0a30e26e83b2ac5b9e29e1b16\
1e5c1fa7425e73043362938b98248080ffff05ffff01ff3380ffff05ff05ffff05ffff01ff6480ffff01ff808080808\
0ffff01ff8e77726f6e672070617373776f72648080";
    let sexp = parse(hex);
    let reencoded = sexp_to_bytes(&sexp).unwrap();
    assert_eq!(hex::encode(reencoded.as_ref()), hex);
}

#[test]
fn deserialization_large_numbers_round_trips() {
    let hex = "ff9c00f316271c7fc3908a8bef464e3945ef7a253609ffffffffffffffffffb00fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffa1ff22ea0179500526edb610f148ec0c614155678491902d6000000000000000000180";
    let sexp = parse(hex);
    let reencoded = sexp_to_bytes(&sexp).unwrap();
    assert_eq!(hex::encode(reencoded.as_ref()), hex);
}

// a truncated over-large atom must error, never panic
#[test]
fn overflow_atoms_error_not_panic() {
    for size in [
        0xFFFF_FFFF_u64,
        0x3_FFFF_FFFF,
        0xFF_FFFF_FFFF,
        0x1FF_FFFF_FFFF,
    ] {
        let bytes = serialized_atom_overflow(size);
        let result = sexp_from_bytes(&mut Cursor::new(bytes.as_slice()));
        assert!(
            result.is_err(),
            "over-large atom (size={size:#x}) must be rejected"
        );
    }
}

// A CLVM back-reference (0xfe) must decode to the same tree as its expanded
// form. ("foobar" "foobar") compressed vs expanded (the second element is a
// back-reference to the first).
#[test]
fn backreference_matches_expanded_tree() {
    let expanded = parse("ff86666f6f626172ff86666f6f62617280");
    let compressed_bytes = hex::decode("ff86666f6f626172fe01").unwrap();
    let compressed =
        sexp_from_bytes_backrefs(&mut Cursor::new(compressed_bytes.as_slice())).unwrap();
    assert_eq!(expanded, compressed);
    assert_eq!(expanded.tree_hash(), compressed.tree_hash());
}

// The back-reference-aware parser must be a superset: a plain (no-0xfe) blob
// decodes identically under both parsers.
#[test]
fn backref_parser_agrees_on_plain_blobs() {
    for hex in [
        "ff8568656c6c6fff86667269656e6480",
        "ff01ff02ff03ff0480",
        "80",
        "ffff0102ff0380",
    ] {
        let bytes = hex::decode(hex).unwrap();
        let plain = sexp_from_bytes(&mut Cursor::new(bytes.as_slice())).unwrap();
        let backref = sexp_from_bytes_backrefs(&mut Cursor::new(bytes.as_slice())).unwrap();
        assert_eq!(plain, backref, "parsers disagree on {hex}");
    }
}

// Decoder fuzz: arbitrary short byte strings must never panic; the parser
// returns Ok or Err. Covers both the plain and back-reference decoders.
#[test]
fn decoder_never_panics_on_garbage() {
    let mut count = 0u32;
    // deterministic pseudo-random walk over the byte space
    let mut state: u32 = 0x1234_5678;
    for _ in 0..20_000 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let len = (state as usize % 12) + 1;
        let mut buf = Vec::with_capacity(len);
        let mut s = state;
        for _ in 0..len {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            buf.push((s >> 16) as u8);
        }
        let _ = sexp_from_bytes(&mut Cursor::new(buf.as_slice()));
        let _ = sexp_from_bytes_backrefs(&mut Cursor::new(buf.as_slice()));
        count += 1;
    }
    assert_eq!(count, 20_000);
}
