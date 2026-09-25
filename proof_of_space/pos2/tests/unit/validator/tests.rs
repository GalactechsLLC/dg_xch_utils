use super::*;

fn validator() -> ProofValidator {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    ProofValidator::new(ProofParams::new(Bytes32::from(bytes), 28, 2, false).expect("params"))
        .expect("validator")
}

#[test]
fn an_arbitrary_pair_almost_never_validates() {
    let v = validator();
    let mut paired = 0;
    for i in 0..20_000u32 {
        if v.validate_table_1_pair(&[i, i.wrapping_mul(2_654_435_761) & 0x0FFF_FFFF])
            .is_some()
        {
            paired += 1;
        }
    }
    assert!(paired < 200, "{paired} of 20000 random pairs matched");
}

#[test]
fn a_random_proof_is_rejected() {
    let v = validator();
    let proof: [u32; TOTAL_XS_IN_PROOF] =
        std::array::from_fn(|i| ((i as u32).wrapping_mul(2_654_435_761)) & 0x0FFF_FFFF);
    assert!(
        v.validate_full_proof(&proof, Bytes32::from([5u8; 32]))
            .is_none()
    );
}

#[test]
fn an_all_zero_proof_is_rejected() {
    let v = validator();
    assert!(
        v.validate_full_proof(&[0u32; TOTAL_XS_IN_PROOF], Bytes32::from([1u8; 32]))
            .is_none()
    );
}

#[test]
fn the_levels_are_nested() {
    // A table 2 pairing can only exist when both of its table 1 pairings do.
    let v = validator();
    let mut xs = [0u32; 4];
    for i in 0..2000u32 {
        xs[0] = i;
        xs[1] = i.wrapping_mul(7919) & 0x0FFF_FFFF;
        xs[2] = i.wrapping_add(1);
        xs[3] = i.wrapping_mul(104_729) & 0x0FFF_FFFF;
        if v.validate_table_2_pairs(&xs).is_some() {
            assert!(v.validate_table_1_pair(&[xs[0], xs[1]]).is_some());
            assert!(v.validate_table_1_pair(&[xs[2], xs[3]]).is_some());
        }
    }
}
