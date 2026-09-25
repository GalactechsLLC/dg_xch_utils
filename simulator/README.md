# dg_xch_simulator

Deterministic consensus fixtures and local block-production experiments. This is a developer tool, not a Chia mainnet node or an authenticated wallet service.

## Install and launch

From the repository root:

```sh
cargo install --path cli --locked
cargo install --path simulator --locked --bin dg_xch_simulator
dgx init
SIMULATOR_HOSTNAME=127.0.0.1 dgx simulator
```

Install both into the same Cargo bin directory: `dgx simulator` launches the companion beside it. The server reads `SIMULATOR_HOSTNAME` and `SIMULATOR_PORT` (default port 8080). Keep it bound to loopback.

The optional `server` feature provides a separate `sim_node` developer binary. The default `pos2` feature includes Chia's reference implementation as a test oracle and needs system zstd development files; it is not used by the native plotter.

## Library use

Use `HarnessConfig`, `SimConfig`, `PlotKeys`, and deterministic chain/step helpers. Keep seeds and small fixture parameters explicit. Disable default features when the reference PoS2 path is unnecessary.

For normal Chia operation, use [dgx full-node](../full-node/README.md). Simulator results do not establish live-network compatibility.
