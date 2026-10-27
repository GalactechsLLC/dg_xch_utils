#[cfg(not(feature = "falcon"))]
use std::path::Path;
use std::process::Command;

#[cfg(not(feature = "falcon"))]
fn run_falcon_tools(root: &Path, plots: &Path) {
    for (binary, needs_plots) in [
        (env!("CARGO_BIN_EXE_dg_xch_stack_init"), false),
        (env!("CARGO_BIN_EXE_dg_xch_stack_plot"), true),
        (env!("CARGO_BIN_EXE_dg_xch_stack_check"), false),
    ] {
        let mut command = Command::new(binary);
        command
            .arg("--root")
            .arg(root)
            .args(["--chain-profile", "falcon"]);
        if needs_plots {
            command.arg("--plots-root").arg(plots);
        }
        let output = command.output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{binary} accepted Falcon unexpectedly"
        );
        assert!(
            error.contains("rebuild stack tools with --features falcon"),
            "{binary}: {error}"
        );
        assert!(
            output.stdout.is_empty(),
            "{binary} emitted acceptance output"
        );
    }
}

#[test]
#[cfg(not(feature = "falcon"))]
fn bls_tools_create_no_keys_or_plots_for_a_falcon_request() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("stack");
    let plots = directory.path().join("plots");
    run_falcon_tools(&root, &plots);
    assert!(!root.exists());
    assert!(!plots.exists());
}

#[test]
#[cfg(not(feature = "falcon"))]
fn existing_bls_state_cannot_satisfy_a_falcon_stack_request() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("stack");
    let common = root.join("common");
    std::fs::create_dir_all(&common).unwrap();
    let definition = dg_xch_core::consensus::chain_definition::ChainDefinition::development(
        "existing BLS stack".into(),
    );
    let bytes = serde_json::to_vec(&definition).unwrap();
    let chain = common.join("chain.json");
    std::fs::write(&chain, &bytes).unwrap();
    let plots = directory.path().join("plots");
    run_falcon_tools(&root, &plots);
    assert_eq!(std::fs::read(chain).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
    assert!(!plots.exists());
}

#[test]
#[cfg(feature = "falcon")]
fn falcon_stack_initialization_is_idempotent_and_profile_bound() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("stack");
    let initialize = || {
        Command::new(env!("CARGO_BIN_EXE_dg_xch_stack_init"))
            .arg("--root")
            .arg(&root)
            .args(["--chain-profile", "falcon"])
            .output()
            .unwrap()
    };
    let first = initialize();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let key_path = root.join("farmer-cpu/falcon-keys.bin");
    let key_bytes = std::fs::read(&key_path).unwrap();
    let chain =
        dg_xch_servers::chain_config::read_selection(&root.join("common/chain.json")).unwrap();
    assert!(!chain.constants().unwrap().signature_profile.is_bls());
    let keys = dg_xch_core::signatures::falcon_keys::FalconKeyring::load(&key_path).unwrap();
    assert_eq!(keys.plots.len(), 32);
    assert!(dg_xch_core::signatures::falcon_keys::FalconKeyring::generate(0).is_err());
    assert!(dg_xch_core::signatures::falcon_keys::FalconKeyring::generate(33).is_err());
    let second = initialize();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(key_bytes, std::fs::read(&key_path).unwrap());
    let bls = Command::new(env!("CARGO_BIN_EXE_dg_xch_stack_init"))
        .arg("--root")
        .arg(&root)
        .output()
        .unwrap();
    assert!(!bls.status.success());
    assert!(String::from_utf8_lossy(&bls.stderr).contains("stack profile differs"));
}

#[test]
#[cfg(feature = "falcon")]
fn legacy_falcon_chain_is_rejected_without_replacing_saved_identity() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("stack");
    let common = root.join("common");
    std::fs::create_dir_all(&common).unwrap();
    let definition = dg_xch_core::consensus::chain_definition::ChainDefinition::falcon_development(
        "legacy Falcon fixture".into(),
    );
    let mut legacy = serde_json::to_value(definition).unwrap();
    legacy["signature_profile"] = serde_json::json!("falcon512_sp1");
    let saved = serde_json::to_vec(&legacy).unwrap();
    let path = common.join("chain.json");
    std::fs::write(&path, &saved).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dg_xch_stack_init"))
        .arg("--root")
        .arg(&root)
        .args(["--chain-profile", "falcon"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("fresh development volumes, keys and plots"),
        "{error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    assert!(!root.join("farmer-cpu").exists());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
}
