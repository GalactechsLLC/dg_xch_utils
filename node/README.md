# dg_xch_node

The reusable full-node engine, mempool, slot state, synchronization, and consensus interfaces.

## Status

This package is a library, not a standalone daemon. It does not own HTTP routing or operator configuration. VDF dependencies currently prevent Windows MSVC node builds; the desktop has a separate dependency path.

## Usage

Embed the exported engine with a store and primitive verifier, or use `dg_full_node` through the `dg full-node` command. `slots`, `unfinished`, `sync`, and `mempool` own node state rather than desktop UI state.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_node
cargo doc -p dg_xch_node --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_node
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)

