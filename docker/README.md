# Development stack

This Compose setup connects three isolated full nodes through `dg_xch_introducer`, attaches a separate farmer to each configured node, and runs the compact-proof timelord beside the CPU node. It generates its own chain identity, TLS certificates, and disposable farming keys.

**It is an integration harness, not a working block-production network.** The regular timelord scheduler and production PoS2 network-farming integration are unfinished. An empty stack will discover peers but will not produce its first block. GPU passthrough on a farmer container does not turn its existing PoS1 signage loop into a PoS2 GPU harvester.

## Requirements

- Docker Engine and Docker Compose with GPU device reservations support.
- Enough disk space for a Rust release build, container layers, and separate node databases.
- For NVIDIA: host driver and NVIDIA Container Toolkit configured for Docker.
- For AMD: a Linux render node exposed under `/dev/dri`; the image includes Mesa's Vulkan drivers.
- Disposable plots and test data only. Never mount a production wallet, real farming keys, or an existing chain database into this stack.

The image builds the native services and Vulkan plotter. It does not build the separate nightly CUDA executable or desktop GUI. Building the image is CPU-intensive; do it after source changes and lightweight checks are complete.

## Start the CPU stack

From the repository root:

```sh
mkdir -p docker/plots/cpu docker/plots/nvidia docker/plots/amd
docker compose -f compose.yaml config --quiet
docker compose -f compose.yaml build
docker compose -f compose.yaml up -d
docker compose -f compose.yaml ps
docker compose -f compose.yaml logs --tail=100 introducer node-cpu farmer-cpu timelord
```

The init service creates separate volumes for each node and farmer. The chain uses a random genesis seed and zero prefarm. Each node owns a different private CA. Farmers receive only their own public peer identity and node-signed private RPC identity, not the node CA signing key. The development master keys are plaintext in their private farmer volumes so test identities survive restarts; they are not suitable for production custody.

| Service | Connection and role |
| --- | --- |
| `introducer` | Internal port 8445; vets and returns full-node endpoints. |
| `node-cpu` | Internal port 8444; dedicated database and CA. |
| `node-nvidia` | Internal port 8444; dedicated database and CA. |
| `node-amd` | Internal port 8444; dedicated database and CA. |
| `farmer-cpu` | Shares `node-cpu`'s network namespace and connects to its verified local endpoint. |
| `farmer-nvidia` | Optional `nvidia` profile; shares `node-nvidia`'s network namespace. |
| `farmer-amd` | Optional `amd` profile; shares `node-amd`'s network namespace. |
| `timelord` | Compact-proof mode, connected to `node-cpu` over verified local TLS. |

No service publishes a host port. The bridge is internal. The readiness check verifies TLS and that `/metrics` answers; it does **not** declare the chain synchronized or capable of producing blocks. `/health` retains the node's actual liveness assessment.

Each full node registers with the introducer without manual peer addresses. To inspect connections from within the isolated stack:

```sh
docker compose exec node-cpu curl --fail --silent \
  --cacert /data/ssl/ca/private_ca.crt https://localhost:8444/metrics
docker compose exec farmer-cpu curl --fail --silent \
  --cacert /service/ssl/ca/private_ca.crt \
  --cert /service/ssl/farmer/private_farmer.crt \
  --key /service/ssl/farmer/private_farmer.key \
  -H 'Content-Type: application/json' -d '{}' \
  https://localhost:8444/get_node_details
```

## Enable GPU containers

For one NVIDIA card:

```sh
DGX_NVIDIA_DEVICE=0 docker compose --profile nvidia up -d
docker compose --profile nvidia run --rm --entrypoint dg_xch_plotter farmer-nvidia devices
```

For one AMD card, replace the device and group with the values on your host:

```sh
export DGX_AMD_RENDER_NODE=/dev/dri/renderD128
export DGX_RENDER_GID="$(stat -c %g "$DGX_AMD_RENDER_NODE")"
docker compose --profile amd up -d
docker compose --profile amd run --rm --entrypoint dg_xch_plotter farmer-amd devices
```

Use both `--profile nvidia --profile amd` when both are available. The example reserves one device of each vendor. Do not scale a GPU farmer service to obtain one worker per card: replicas would share the same device and node. Additional cards need separate service definitions, node instances, identity volumes, and initialization entries.

`DGX_CPU_PLOTS`, `DGX_NVIDIA_PLOTS`, and `DGX_AMD_PLOTS` override the read-only plot directories. New test identities do not match arbitrary existing plots. Read `plot-keys.json` in the relevant farmer volume to obtain the generated public keys before creating matching plots:

```sh
docker compose exec farmer-cpu cat /service/plot-keys.json
```

To exercise a low-k PoS2 plot through the real Vulkan backend without claiming network farming:

```sh
docker compose --profile amd run --rm --entrypoint dg_xch_plotter farmer-amd \
  prove-plot /plots/development.plot \
  --challenge 0000000000000000000000000000000000000000000000000000000000000000 \
  --backend vulkan --device 0
```

Add `--testnet` only if that plot was created for the testnet PoS2 domain. Use `farmer-nvidia` with the `nvidia` profile for the equivalent NVIDIA Vulkan check. The [plotter README](../plotter/README.md) describes limits and plotting commands. The separate [CUDA backend](../plotter/cuda/README.md) is not bundled in this image.

## Stop and reset

```sh
docker compose --profile nvidia --profile amd down
```

Volumes survive a normal shutdown. For a deliberate **destructive reset of all stack identities and databases**:

```sh
docker compose --profile nvidia --profile amd down --volumes
```

The next start generates a new chain and different keys, so old plots no longer belong to those generated farmer identities. Host-mounted plot directories are not deleted by `down --volumes`.

## Remaining integration work

The stack still needs a regular timelord, scalable on-disk PoS2 solving connected to signage/infusion submission, matched production plots, and funded-wallet/reorganization tests. No successful end-to-end block-production run is implied by container readiness or peer discovery.
