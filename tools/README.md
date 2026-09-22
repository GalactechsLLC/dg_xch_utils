# dg_xch_dev_tools

Developer binaries for inspecting nodes, importing corpora, deriving coin roots, and working with weight proofs.

## Status

These are developer tools, not production services. They may open databases, write fixture files, or contact the explicitly selected node. Use copied databases and disposable output directories; do not point experiments at a running production database.

## Build and usage

From the repository root:

```sh
cargo build -p dg_xch_dev_tools --bins
cargo test -p dg_xch_dev_tools
```

Available binaries include `block_fetch`, `coin_root_derive`, `corpus_import`, `validate_node_ws`, and `wp_build`. `leak_probe` requires `--features postgres`. Each source file under `src/bin` defines its arguments; not all legacy tools implement `--help`.

The disposable chain tools are `dg_xch_stack_init`, `dg_xch_stack_plot`, and `dg_xch_stack_check`. The initializer moved here from the farmer package. It creates per-node certificates and development keys without replacing existing identities. Plot preparation creates matching CPU plots sequentially for the selected farmers; it refuses to adopt unmarked existing plot directories or overwrite mismatched plots. The checker uses verified private-CA RPC to wait for real PoS2 genesis and subsequent blocks, compare a common height on all nodes, check zero genesis rewards, and require accepted blocks paying the configured farmers. See the [Compose instructions](../docker/README.md) for mounting the generated layout. These tools are not production key-management commands.

For example, `wp_build` reads a selected tip from a chain store:

```sh
cargo run -p dg_xch_dev_tools --bin wp_build -- \
  --db sqlite://./copied-chain.db --tip HEADER_HASH \
  --out ./weight-proof.bin --network mainnet
```

Replace the hash and network deliberately. PostgreSQL and mmap inputs require their matching build features. Diagnostics and fixtures can contain transaction or operational data; inspect them before sharing.

[Repository overview](../readme.md) · [Stores](../stores/README.md) · [Weight proofs](../weight-proof/README.md)
