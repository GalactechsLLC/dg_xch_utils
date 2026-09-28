use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_farmer::utils::ensure_farmer_tls;
use std::io::Write;

#[test]
fn provisioned_client_identities_do_not_require_or_create_ca_signing_keys() {
    let directory = tempfile::tempdir().unwrap();
    for relative in [
        "ca/private_ca.crt",
        "ca/chia_ca.crt",
        "farmer/private_farmer.crt",
        "farmer/public_farmer.crt",
        "harvester/private_harvester.crt",
    ] {
        let path = directory.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, CHIA_CA_CRT).unwrap();
    }
    for relative in [
        "farmer/private_farmer.key",
        "farmer/public_farmer.key",
        "harvester/private_harvester.key",
    ] {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(directory.path().join(relative))
            .unwrap()
            .write_all(CHIA_CA_KEY.as_bytes())
            .unwrap();
    }
    ensure_farmer_tls(directory.path()).unwrap();
    assert!(!directory.path().join("ca/private_ca.key").exists());
    assert!(!directory.path().join("ca/chia_ca.key").exists());
    std::fs::remove_file(directory.path().join("farmer/private_farmer.key")).unwrap();
    assert!(ensure_farmer_tls(directory.path()).is_err());
    assert!(!directory.path().join("ca/private_ca.key").exists());
}
