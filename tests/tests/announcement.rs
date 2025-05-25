use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;

#[test]
fn test_announcement_name_without_morph_bytes() {
    let origin_info = Bytes32::from([0u8; 32]);
    let message = b"Test message".to_vec();
    let announcement = Announcement {
        origin_info,
        message: message.clone(),
        morph_bytes: None,
    };
    let name = announcement.name();

    let mut buf = Vec::new();
    buf.extend_from_slice(origin_info.bytes().as_slice());
    buf.extend_from_slice(&message);
    let expected_hash = Bytes32::new(hash_256(buf));

    assert_eq!(name, expected_hash);
}

#[test]
fn test_announcement_name_with_morph_bytes() {
    let origin_info = Bytes32::from([1u8; 32]);
    let message = b"Another test message".to_vec();
    let morph_bytes = b"Morph data".to_vec();
    let announcement = Announcement {
        origin_info,
        message: message.clone(),
        morph_bytes: Some(morph_bytes.clone()),
    };
    let name = announcement.name();

    let mut morph_buf = Vec::new();
    morph_buf.extend_from_slice(&morph_bytes);
    morph_buf.extend_from_slice(&message);
    let morph_hash = hash_256(morph_buf);

    let mut buf = Vec::new();
    buf.extend_from_slice(origin_info.bytes().as_slice());
    buf.extend_from_slice(&morph_hash);
    let expected_hash = Bytes32::new(hash_256(buf));

    assert_eq!(name, expected_hash);
}

#[test]
fn test_announcement_name_with_empty_message() {
    let origin_info = Bytes32::from([2u8; 32]);
    let message = Vec::new();
    let announcement = Announcement {
        origin_info,
        message: message.clone(),
        morph_bytes: None,
    };
    let name = announcement.name();

    let mut buf = Vec::new();
    buf.extend_from_slice(origin_info.bytes().as_slice());
    let expected_hash = Bytes32::new(hash_256(buf));

    assert_eq!(name, expected_hash);
}
