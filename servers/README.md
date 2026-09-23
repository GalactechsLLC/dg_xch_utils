# dg_xch_servers

Shared Chia-compatible WebSocket transport and protocol adapters.

The full node and introducer share this transport without depending on each other's application state. Public peer certificates do not grant private administrative access; timelord and RPC connections need their own trust policy.

## Usage

`app_config::{AppConfig, config_dir, default_paths}` shares versioned storage paths between CLI and desktop. `dgx init` writes the profile after setup succeeds. Loading is bounded to 64 KiB and rejects unsupported versions, relative storage paths, and empty plot lists. `save_new` refuses overwrites. Embedders can use these helpers without depending on either application.

Embed `WebsocketServer` with a `WebsocketServerConfig`, peer map, handlers, and shutdown flag. `transport` contains reusable bounded Chia frame helpers. Use the full-node or introducer package for an executable service.

File-backed TLS identities use the [core TLS helpers](../core/README.md): on Unix, private keys require owner-only permissions, usually `0600`, and cannot be symlinks or hard-linked files. Existing permissive files fail closed instead of being silently changed.

`chain_config` loads bounded, validated chain manifests and initializes a chain directory without creating blocks or wallet keys. Writes refuse to replace existing files, and repeated initialization requires the same chain identity. The CLI, GUI and development tools share these helpers; consensus selection itself lives in `dg_xch_core` and defaults to Chia mainnet.

Both embedded and standalone listeners hold a semaphore permit for each inbound session. The default is 128 sessions; the full node reserves its configured outbound slots and sets the inbound ceiling to `target_peer_count - target_outbound`. Wallet, farmer, and timelord connections share that inbound budget. TLS and HTTP upgrades have separate deadlines.

Add this package as a workspace/path dependency. Install [dgx](../cli/README.md) to run a service.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
