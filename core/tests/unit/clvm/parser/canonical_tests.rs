use super::is_canonical_serialization;

// minimal encodings pass; non-minimal length prefixes and trailing garbage fail
#[test]
fn canonical_forms_pass() {
    assert!(is_canonical_serialization(&[0x80])); // nil
    assert!(is_canonical_serialization(&[0x01])); // single-byte atom
    assert!(is_canonical_serialization(&[0xff, 0x01, 0x80])); // (1 . nil)
    // 1-byte length prefix for a 3-byte atom.
    assert!(is_canonical_serialization(&[0x83, 0xaa, 0xbb, 0xcc]));
}

#[test]
fn non_minimal_length_prefix_fails() {
    // 2-byte prefix (0xC0 0x03) declaring length 3 < 64: 0x83 would have sufficed.
    let mut v = vec![0xc0, 0x03];
    v.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
    assert!(!is_canonical_serialization(&v));
}

#[test]
fn trailing_bytes_fail() {
    assert!(!is_canonical_serialization(&[0x80, 0x00]));
}

#[test]
fn truncated_stream_fails() {
    assert!(!is_canonical_serialization(&[0xff, 0x01]));
    assert!(!is_canonical_serialization(&[0x83, 0xaa]));
}
