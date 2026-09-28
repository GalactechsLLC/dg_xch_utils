# dg_xch_stores

Full-node storage interfaces and SQLite, optional PostgreSQL, and optional memory-mapped backends.

These are chain stores, not the wallet account database. Back up live SQLite databases using SQLite-aware tooling or stop the process first; copying only the main file while WAL writes are active is not a consistent backup. Treat backend changes and schema migrations as operational changes.

## Usage

Use `SqliteStore` with the query-shaped `BlockStore` and `CoinStore` traits. Enable `coin-index` for wallet coin queries and `hint` for hint lookups. `postgres` and `mmap` select additional implementations; the full-node CLI configures the chosen backend.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_stores = { path = "../stores" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
