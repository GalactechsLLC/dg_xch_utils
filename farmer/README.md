# dg_xch_farmer

Integrated farmer and harvester, imported from `GalactechsLLC/dg_fast_farmer` at revision `270d44e1d885bd2d21ed658345042c954abfdcdc`. Its previous user interface is not included; management lives in the native desktop.

## Status

Each farmer runs independently and connects to its configured full node; there is no required central fleet farmer. PoS1 and PoS2 plot discovery, signage handling, plot-key signing, proof declaration and foliage-signature responses are connected. PoS2 quality search reads challenge fragments without rebuilding full plotting tables, checks chain-specific eligibility, and recovers only qualifying proofs. Recovered proofs are independently verified before submission. This wiring still needs end-to-end network and long-running deployment validation.

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

The default network remains Chia mainnet. Use `selected_network: dgx` for the built-in DGX definition, or supply a complete `chain_definition` and its matching network ID for a custom chain. Every node and farmer must resolve the same definition; unknown networks fail closed. Rust nodes normally share RPC/WebSocket port 8444; Chia RPC commonly uses a different port. Provision the correct private RPC CA and client identity. Pool partial submission additionally requires valid on-chain PlotNFT configuration and owner/authentication keys; an empty `pool_info` is not a pool setup.

## PoS2 and GPUs

PoS2 plots must use the chain's configured k, strength range and hash domain. Their memos must match a configured farmer key; plots bound to a pool public key also require its signing key. Activation height and plot filters follow the selected chain, not a hard-coded mainnet setting. Discovery refreshes every 30 seconds and deduplicates PoS2 plot IDs. CPU recovery is the default when no PoS2 section is configured.

Add a `pos2` section alongside `druid_garden` under `harvester_configs`:

```yaml
harvester_configs:
  pos2:
    plot_directories: [/absolute/path/pos2-plots]
    backend: cpu
    device: 0
    memory_mib: 1024
    max_entries: 4194304
    max_work: 2000000000
    search_hashes: 100000000
    max_qualities: 32
    deadline_ms: 20000
    parallelism: 1
```

An empty PoS2 directory list inherits the Druid Garden directories. Memory and proof-work limits apply per recovery; search limits apply per plot, and the deadline covers the whole signage job, including its wait for a worker. Concurrent jobs multiply managed memory requirements. Deadline cancellation reaches native proof work between bounded batches. A GPU driver call already in progress cannot be preempted. Increase limits deliberately for larger or stronger plots rather than treating a budget failure as a valid proof.

For AMD or another supported Vulkan device, build with `cargo build -p dg_xch_farmer --release --features vulkan` and set `backend: vulkan`. For native NVIDIA CUDA, build the [separate Rust helper](../plotter/cuda/README.md), set `backend: cuda`, and add `cuda_helper: /absolute/path/dg_xch_plotter_cuda`. Rebuild the helper to include its selected-quality proving interface. The farmer checks the device, invokes the helper without a shell, bounds its output, kills it when its deadline expires, and validates the requested chain independently. No private farming keys are passed to the helper.

`backend: auto` prefers a successfully probed native CUDA helper, otherwise a hardware Vulkan adapter; it does not silently choose CPU when no requested GPU is available. Device ordinals are backend-specific. Explicit selections never substitute CPU hashing. Standalone [plotter proof checks](../plotter/README.md) remain available for diagnosing a file before farming it.

The process logs its selected backend, loaded PoS2 plots, filter passes, eligible qualities, recovered proofs and failures. `FarmerService::pos2_status()` exposes the same counters to library users. Recovered-proof counts are not accepted-block counts. Unexpected service-task termination exits unsuccessfully; `RUST_LOG` sets the process log level.

## Validation

```sh
cargo test -p dg_xch_farmer
```

Configuration and identifier tests do not replace live signage, pool, reorg, and long-running farming tests. Plot discovery keys use the full path identity so equal filenames on separate drives do not replace one another.

[Repository overview](../readme.md) · [PoS1](../proof_of_space/pos1/README.md) · [Servers](../servers/README.md)
