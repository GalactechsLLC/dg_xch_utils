# dg_xch_vdf

Native class-group VDF proving and verification.

## Status

Consensus discriminant generation depends on GMP through `rug`; this is not a dependency-free pure-Rust build. Linux/macOS builds are supported by the workspace; Windows MSVC is not supported by this dependency path. Successful small proofs do not establish mainnet-speed proving throughput.

## Usage

Use `prove` for bounded local work and `verify_vdf` / `verify_n_wesolowski` for verification. Pass the consensus discriminant size and iterations explicitly. The timelord service wraps these primitives with operational limits.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_vdf
cargo doc -p dg_xch_vdf --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_vdf
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [timelord](../timelord/README.md)

