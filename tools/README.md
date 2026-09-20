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

For example, `wp_build` reads a selected tip from a chain store:

```sh
cargo run -p dg_xch_dev_tools --bin wp_build -- \
  --db sqlite://./copied-chain.db --tip HEADER_HASH \
  --out ./weight-proof.bin --network mainnet
```

Replace the hash and network deliberately. PostgreSQL and mmap inputs require their matching build features. Diagnostics and fixtures can contain transaction or operational data; inspect them before sharing.

[Repository overview](../readme.md) · [Stores](../stores/README.md) · [Weight proofs](../weight-proof/README.md)
