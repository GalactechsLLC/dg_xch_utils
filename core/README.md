# dg_xch_core

Shared Chia types, consensus constants, CLVM execution, protocol messages, and TLS helpers.

## Use in Rust

From a neighboring workspace package:

```toml
[dependencies]
dg_xch_core = { path = "../core", default-features = false, features = ["bls"] }
```

Use `blockchain` for chain types, `protocols` for wire messages, and `consensus::chain_definition::ChainSelection` for network selection. `ChainSelection::default()` selects Chia mainnet. Default Cargo features include BLS and PostgreSQL types; neither starts a service.

`ChiaNetwork::genesis_header_hash()` provides pinned wallet trust anchors for mainnet and testnet11. These are block header hashes, **not** the genesis challenge. The GUI uses them without trusting a hash supplied by its node.

TLS helpers separate public peer certificates from private RPC identities. On Unix, private-key files must be regular, non-symlinked, singly linked, and owner-only (usually `0600`). Windows ACL enforcement is not provided by these helpers.

This is a library. See [dgx installation](../cli/README.md) to run a node.

[Full node](../full-node/README.md) · [Serialization](../serialize/README.md)
