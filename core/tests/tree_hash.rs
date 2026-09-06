use dg_parser_macro::parse_program_hex;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::{Program, SerializedProgram};

parse_program_hex!(
    TEST,
    "ff02ffff01ff04ffff04ff02ffff04ff05ffff04ff5fff80808080ff8080ffff04ffff0132ff018080"
);
pub const TEST_HASH: Bytes32 =
    Bytes32::const_hex("1720d13250a7c16988eaf530331cefa9dd57a76b2c82236bec8bbbff91499b89");
static SERIAL_PROGRAM: once_cell::sync::Lazy<SerializedProgram> =
    once_cell::sync::Lazy::new(|| SerializedProgram::from_hex(TEST_HEX).unwrap());
static TEST_STATIC: once_cell::sync::Lazy<Program<'static>> =
    once_cell::sync::Lazy::new(|| Program::from_serial(&SERIAL_PROGRAM).unwrap());

#[test]
fn test_minimal_static_program() {
    let static_tree_hash = TEST_STATIC.tree_hash();
    let program_tree_hash = TEST_PROGRAM.tree_hash();
    let known_tree_hash = TEST_HASH;
    assert_eq!(static_tree_hash, known_tree_hash);
    assert_eq!(program_tree_hash, known_tree_hash);
}
