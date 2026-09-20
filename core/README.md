# dg_xch_core

Shared blockchain types, consensus rules, CLVM execution, protocol messages, and TLS helpers.

## Status

Changes here can change consensus or wire compatibility. Default features include PostgreSQL type support and BLS; they do not start a database or node. The supplied public Chia CA is for peer compatibility, not private authentication.

## Usage

Import shared types from `blockchain`, consensus constants and `ChainDefinition` from `consensus`, and wire messages from `protocols`. Services must derive their network identity and constants from the same chain definition. For lean consumers, disable default features and select `bls` or the required storage features explicitly.

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
