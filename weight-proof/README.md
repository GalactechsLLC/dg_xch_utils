# dg_xch_weight_proof

Weight-proof verification and serving for chain synchronization.

This is a consensus-sensitive library, not a replacement for validating downloaded blocks or a complete wallet light client. Fixture-based correctness tests and production performance validation are different checks.

## Usage

Call `validate_weight_proof` with the intended consensus constants and primitive verifier. `serve::WeightProofServer` constructs responses from chain storage. Operators normally reach this through the full node rather than starting a separate service.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_weight_proof = { path = "../weight-proof" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [full-node](../full-node/README.md)
