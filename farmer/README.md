# dg_xch_farmer

An integrated Chia farmer and harvester, based on FastFarmer. Each instance connects directly to its full node; no central fleet farmer is required.

## Install and run

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx farmer --config /home/user/.dgx/config/farmer.yaml
```

Use an existing compatible FastFarmer configuration, or start an account-backed farmer from `dgx gui`. A minimal mainnet YAML looks like:

```yaml
selected_network: mainnet
ssl_root_path: /home/user/.dgx/config/ssl
fullnode_ws_host: localhost
fullnode_ws_port: 8444
fullnode_rpc_host: localhost
fullnode_rpc_port: 8444
payout_address: REPLACE_WITH_YOUR_XCH_ADDRESS
farmer_info:
  - farmer_secret_key: REPLACE_WITH_DERIVED_FARMER_KEY_HEX
    pool_secret_key: REPLACE_WITH_DERIVED_POOL_KEY_HEX
pool_info: []
harvester_configs:
  druid_garden:
    plot_directories: [/home/user/.dgx/data/plots]
metrics: null
```

Replace the keys, address, and paths before use. This YAML contains plaintext farming keys; restrict access and never store a mnemonic in it. GUI account mode derives keys in memory instead. Chia reference nodes commonly use RPC port 8555; use their matching TLS credentials.

## PoS2 and GPUs

PoS1 and PoS2 discovery and proof submission are implemented. Network activation rules still apply; this checkout's mainnet PoS2 activation constants need updating and validation before production use.

For PoS2, add this alongside `druid_garden` under `harvester_configs`:

```yaml
  pos2:
    plot_directories: [/home/user/.dgx/data/plots]
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

Choose `vulkan` for supported AMD/NVIDIA compute, or `cuda` with an absolute `cuda_helper` path to the [installed CUDA helper](../plotter/cuda/README.md). `auto` prefers a working CUDA helper, then hardware Vulkan. GPU failures do not silently switch to CPU.

Limits apply per recovery; parallel jobs multiply memory use. Device ordinals differ between backends. Proofs are independently verified before submission, but proof counters alone do not establish accepted blocks.

## Pool settings

Pool farming needs portable plots, an on-chain PlotNFT, and registered owner/authentication keys. An empty `pool_info` is self-farming, not pool setup.

`FarmerService::pool_settings()` reads the pool's current values; `update_pool_settings()` applies explicit edits. The GUI exposes both under **Farm → Pool settings**. Background polling never registers accounts or overwrites payout instructions or difficulty.

Private pools can set `pool_ca_certificates`. Experimental v2 accounts use `pooling_version: v2` and a URL ending in `/v2`; their synthetic PlotNFT key must match both owner and authentication configuration. V1 keys cannot be reused as a v2 identity. See the [pool package](../pool/README.md) for protocol details.

[Desktop](../gui/README.md) · [Plotter](../plotter/README.md)
