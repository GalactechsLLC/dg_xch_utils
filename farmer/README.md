# dg_xch_farmer

Integrated farmer and harvester, imported from `GalactechsLLC/dg_fast_farmer` at revision `270d44e1d885bd2d21ed658345042c954abfdcdc`. Its previous user interface is not included; management lives in the native desktop.

## Status

PoS1 plot reading, signage handling, signing, and pool communication are integrated. Each farmer runs independently and connects to its configured full node; there is no required central fleet farmer. PoS2 proof reconstruction exists as a bounded development path, including CPU/CUDA/Vulkan engines, but is not connected to production signage submission. It rebuilds tables and is not an efficient compact-plot disk solver.

## Run

From the repository root:

```sh
cargo run -p dg_xch_farmer --release --bin dg_xch_farmer -- --config /absolute/path/farmer.yaml
```

Use an existing compatible FastFarmer YAML or configure an account-backed farmer in the [desktop](../gui/README.md). For disposable local tests, the [Compose setup](../docker/README.md) generates per-service identities and configuration. A minimal legacy configuration has these fields:

```yaml
selected_network: mainnet
ssl_root_path: /absolute/path/farmer-ssl
fullnode_ws_host: localhost
fullnode_ws_port: 8444
fullnode_rpc_host: localhost
fullnode_rpc_port: 8444
payout_address: REPLACE_WITH_VALID_ADDRESS
farmer_info:
  - farmer_secret_key: REPLACE_WITH_DERIVED_FARMER_SECRET_KEY_HEX
    pool_secret_key: REPLACE_WITH_DERIVED_POOL_SECRET_KEY_HEX
pool_info: []
harvester_configs:
  druid_garden:
    plot_directories:
      - /absolute/path/plots
metrics: null
```

The placeholders intentionally are not usable keys. Do not paste a mnemonic in this file or derive production keys through shell command arguments. Restrict access to legacy YAML: it contains plaintext farming keys. Account-backed GUI mode derives farming keys without writing them to YAML. Back up the seed separately.

For a custom chain, set `selected_network` to its network ID and supply the complete `chain_definition` object matching every node. Unknown networks fail closed. Rust nodes normally share RPC/WebSocket port 8444; Chia RPC commonly uses a different port. Provision the correct private RPC CA and client identity. Pool farming additionally requires valid on-chain PlotNFT configuration and owner/authentication keys; an empty `pool_info` is not a pool setup.

## PoS2 and GPUs

Use [plotter proof checks](../plotter/README.md) for development PoS2 files. NVIDIA CUDA has Rust device kernels in a separate executable. AMD-capable Vulkan uses a WGSL AES shader and Rust matching/writing. Neither backend currently provides timely production network farming. Do not route production PoS2 plots to this service expecting accepted partials or blocks.

## Validation

```sh
cargo test -p dg_xch_farmer
```

Configuration and identifier tests do not replace live signage, pool, reorg, and long-running farming tests. Plot discovery keys use the full path identity so equal filenames on separate drives do not replace one another.

[Repository overview](../readme.md) · [PoS1](../proof_of_space/pos1/README.md) · [Servers](../servers/README.md)
