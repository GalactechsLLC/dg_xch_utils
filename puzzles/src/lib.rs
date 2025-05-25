pub mod cats;
pub mod clvm_puzzles;
pub mod p2_conditions;
pub mod p2_delegated_puzzle_or_hidden_puzzle;
pub mod singleton;
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
