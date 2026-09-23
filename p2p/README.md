# dg_xch_p2p

Peer address management, sessions, reconnection, and introducer discovery.

Introducers provide candidate endpoints, not trusted chain state. Peers still need handshake, network, rate-limit, and consensus validation. Private test-network address policies must be explicit.

## Usage

Configure `P2pSettings`, create the peer registry and `Supervisor`, then start supervised outbound connections and optional introducer discovery. Most operators should configure these through `dgx full-node`.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_p2p = { path = "../p2p" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [introducer](../introducer/README.md)
