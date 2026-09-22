# dg_xch_puzzles

CLVM puzzle definitions and helpers for standard coins, singletons, pools, CATs, DIDs, and NFTs.

## Status

Having a puzzle helper does not mean the desktop supports that wallet type. CAT/NFT/offers and pool-management UX remain separate work. Do not hand-build spends for valuable funds without testing their conditions and signatures.

## Usage

Use the appropriate puzzle module to construct puzzles and solutions, and verify expected tree hashes and spend conditions with `dg_xch_core`. Wallet transaction assembly uses `p2_delegated_puzzle_or_hidden_puzzle` and `clvm_puzzles`.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_puzzles
cargo doc -p dg_xch_puzzles --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_puzzles = { path = "../puzzles" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_puzzles
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [wallet](../wallet/README.md)
