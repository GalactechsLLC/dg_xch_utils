//! Singleton fast-forward — the mempool's ability to rebase a spend of an older singleton
//! version onto the newest unspent version of the same singleton, so a transaction that raced a
//! block stays valid instead of dying as a double-spend.
//!
//! Port of chia_rs 0.42.1 `chia-consensus/src/fast_forward.rs` (`fast_forward_singleton`) and the
//! wheel's `supports_fast_forward` (chia_rs `wheel/src/api.rs`): given the spend of a
//! `singleton_top_layer_v1_1` coin, validate the lineage proof against the coin actually being
//! spent, then rewrite the solution's lineage proof + amount to spend `new_coin` (whose parent is
//! `new_parent`). The puzzle reveal is unchanged — only the solution moves.

use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::program::{Program, SerializedProgram};
use crate::clvm::sexp::SExp;
use crate::curry_and_treehash::{
    calculate_hash_of_quoted_mod_hash, curry_and_treehash, shatree_atom, shatree_pair,
};
use crate::errors::ClvmError;
use crate::traits::SizedBytes;
use num_traits::ToPrimitive;

/// The tree hash of `singleton_top_layer_v1_1.clsp` — chia_puzzles
/// `SINGLETON_TOP_LAYER_V1_1_HASH` (pinned by `dg_xch_puzzles::singleton_top_layer_v1_1`'s
/// `test_hashes`).
pub const SINGLETON_TOP_LAYER_V1_1_TREE_HASH: Bytes32 =
    Bytes32::const_hex("7faa3253bfddd1e0decb0906b2dc6247bbc4cf608f58345d173adb63e8b47c9f");

// The parsed `SINGLETON_STRUCT` curry argument: `(MOD_HASH . (LAUNCHER_ID . LAUNCHER_PH))`.
struct SingletonStruct {
    mod_hash: Bytes32,
    launcher_id: Bytes32,
    launcher_puzzle_hash: Bytes32,
}

/// The full puzzle hash of the singleton top layer curried with `(SINGLETON_STRUCT inner_puzzle)`,
/// given only the inner puzzle's hash. The mod-hash inside `SINGLETON_STRUCT` is the module's tree
/// hash and is quoted directly, not re-hashed as an atom.
fn singleton_puzzle_hash(
    inner_puzzle_hash: Bytes32,
    singleton_struct: &SingletonStruct,
) -> Bytes32 {
    let singleton_struct_hash = shatree_pair(
        &shatree_atom(singleton_struct.mod_hash.bytes().as_slice()),
        &shatree_pair(
            &shatree_atom(singleton_struct.launcher_id.bytes().as_slice()),
            &shatree_atom(singleton_struct.launcher_puzzle_hash.bytes().as_slice()),
        ),
    );
    curry_and_treehash(
        &calculate_hash_of_quoted_mod_hash(&singleton_struct.mod_hash),
        &[singleton_struct_hash, inner_puzzle_hash],
    )
}

fn err(msg: &str) -> ClvmError {
    ClvmError::InvalidSpendbundle(format!("fast-forward: {msg}"))
}

fn atom32(program: &Program<'_>, what: &str) -> Result<Bytes32, ClvmError> {
    let bytes = program
        .as_vec()
        .ok_or_else(|| err(&format!("{what} is not an atom")))?;
    Bytes32::parse(&bytes).map_err(|_| err(&format!("{what} is not 32 bytes")))
}

fn atom_u64(program: &Program<'_>, what: &str) -> Result<u64, ClvmError> {
    program
        .as_int()?
        .to_u64()
        .ok_or_else(|| err(&format!("{what} is not a u64")))
}

