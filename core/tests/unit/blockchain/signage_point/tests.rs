use super::*;
use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

fn full_sp() -> SignagePoint {
    let vdf = VdfInfo {
        challenge: Bytes32::from([7; 32]),
        number_of_iterations: 42,
        output: ClassgroupElement::get_default_element(),
    };
    let proof = VdfProof {
        witness_type: 0,
        witness: UnsizedBytes::default(),
        normalized_to_identity: false,
    };
    SignagePoint {
        cc_vdf: Some(vdf),
        cc_proof: Some(proof.clone()),
        rc_vdf: Some(vdf),
        rc_proof: Some(proof),
    }
}

fn round_trip(sp: &SignagePoint) -> SignagePoint {
    let v = ChiaProtocolVersion::default();
    let bytes = sp.to_bytes(v).expect("encode");
    SignagePoint::from_bytes(&mut Cursor::new(bytes.as_slice()), v).expect("decode")
}

#[test]
fn full_signage_point_round_trips() {
    let sp = full_sp();
    assert_eq!(round_trip(&sp), sp);
}

#[test]
fn sub_slot_start_signage_point_round_trips() {
    //each absent field is a single 0x00 presence tag, so the index-0 SP encodes to four zero bytes
    let sp = SignagePoint::sub_slot_start();
    assert!(sp.is_sub_slot_start());
    let v = ChiaProtocolVersion::default();
    let bytes = sp.to_bytes(v).expect("encode");
    assert_eq!(
        bytes,
        vec![0u8; 4],
        "all-None SP is four presence-tag zero bytes"
    );
    assert_eq!(round_trip(&sp), sp);
}

#[test]
fn garbage_presence_tag_errs_not_panics() {
    //a non-0/1 presence byte must return Err, never panic
    let v = ChiaProtocolVersion::default();
    let garbage = [2u8, 0, 0, 0];
    assert!(SignagePoint::from_bytes(&mut Cursor::new(&garbage[..]), v).is_err());
}

#[test]
fn truncated_input_errs_not_panics() {
    // A "Some" cc_vdf tag with no following bytes must error, not panic.
    let v = ChiaProtocolVersion::default();
    let truncated = [1u8];
    assert!(SignagePoint::from_bytes(&mut Cursor::new(&truncated[..]), v).is_err());
}
