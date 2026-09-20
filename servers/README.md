# dg_xch_servers

Shared Chia-compatible WebSocket transport and protocol adapters.

## Status

This package remains separate: the full node and discovery services share transport without needing each other's application state. Public peer certificates prove key possession, not membership in a private deployment. Timelord and administrative access require separate trust policy. There is no reason to merge the node, farmer, or GUI into this library.

## Usage

Embed `WebsocketServer` with a `WebsocketServerConfig`, peer map, handlers, and shutdown flag. `transport` contains reusable bounded Chia frame helpers. Use the full-node or introducer package for an executable service.

File-backed TLS identities use the [core TLS helpers](../core/README.md): on Unix, private keys require owner-only permissions, usually `0600`, and cannot be symlinks or hard-linked files. Existing permissive files fail closed instead of being silently changed.

Both embedded and standalone listeners hold a semaphore permit for each inbound session. The default is 128 sessions; the full node reserves its configured outbound slots and sets the inbound ceiling to `target_peer_count - target_outbound`. Wallet, farmer, and timelord connections share that inbound budget. TLS and HTTP upgrades have separate deadlines.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_servers
cargo doc -p dg_xch_servers --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_servers
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
