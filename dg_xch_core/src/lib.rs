pub mod blockchain;
pub mod clvm;
pub mod consensus;
pub mod curry_and_treehash;
pub mod keys;
pub mod errors;
pub mod plots;
pub mod pool;
pub mod puzzles;
pub mod ssl;

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
