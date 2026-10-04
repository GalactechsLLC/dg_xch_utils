use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::compile::{COMPAT_CHIA, Compiler};
use dg_xch_core::clvm::program::SerializedProgram;
use std::borrow::Cow;
use std::path::Path;

#[test]
fn canonical_puzzles_compile_to_published_bytes_and_hashes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/programs");
    let entries: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/programs.json")).unwrap();
    for entry in entries.as_array().unwrap() {
        let artifact = root.join(entry["path"].as_str().unwrap());
        let source = artifact.with_extension("");
        let text = std::fs::read(&source).unwrap();
        let dirs = [
            root.to_str().unwrap(),
            source.parent().unwrap().to_str().unwrap(),
        ];
        let compiler = Compiler::new(Cow::Owned(text), COMPAT_CHIA, 0, &dirs);
        let program = compiler
            .compile()
            .unwrap_or_else(|e| panic!("{}: {e}", source.display()));
        let expected = hex::decode(std::fs::read_to_string(&artifact).unwrap().trim()).unwrap();
        assert_eq!(
            program.serialized().unwrap().as_ref(),
            expected,
            "{}",
            source.display()
        );
        let hash: Bytes32 = entry["hash"].as_str().unwrap().parse().unwrap();
        assert_eq!(program.tree_hash(), hash, "{}", source.display());
        assert_eq!(
            SerializedProgram::from_bytes(&expected)
                .to_program()
                .unwrap()
                .tree_hash(),
            hash
        );
    }
}
