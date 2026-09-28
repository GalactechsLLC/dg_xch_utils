pub mod cats;
pub mod clvm_puzzles;
pub mod dids;
mod lineage_proof;
pub mod nft;
pub mod p2_conditions;
pub mod p2_delegated_puzzle_or_hidden_puzzle;
pub mod p2_parent;
pub mod settlement_payments;
pub mod singleton_launcher;
pub mod singleton_top_layer;
pub mod singleton_top_layer_v1_1;
pub mod tibet_swap;
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
pub mod pool_launch;
pub mod pool_v2;
