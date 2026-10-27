macro_rules! test_bytes {
    ($($name: ident, $size: expr, $bad:ident, $large:ident),*) => {
        $(
            #[test]
            #[allow(non_snake_case)]
            fn $name() {
                use std::str::FromStr;
                use dg_xch_core::traits::SizedBytes;
                use dg_xch_core::blockchain::sized_bytes::$name;

                let test_bytes: String = format!("0x{:0width$x}", 8, width = $size * 2);
                let bytes = $name::from_str(&test_bytes).unwrap().bytes();

                let test_bytes_ptr: &String = &test_bytes;
                let test_bytes_str: &str = test_bytes.as_str();

                let bytes_ptr = $name::from_str(test_bytes_ptr).unwrap().bytes();
                let bytes_str = $name::from_str(test_bytes_str).unwrap().bytes();

                assert_eq!(bytes.len(), $size);
                assert_eq!(bytes, bytes_str);
                assert_eq!(bytes, bytes_ptr);
                assert_eq!(bytes_str, bytes_ptr);
            }

                #[test]
                #[should_panic]
                #[allow(non_snake_case)]
                fn $bad() {
                    use std::str::FromStr;
                    use dg_xch_core::traits::SizedBytes;
                    use dg_xch_core::blockchain::sized_bytes::$name;

                    let test_bytes: String = String::from("0x8");
                    let bytes = $name::from_str(&test_bytes).unwrap().bytes();
                    assert_eq!(bytes.len(), $size);
                }

                #[test]
                #[should_panic]
                #[allow(non_snake_case)]
                fn $large() {
                    use std::str::FromStr;
                    use dg_xch_core::traits::SizedBytes;
                    use dg_xch_core::blockchain::sized_bytes::$name;

                    let test_bytes: String = format!("0x{:0width$x}", 0, width = $size * 2 + 3);
                    let bytes = $name::from_str(&test_bytes).unwrap().bytes();
                    assert_eq!(bytes.len(), $size);
                }

        )*
    };
}

test_bytes!(
    Bytes4,
    4,
    test_bad_Bytes4,
    test_large_Bytes4,
    Bytes8,
    8,
    test_bad_Bytes8,
    test_large_Bytes8,
    Bytes32,
    32,
    test_bad_Bytes32,
    test_large_Bytes32,
    Bytes48,
    48,
    test_bad_Bytes48,
    test_large_Bytes48,
    Bytes96,
    96,
    test_bad_Bytes96,
    test_large_Bytes96,
    Bytes100,
    100,
    test_bad_Bytes100,
    test_large_Bytes100,
    Bytes480,
    480,
    test_bad_Bytes480,
    test_large_Bytes480
);

#[test]
pub fn test_sized_bytes_helpers() {
    use dg_xch_core::formatting::{hex_to_bytes, prep_hex_str, u64_to_bytes};

    let test_hex: String = "0x0000000000000008".to_string();
    let trimmed_hex_str = prep_hex_str(&test_hex);

    let numeric_value: u64 = 8;
    let numeric_value_with_leading: u64 = 8;

    let num_be_bytes = numeric_value.to_be_bytes().to_vec();
    let hex_bytes = hex_to_bytes(&test_hex).unwrap();

    assert_eq!(trimmed_hex_str, "0000000000000008");
    assert_eq!(num_be_bytes, hex_bytes);
    assert_eq!(
        u64_to_bytes(numeric_value_with_leading),
        u64_to_bytes(numeric_value)
    );
}