/// Rewrite `spend`'s solution so it spends `new_coin` (child of `new_parent`) instead of
/// `spend.coin`, validating the singleton shape and the existing lineage proof along the way —
/// chia_rs 0.42.1 `fast_forward_singleton`, check-for-check. Returns the new solution,
/// serialized.
///
/// # Errors
/// [`ClvmError::InvalidSpendbundle`] when the spend is not a rebasable
/// `singleton_top_layer_v1_1` spend: even amounts, puzzle-hash mismatches, a non-singleton mod
/// hash, an eve (2-element) lineage proof, an amount mismatch, or a lineage proof that does not
/// reproduce `spend.coin.parent_coin_info`.
pub fn fast_forward_singleton(
    spend: &CoinSpend,
    new_coin: &Coin,
    new_parent: &Coin,
) -> Result<SerializedProgram, ClvmError> {
    // a coin with an even amount is not a valid singleton (singleton_top_layer_v1_1.clsp)
    if spend.coin.amount & 1 == 0 || new_parent.amount & 1 == 0 || new_coin.amount & 1 == 0 {
        return Err(err("coin amount is even"));
    }
    // we can only fast-forward spends whose puzzle hash doesn't change
    if spend.coin.puzzle_hash != new_parent.puzzle_hash
        || spend.coin.puzzle_hash != new_coin.puzzle_hash
    {
        return Err(err("puzzle hash mismatch"));
    }

    let puzzle = Program::from_serial(&spend.puzzle_reveal)?;
    let (module, args) = puzzle.uncurry()?;
    let args = args.as_list();
    if args.len() != 2 {
        return Err(err("expected 2 curried singleton args"));
    }
    // SINGLETON_STRUCT = (MOD_HASH . (LAUNCHER_ID . LAUNCHER_PUZZLE_HASH))
    let (mod_hash_prog, struct_rest) = args[0]
        .as_pair()
        .ok_or_else(|| err("SINGLETON_STRUCT is not a pair"))?;
    let (launcher_id_prog, launcher_ph_prog) = struct_rest
        .as_pair()
        .ok_or_else(|| err("SINGLETON_STRUCT tail is not a pair"))?;
    let singleton_struct = SingletonStruct {
        mod_hash: atom32(&mod_hash_prog, "SINGLETON_STRUCT mod hash")?,
        launcher_id: atom32(&launcher_id_prog, "SINGLETON_STRUCT launcher id")?,
        launcher_puzzle_hash: atom32(&launcher_ph_prog, "SINGLETON_STRUCT launcher ph")?,
    };
    // the curried mod-hash argument AND the actual mod's tree hash must both be
    // singleton_top_layer_v1_1
    if singleton_struct.mod_hash != SINGLETON_TOP_LAYER_V1_1_TREE_HASH
        || module.tree_hash() != SINGLETON_TOP_LAYER_V1_1_TREE_HASH
    {
        return Err(err("not the singleton_top_layer_v1_1 mod hash"));
    }
    let inner_puzzle = &args[1];

    // solution = (lineage_proof my_amount inner_solution)
    let solution = Program::from_serial(&spend.solution)?;
    let solution_parts = solution.as_list();
    if solution_parts.len() != 3 {
        return Err(err("solution is not (lineage_proof amount inner_solution)"));
    }
    // a Lineage proof is (parent_parent_coin_info parent_inner_puzzle_hash parent_amount); an
    // eve (2-element) proof cannot be fast-forwarded
    let proof_parts = solution_parts[0].as_list();
    if proof_parts.len() != 3 {
        return Err(err("expected a (non-eve) lineage proof"));
    }
    let parent_inner_puzzle_hash = atom32(&proof_parts[1], "parent inner puzzle hash")?;
    let lineage_parent_parent = atom32(&proof_parts[0], "parent parent coin info")?;
    let lineage_parent_amount = atom_u64(&proof_parts[2], "parent amount")?;

    // if the solution's amount doesn't match the coin, it's an invalid spend — don't rebase it
    let solution_amount = atom_u64(&solution_parts[1], "solution amount")?;
    if spend.coin.amount != solution_amount {
        return Err(err("coin amount mismatch"));
    }

    // with the parent's inner puzzle hash we can reproduce the parent coin, which must be the
    // coin being spent's parent
    let parent_puzzle_hash = singleton_puzzle_hash(parent_inner_puzzle_hash, &singleton_struct);
    let parent_coin = Coin {
        parent_coin_info: lineage_parent_parent,
        puzzle_hash: parent_puzzle_hash,
        amount: lineage_parent_amount,
    };
    if parent_coin.name() != spend.coin.parent_coin_info {
        return Err(err("parent coin mismatch"));
    }

    if inner_puzzle.tree_hash() != parent_inner_puzzle_hash {
        return Err(err("inner puzzle hash mismatch"));
    }

    let puzzle_hash = puzzle.tree_hash();
    if puzzle_hash != new_parent.puzzle_hash || puzzle_hash != spend.coin.puzzle_hash {
        return Err(err("full puzzle hash mismatch"));
    }

    if new_coin.parent_coin_info != new_parent.name() {
        return Err(err("new coin is not a child of new parent"));
    }

    // rebuild the solution with the new parent's lineage frame and the new coin's amount
    let new_lineage: SExp<'static> = vec![
        SExp::from(new_parent.parent_coin_info),
        SExp::from(parent_inner_puzzle_hash),
        SExp::from(new_parent.amount),
    ]
    .into();
    let new_solution: SExp<'static> = vec![
        new_lineage,
        SExp::from(new_coin.amount),
        SExp::from(solution_parts[2].sexp()),
    ]
    .into();
    Program::to(new_solution).serialized()
}

/// Whether `spend` COULD be fast-forwarded — chia_rs `supports_fast_forward` (wheel/src/api.rs):
/// attempt the rebase onto a synthesized dummy parent; structural validity is the answer. The
/// mempool runs this on every `ELIGIBLE_FOR_FF` spend before looking up the singleton's latest
/// unspent lineage (chia mempool_manager.py:680).
#[must_use]
pub fn supports_fast_forward(spend: &CoinSpend) -> bool {
    let new_parent = Coin {
        parent_coin_info: Bytes32::default(),
        puzzle_hash: spend.coin.puzzle_hash,
        amount: spend.coin.amount,
    };
    let new_coin = Coin {
        parent_coin_info: new_parent.name(),
        puzzle_hash: spend.coin.puzzle_hash,
        amount: spend.coin.amount,
    };
    fast_forward_singleton(spend, &new_coin, &new_parent).is_ok()
}
