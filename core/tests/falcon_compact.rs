#![cfg(feature = "falcon")]

use dg_xch_core::{
    blockchain::{full_block::FullBlock, sized_bytes::Bytes32},
    protocols::full_node::RespondBlocks,
    signatures::{
        extension::{AuthorizationExtension, FalconAuthorization},
        falcon::FalconSigner,
        falcon_block::{FalconHeaderAuthorization, public_key_id},
    },
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

fn fixture(count: usize) -> FalconAuthorization {
    let genesis = Bytes32::const_new([73; 32]);
    let plot = FalconSigner::generate().unwrap();
    let pool = FalconSigner::generate().unwrap();
    let mut auth = FalconAuthorization {
        labrador: None,
        genesis,
        header: (0..count)
            .map(|i| {
                FalconHeaderAuthorization::sign(
                    if i + 1 == count { &pool } else { &plot },
                    genesis,
                    &vec![i as u8; if i + 1 == count { 36 } else { 32 }],
                )
                .unwrap()
            })
            .collect(),
        transactions: Vec::new(),
    };
    auth.canonicalize();
    auth
}

// Independent pre-optimization encoding, retained to exercise actual V1 input.
fn v1(auth: &FalconAuthorization) -> Vec<u8> {
    let mut payload = auth.genesis.const_bytes().to_vec();
    payload.push(auth.header.len() as u8);
    for proof in &auth.header {
        payload.extend(proof.public_key);
        payload.extend((proof.message.len() as u16).to_be_bytes());
        payload.extend(&proof.message);
        payload.extend(proof.signature);
    }
    payload.extend((auth.transactions.len() as u32).to_be_bytes());
    for signature in &auth.transactions {
        payload.extend(signature);
    }
    let mut bytes = b"DGXAUT21".to_vec();
    bytes.extend((payload.len() as u32).to_be_bytes());
    bytes.extend(payload);
    bytes
}

fn decode(bytes: &[u8]) -> Result<AuthorizationExtension, std::io::Error> {
    let mut cursor = Cursor::new(bytes);
    let auth = AuthorizationExtension::from_bytes(&mut cursor, ChiaProtocolVersion::default())?;
    assert_eq!(cursor.position() as usize, bytes.len());
    Ok(auth)
}

#[test]
fn compact_headers_save_bytes_and_read_legacy_blocks_without_changing_identities() {
    let version = ChiaProtocolVersion::default();
    let bls: FullBlock = serde_json::from_str(include_str!(
        "../../full-node/tests/fixtures/full_block_5000000.json"
    ))
    .unwrap();
    for count in [4, 5] {
        let auth = fixture(count);
        let extension: AuthorizationExtension = auth.clone().into();
        let old = v1(&auth);
        let compact = extension.to_bytes(version).unwrap();
        assert_eq!(&compact[..8], b"DGXAUT22");
        assert_eq!(old.len() - compact.len(), (count - 2) * 897 - count - 1);
        assert_eq!(decode(&old).unwrap(), extension);
        assert_eq!(decode(&compact).unwrap(), extension);
        let mut block = bls.clone();
        block.authorization = extension.clone();
        assert_eq!(block.header_hash().unwrap(), bls.header_hash().unwrap());

        // Mix V1, V2 and bare BLS blocks in one native network message.
        let response = RespondBlocks {
            start_height: 0,
            end_height: 2,
            blocks: vec![block.clone(), bls.clone(), block.clone()],
        };
        let mut mixed = response.to_bytes(version).unwrap();
        let offset = mixed.windows(8).position(|v| v == b"DGXAUT22").unwrap();
        mixed.splice(offset..offset + compact.len(), old);
        let mut cursor = Cursor::new(mixed.as_slice());
        assert_eq!(
            RespondBlocks::from_bytes(&mut cursor, version).unwrap(),
            response
        );
        assert_eq!(cursor.position() as usize, mixed.len());

        // Transaction-only bundles keep their exact original wire bytes and IDs.
        let transaction_only = FalconAuthorization {
            header: vec![],
            transactions: vec![auth.header[0].signature],
            ..auth
        };
        assert_eq!(
            AuthorizationExtension::from(transaction_only.clone())
                .to_bytes(version)
                .unwrap(),
            v1(&transaction_only),
        );
    }
}

#[test]
fn compact_key_table_rejects_noncanonical_unused_and_out_of_range_entries() {
    let auth = fixture(4);
    let bytes = AuthorizationExtension::from(auth)
        .to_bytes(ChiaProtocolVersion::default())
        .unwrap();
    let key_count = 12 + 32;
    let first_key = key_count + 1;
    let second_key = first_key + 897;
    let header_count = second_key + 897;
    let first_index = header_count + 1;
    let mut malformed = Vec::new();
    for count in [0, 4, 5, 255] {
        let mut bad = bytes.clone();
        bad[key_count] = count;
        malformed.push(bad);
    }
    let mut duplicate = bytes.clone();
    duplicate.copy_within(first_key..second_key, second_key);
    malformed.push(duplicate);
    let mut unordered = bytes.clone();
    unordered[first_key..second_key].copy_from_slice(&bytes[second_key..header_count]);
    unordered[second_key..header_count].copy_from_slice(&bytes[first_key..second_key]);
    malformed.push(unordered);
    let mut invalid_index = bytes.clone();
    invalid_index[first_index] = 2;
    malformed.push(invalid_index);
    let mut unused = bytes.clone();
    let mut offset = first_index;
    for _ in 0..4 {
        unused[offset] = 0;
        let length = u16::from_be_bytes([bytes[offset + 1], bytes[offset + 2]]) as usize;
        offset += 1 + 2 + length + 666;
    }
    malformed.push(unused);
    let mut oversized_messages = bytes.clone();
    oversized_messages[first_index + 1..first_index + 3].copy_from_slice(&1025u16.to_be_bytes());
    malformed.push(oversized_messages);
    let mut too_many_transactions = bytes.clone();
    too_many_transactions[offset..offset + 4].copy_from_slice(&1025u32.to_be_bytes());
    malformed.push(too_many_transactions);
    let mut trailing = bytes.clone();
    trailing.push(0);
    let length = (trailing.len() - 12) as u32;
    trailing[8..12].copy_from_slice(&length.to_be_bytes());
    malformed.push(trailing);
    let mut unknown_version = bytes.clone();
    unknown_version[7] = b'3';
    malformed.push(unknown_version);
    for bad in malformed {
        assert!(decode(&bad).is_err());
    }
    for length in 7..bytes.len() {
        assert!(decode(&bytes[..length]).is_err());
    }
}

#[test]
fn verified_headers_bind_exact_messages_keys_chain_and_immutable_evidence() {
    let mut auth = fixture(5);
    let genesis = auth.genesis;
    let expected: Vec<_> = auth
        .header
        .iter()
        .map(|p| {
            (
                public_key_id(&p.public_key),
                p.message.clone(),
                p.reference(genesis),
            )
        })
        .collect();
    let verified = auth.verified(genesis).unwrap();
    assert!(auth.verify_messages(genesis, &expected));
    assert!(!auth.verify_messages(genesis, &expected[..4]));
    assert!(auth.verified(Bytes32::default()).is_err());
    for (key, message, reference) in &expected {
        assert!(verified.verify_header(key, message, reference));
        assert!(!verified.verify_header(&Default::default(), message, reference));
        assert!(!verified.verify_header(key, b"changed", reference));
        assert!(!verified.verify_header(key, message, &Default::default()));
    }
    auth.header[0].signature[80] ^= 1;
    assert!(auth.verified(genesis).is_err());
    assert!(!auth.verify_messages(genesis, &expected));
    // No untrusted mutation can change the evidence behind an accepted binding.
    assert_ne!(verified.authorization(), &auth);
    assert!(verified.verify_header(&expected[0].0, &expected[0].1, &expected[0].2));
    let bytes = AuthorizationExtension::from(auth)
        .to_bytes(ChiaProtocolVersion::default())
        .unwrap();
    assert!(
        decode(&bytes)
            .unwrap()
            .as_ref()
            .unwrap()
            .verified(genesis)
            .is_err()
    );
}

#[test]
fn legacy_pqcrypto_authorization_versions_are_rejected() {
    for magic in [b"DGXAUTH1", b"DGXAUTH2", b"DGXAUTH3"] {
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let error = AuthorizationExtension::from_bytes(
            &mut Cursor::new(bytes.as_slice()),
            ChiaProtocolVersion::default(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported Falcon authorization version")
        );
    }
}
