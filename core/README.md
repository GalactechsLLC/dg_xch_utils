# dg_xch_core

## Wallet network anchors

`ChiaNetwork::genesis_header_hash()` supplies the pinned height-zero header hash for mainnet (`d780d22c7a87c9e01d98b49a0910f6701c3b95015741316b3fda042e5d7b81d2`) and testnet11 (`3068458e6ce87dbb5e2ace5378bb84185cb0638da84ab28c39153f665e7b2c97`). These are not `GENESIS_CHALLENGE`. The desktop uses these constants rather than accepting a replacement from settings or from its connected node.

The hashes were sourced from height-one `prev_hash` records from the [Coinset mainnet RPC](https://api.coinset.org/get_block_record_by_height) and [testnet11 RPC](https://testnet11.api.coinset.org/get_block_record_by_height) on 2026-09-22, using `{"height":1}`. These lookups are not performed at runtime. [Chia documents testnet11 as its only supported testnet](https://docs.chia.net/reference-client/install-and-setup/testnets/). Older network consensus presets remain available to library callers, but have no bundled wallet header anchor; retired networks require an explicit custom definition and independently verified header hash in the desktop.

Shared blockchain types, consensus rules, CLVM execution, protocol messages, and TLS helpers.

## Status

Changes here can change consensus or wire compatibility. Default features include PostgreSQL type support and BLS; they do not start a database or node. The supplied public Chia CA is for peer compatibility, not private authentication.

## Usage

Import shared types from `blockchain`, `ChainSelection` and `ChainDefinition` from `consensus::chain_definition`, and wire messages from `protocols`. `ChainSelection::default()` resolves to unmodified Chia mainnet. `Chia(ChiaNetwork)`, `Dgx`, and `Custom(ChainDefinition)` resolve shared constants, handshake identity and bootstrap policy once at startup. Select capabilities with Cargo features; do not compile a different consensus implementation per network.

Serialized selections are Chia network names such as `"mainnet"`, `"dgx"`, or a custom definition object. The version-2 custom definition pins its baseline and explicit activation/work parameters. Development presets keep real proof verification and 1024-bit VDFs, but lower work and eligibility settings for a small farm. They are separate chains, not production overrides. Legacy objects without `consensus` retain their version-1 identity. `ChainDefinition::default()` remains that legacy custom builder for compatibility; it is not the default chain selection. Never regenerate a running chain's genesis identity as an upgrade mechanism.

For lean consumers, disable default features and select `bls` or the required storage features explicitly.

On Unix, TLS private-key files must be regular, non-symlinked files with a single hard link and owner-only permissions, usually `0600`. Existing files are rejected rather than silently repaired; correct permissions only on keys you own, and restore incomplete certificate/key pairs from a matching backup. Keep their parent directories trusted. Windows ACL enforcement is not implemented by these helpers.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_core
cargo doc -p dg_xch_core --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_core = { path = "../core", default-features = false, features = ["bls"] }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_core
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
