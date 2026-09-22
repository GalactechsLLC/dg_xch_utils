# dg_xch_core

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

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_core
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
