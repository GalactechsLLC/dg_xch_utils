# dg_xch_puzzles

CLVM puzzle definitions and helpers for standard coins, singletons, pools, CATs, DIDs, and NFTs.

Having a puzzle helper does not mean the desktop supports every operation for that wallet type. See the wallet and desktop packages for supported CAT, NFT, DID, and pool-management workflows. Do not hand-build spends for valuable funds without testing their conditions and signatures.

`pool_launch::launch_v1` constructs a v1 PlotNFT launcher and its required funding conditions. `pool_v2::PlotNft` constructs experimental v2 puzzles, memos, launches, and reward claims. The native v2 output is checked byte-for-byte against the pinned Chia reference described in the [pool README](../pool/README.md). Compiled v2 puzzle artifacts originate from that upstream revision; application-side construction and spend assembly are Rust. These helpers do not broadcast or maintain wallet reservations.

## Usage

Use the appropriate puzzle module to construct puzzles and solutions, and verify expected tree hashes and spend conditions with `dg_xch_core`. Wallet transaction assembly uses `p2_delegated_puzzle_or_hidden_puzzle` and `clvm_puzzles`.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_puzzles = { path = "../puzzles" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [wallet](../wallet/README.md)
