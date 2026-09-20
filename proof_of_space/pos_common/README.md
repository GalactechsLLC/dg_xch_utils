# dg_xch_pos_common

Shared finite-state entropy codecs used by proof-of-space implementations.

## Status

This is a low-level library, not a plot inspector or farmer. Callers remain responsible for file-format bounds, resource limits, and validating untrusted compressed input.

## Usage

Import `finite_state_entropy` through this package instead of copying codec implementations into PoS1, PoS2, or the plotter. Version-specific plot layouts and proof rules stay in their own packages.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_pos_common
cargo doc -p dg_xch_pos_common --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_pos_common
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../../readme.md)
- [plotter](../../plotter/README.md)

