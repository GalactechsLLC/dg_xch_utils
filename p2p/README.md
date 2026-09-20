# dg_xch_p2p

Peer address management, sessions, reconnection, and introducer discovery.

## Status

Introducers provide candidate endpoints, not trusted chain state. Peers still need handshake, network, rate-limit, and consensus validation. Private test-network address policies must be explicit.

## Usage

Configure `P2pSettings`, create the peer registry and `Supervisor`, then start supervised outbound connections and optional introducer discovery. Most operators should configure these through `dg full-node`.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_p2p
cargo doc -p dg_xch_p2p --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_p2p
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [introducer](../introducer/README.md)

