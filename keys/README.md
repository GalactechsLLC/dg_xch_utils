# dg_xch_keys

Mnemonic parsing, BLS key derivation, addresses, and fingerprints.

## Status

This is a key-derivation library, not a key vault. Durable encrypted account storage belongs to `dg_xch_wallet`. Back up seed phrases separately from the application database.

## Usage

Use `key_from_mnemonic_str` to derive a master key, the `master_sk_to_*` functions for role-specific keys, and `encode_puzzle_hash` / `decode_puzzle_hash` for addresses. Return derivation errors to the caller; never log secret keys or seed phrases.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_keys
cargo doc -p dg_xch_keys --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_keys = { path = "../keys" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_keys
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [wallet](../wallet/README.md)
