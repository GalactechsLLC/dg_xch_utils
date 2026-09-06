use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::{Cursor, Error, ErrorKind};

struct LegacyValue;

impl ChiaSerialize for LegacyValue {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Result<Vec<u8>, Error> {
        if version == ChiaProtocolVersion::Chia0_0_34 {
            return Err(Error::new(ErrorKind::InvalidInput, "unsupported version"));
        }
        Ok(vec![0x42])
    }

    fn from_bytes(_: &mut Cursor<&[u8]>, _: ChiaProtocolVersion) -> Result<Self, Error> {
        Err(Error::new(ErrorKind::Unsupported, "test encoder only"))
    }
}

#[test]
fn nested_append_matches_fixed_wire_bytes() {
    let value = (
        vec![(0x1234u16, Some([1u8, 2, 3, 4])), (0xff00, None)],
        true,
        String::from("hi"),
    );
    let expected = [
        0, 0, 0, 2, 0x12, 0x34, 1, 1, 2, 3, 4, 0xff, 0, 0, 1, 0, 0, 0, 2, b'h', b'i',
    ];
    for version in [
        ChiaProtocolVersion::Chia0_0_34,
        ChiaProtocolVersion::Chia0_0_35,
        ChiaProtocolVersion::Chia0_0_36,
        ChiaProtocolVersion::Chia0_0_37,
    ] {
        let mut bytes = vec![0xee];
        value.append_bytes(&mut bytes, version).unwrap();
        assert_eq!(bytes[0], 0xee);
        assert_eq!(&bytes[1..], expected);
        assert_eq!(value.to_bytes(version).unwrap(), expected);
    }
}

#[test]
fn legacy_fallback_preserves_version_and_propagates_partial_append_errors() {
    let mut bytes = vec![0xee];
    Some(LegacyValue)
        .append_bytes(&mut bytes, ChiaProtocolVersion::Chia0_0_37)
        .unwrap();
    assert_eq!(bytes, [0xee, 1, 0x42]);
    let error = Some(LegacyValue)
        .append_bytes(&mut bytes, ChiaProtocolVersion::Chia0_0_34)
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    assert_eq!(bytes, [0xee, 1, 0x42, 1]);
}
