# dg_xch_pos

Compatibility facade and consensus dispatch for PoS1 and PoS2.

## Status

The facade does not make development PoS2 plots production-farmable. Activation and phase-out heights come from the chain definition. Shared entropy code lives in `pos_common`, not duplicated under each version.

## Usage

Existing PoS1 imports remain available through this facade. New version-specific consumers can depend on `dg_xch_pos1` or `dg_xch_pos2` directly. `verify_and_get_quality_string` dispatches by proof version using consensus constants.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_pos
cargo doc -p dg_xch_pos --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_pos = { path = "../proof_of_space" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_pos
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [plotter](../plotter/README.md)
