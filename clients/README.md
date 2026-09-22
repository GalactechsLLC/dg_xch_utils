# dg_xch_clients

RPC, pool HTTP, and peer WebSocket clients.

## Status

Both RPC constructors verify server certificates and hostnames and refuse redirects. Supply an explicit `ClientSSLConfig` for private-CA RPC; the old implicit environment/public-Chia-CA identity fallback is removed. Public peer transport has a different trust model. A public Chia CA is not a private operator identity.

## Usage

Use `rpc::full_node::FullnodeClient::new_verified` for authenticated RPC connections, and `FullnodeAPI` for calls. Pass the private CA and client certificate/key through `ClientSSLConfig`. Peer clients live under `websocket`; pool requests live under `api::pool`.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_clients
cargo doc -p dg_xch_clients --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_clients = { path = "../clients" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_clients
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [gui](../gui/README.md)
