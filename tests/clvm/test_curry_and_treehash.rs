use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::curry_and_treehash::{calculate_hash_of_quoted_mod_hash, curry_and_treehash};

#[tokio::test]
fn test_curry_and_treehash() {
    let arbitrary_mod =
    // arbitrary_mod = p2_delegated_puzzle_or_hidden_puzzle.MOD
    // arbitrary_mod_hash = arbitrary_mod.get_tree_hash()
    //
    // // we don't really care what `arbitrary_mod` is. We just need some code
    //
    // quoted_mod_hash = calculate_hash_of_quoted_mod_hash(arbitrary_mod_hash)
    //
    // for v in range(500):
    //     args = [v, v * v, v * v * v];
    //     // we don't really care about the arguments either
    //     puzzle = arbitrary_mod.curry(*args)
    //     puzzle_hash_via_curry = puzzle.get_tree_hash()
    //     hashed_args = [Program.to(_).get_tree_hash() for _ in args]
    //     puzzle_hash_via_f = curry_and_treehash(quoted_mod_hash, *hashed_args)
    //     assert!(puzzle_hash_via_curry == puzzle_hash_via_f)
}