# dg_xch_vdf

Native class-group VDF proving and verification.

Consensus discriminant generation depends on GMP through `rug`; this is not a dependency-free pure-Rust build. Linux/macOS builds are supported by the workspace; Windows MSVC is not supported by this dependency path. Successful small proofs do not establish mainnet-speed proving throughput.

## Usage

Use `prove` for bounded local work and `verify_vdf` / `verify_n_wesolowski` for verification. Pass the consensus discriminant size and iterations explicitly. The timelord service wraps these primitives with operational limits.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_vdf = { path = "../vdf" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [timelord](../timelord/README.md)
