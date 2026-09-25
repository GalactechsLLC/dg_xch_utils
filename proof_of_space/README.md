# dg_xch_pos

Compatibility facade and consensus dispatch for PoS1 and PoS2.

Proof versions follow Chia's activation and phase-out rules. Shared entropy code lives in `pos_common`; version-specific algorithms stay in their own packages.

## Usage

Existing PoS1 imports remain available through this facade. New version-specific consumers can depend on `dg_xch_pos1` or `dg_xch_pos2` directly. `verify_and_get_quality_string` dispatches by proof version using consensus constants.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_pos = { path = "../proof_of_space" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [plotter](../plotter/README.md)
