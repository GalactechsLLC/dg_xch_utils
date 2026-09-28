use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;

#[test]
fn malformed_hex_returns_an_error() {
    for input in ["not-hex", "f", "0x0g", "☃"] {
        assert!(UnsizedBytes::try_from(input).is_err());
        let encoded = serde_json::to_string(input).unwrap();
        assert!(serde_json::from_str::<UnsizedBytes>(&encoded).is_err());
    }
    let value = serde_json::from_str::<UnsizedBytes>("\"0x0102\"").unwrap();
    assert_eq!(value.as_slice(), &[1, 2]);
}
