# dg_xch_keys

Mnemonic parsing, BLS key derivation, addresses, and fingerprints.

This is a key-derivation library, not a key vault. Durable encrypted account storage belongs to `dg_xch_wallet`. Back up seed phrases separately from the application database.

## Usage

Use `key_from_mnemonic_str` to derive a master key, the `master_sk_to_*` functions for role-specific keys, and `encode_puzzle_hash` / `decode_puzzle_hash` for addresses. Return derivation errors to the caller; never log secret keys or seed phrases.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_keys = { path = "../keys" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [wallet](../wallet/README.md)
