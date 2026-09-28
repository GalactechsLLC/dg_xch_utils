# dg_xch_pos_common

Shared finite-state entropy codecs used by proof-of-space implementations.

This is a low-level library, not a plot inspector or farmer. Callers remain responsible for file-format bounds, resource limits, and validating untrusted compressed input.

## Usage

Import `finite_state_entropy` through this package instead of copying codec implementations into PoS1, PoS2, or the plotter. Version-specific plot layouts and proof rules stay in their own packages.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_pos_common = { path = "../proof_of_space/pos_common" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../../cli/README.md) to run services.

## Related packages

- [Repository overview](../../readme.md)
- [plotter](../../plotter/README.md)
