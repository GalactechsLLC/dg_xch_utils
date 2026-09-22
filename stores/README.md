# dg_xch_stores

Full-node storage interfaces and SQLite, optional PostgreSQL, and optional memory-mapped backends.

## Status

These are chain stores, not the wallet account database. Back up live SQLite databases using SQLite-aware tooling or stop the process first; copying only the main file while WAL writes are active is not a consistent backup. Treat backend changes and schema migrations as operational changes.

## Usage

Use `SqliteStore` with the query-shaped `BlockStore` and `CoinStore` traits. Enable `coin-index` for wallet coin queries and `hint` for hint lookups. `postgres` and `mmap` select additional implementations; the full-node CLI configures the chosen backend.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_stores
cargo doc -p dg_xch_stores --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_stores = { path = "../stores" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_stores
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
