use dg_xch_pos::{pos_common, pos1, pos2};

#[test]
fn legacy_paths_reexport_the_same_types() {
    let legacy = dg_xch_pos::PathInfo {
        path: "plot.plot".into(),
        file_name: "plot.plot".into(),
    };
    let _: pos1::PathInfo = legacy;
    let params: dg_xch_pos2::ProofParams =
        pos2::ProofParams::new([0; 32].into(), 18, 2, false).unwrap();
    assert_eq!(params.k(), 18);
    let _: pos_common::finite_state_entropy::compress::CTable =
        dg_xch_pos::finite_state_entropy::compress::CTable::default();
    assert_eq!(
        dg_xch_pos::constants::FSE_MAX_SYMBOL_VALUE,
        pos_common::finite_state_entropy::FSE_MAX_SYMBOL_VALUE
    );
}
