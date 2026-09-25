# dg_xch_dev_tools

Developer utilities for inspecting Chia nodes, copied databases, fixtures, and weight proofs. These are separate tools; normal service startup belongs to `dgx`.

## Install and use

From the repository root:

```sh
cargo install --path tools --locked --bins
wp_build --db sqlite://./copied-chain.db --tip HEADER_HASH \
  --out ./weight-proof.bin --network mainnet
```

Replace `HEADER_HASH` with a tip in the copied database. Other installed tools include `block_fetch`, `coin_root_derive`, `corpus_import`, and `validate_node_ws`. Check their argument definitions in `src/bin`; not all legacy tools implement `--help`.

Optional `postgres` and `mmap` features enable those stores. `leak_probe` requires `postgres`. Use copied databases and disposable output paths, never experiments against a live production store.

## Integration helpers

`dg_xch_stack_init`, `dg_xch_stack_plot`, `dg_xch_stack_pool`, and `dg_xch_stack_check` support the isolated [Compose stack](../docker/README.md). They provision test identities, prepare plots, register pool accounts, and check actual chain/payout progress. They are not mainnet setup or key-management tools.

Pool preparation preserves original configurations and uses new portable plots. The checker distinguishes accepted partials from confirmed payouts; running containers alone are not a passing integration test.

[CLI installation](../cli/README.md) · [Stores](../stores/README.md) · [Weight proofs](../weight-proof/README.md)
