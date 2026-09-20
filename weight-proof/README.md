# dg_xch_weight_proof

Weight-proof verification and serving for chain synchronization.

## Status

This is a consensus-sensitive library, not a replacement for validating downloaded blocks or a complete wallet light client. Fixture-based correctness tests and production performance validation are different checks.

## Usage

Call `validate_weight_proof` with the intended consensus constants and primitive verifier. `serve::WeightProofServer` constructs responses from chain storage. Operators normally reach this through the full node rather than starting a separate service.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_weight_proof
cargo doc -p dg_xch_weight_proof --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_weight_proof
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)

