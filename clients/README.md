# dg_xch_clients

RPC, pool HTTP, and peer WebSocket clients.

Both RPC constructors verify server certificates and hostnames and refuse redirects. Supply an explicit `ClientSSLConfig` for private-CA RPC; the old implicit environment/public-Chia-CA identity fallback is removed. Public peer transport has a different trust model. A public Chia CA is not a private operator identity.

## Usage

Use `rpc::full_node::FullnodeClient::new_verified` for authenticated RPC connections, and `FullnodeAPI` for calls. Pass the private CA and client certificate/key through `ClientSSLConfig`. Peer clients live under `websocket`; pool requests live under `api::pool`.

`DefaultPoolClient::with_ca_certificates` adds explicitly trusted private pool CAs without disabling hostname verification. `api::pool_v2` provides experimental CHIP-0059 requests. `add_v2_account` enables token authentication for a launcher at a URL ending in `/v2`; v1 accounts remain unchanged. Client-side token refresh does not register farmers or update pool settings.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_clients = { path = "../clients" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [gui](../gui/README.md)
