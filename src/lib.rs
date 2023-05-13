pub mod clients;
pub mod clvm;
pub mod consensus;
pub mod keys;
pub mod plots;
pub mod proof_of_space;
pub mod types;
pub mod utils;
pub mod wallet;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}
