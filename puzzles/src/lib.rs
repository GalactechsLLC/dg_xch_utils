pub mod clvm_puzzles;
pub mod p2_delegated_puzzle_or_hidden_puzzle;
pub mod singleton;
pub mod utils;
pub mod p2_conditions;

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
