# dg_xch_node

The reusable full-node engine, mempool, slot state, synchronization, and consensus interfaces.

This package is a library, not a standalone daemon. It does not own HTTP routing or operator configuration. VDF dependencies currently prevent Windows MSVC node builds; the desktop has a separate dependency path.

## Usage

Embed the exported engine with a store and primitive verifier, or use `dg_full_node` through the `dgx full-node` command. `slots`, `unfinished`, `sync`, and `mempool` own node state rather than desktop UI state.

`SlotState` records bounded first-seen receipt times for validated signage points and end-of-slot bundles. Duplicate gossip does not refresh those timestamps. RPC consumers must also check that the corresponding object remains in the active slot cache; a timestamp alone does not establish chain membership.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_node = { path = "../node" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
