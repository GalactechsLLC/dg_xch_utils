use super::*;

#[test]
fn codes_are_banded_and_stable() {
    assert_eq!(ChiaError::DoubleSpend.error_code(), 0x0001_0005);
    // Negative wire values keep their two's-complement image in the low half.
    assert_eq!(
        ChiaError::DoesNotExtend.error_code() & 0xFFFF,
        (-1i16) as u16 as u32
    );
    assert_eq!(ClvmError::TooManyAtoms.error_code(), 0x0002_001D);
    // A condition failure surfaces the consensus code, not a clvm wrapper.
    let e = ClvmError::ConditionFailure(ChiaError::AssertMyAmountFailed);
    assert_eq!(e.band(), ErrorBand::Consensus);
    assert_eq!(e.variant(), 116);
}

#[test]
fn dg_error_carries_band_and_code() {
    let e = DgError::new(&ChiaError::MempoolConflict);
    assert_eq!(e.band, ErrorBand::Consensus);
    assert_eq!(e.code, 0x0001_0013);
    assert!(e.to_string().contains("consensus"));
}
