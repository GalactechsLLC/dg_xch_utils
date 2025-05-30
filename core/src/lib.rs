pub mod blockchain;
pub mod clvm;
pub mod config;
pub mod consensus;
pub mod constants;
pub mod curry_and_treehash;
pub mod errors;
pub mod formatting;
pub mod plots;
pub mod pool;
pub mod protocols;
pub mod puzzles;
pub mod ssl;
pub mod traits;
pub mod utils;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}
