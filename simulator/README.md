# dg_xch_simulator

Deterministic consensus fixtures, configurable block-production experiments, and development server entry points.

## Status

The simulator is for tests, not a production network or wallet service. Its optional PoS2 reference dependency is a test oracle and is not the native plotter implementation. Simulator success does not establish that the independent farmer/timelord network can produce blocks.

## Usage

From the repository root:

```sh
cargo test -p dg_xch_simulator
cargo run -p dg_xch_simulator --bin dg_xch_simulator
```

The legacy server entry point reads `SIMULATOR_HOSTNAME` and `SIMULATOR_PORT` (defaults `0.0.0.0` and `8080`). Bind it to `127.0.0.1` for local work; it is not an authenticated wallet node. The `server` feature exposes the separate `sim_node` binary. Inspect its source/options before deployment rather than assuming it matches the full-node CLI.

Library consumers use `HarnessConfig`, `SimConfig`, `PlotKeys`, and the deterministic chain/step helpers. The `pos2` feature is enabled by default; disable default features when reference PoS2 functionality is unnecessary. Keep seeds and small fixture parameters explicit so regressions are reproducible.

[Repository overview](../readme.md) · [Full node](../full-node/README.md) · [Timelord](../timelord/README.md)
