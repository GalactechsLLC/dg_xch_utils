# Development stack

Three isolated full nodes discover one another through `dg_xch_introducer`. Each farmer connects to its own node, and a regular CPU timelord advances the chain through the CPU node. CPU, native NVIDIA CUDA, and AMD/Vulkan farmers are independent processes, not a central farming service.

This is an integration-testing setup, not a claim of a completed production deployment. Container health only checks TLS and `/metrics`; the acceptance tool waits for real blocks and checks agreement. Wallet transfers, forced reorganizations, long-running operation, and hardware-specific performance need separate testing.

## Requirements

- Docker Engine and Compose, with GPU reservations support for NVIDIA.
- At least 16 GiB available to plot preparation, plus room for other running services. It creates k28 strength-2 plots sequentially; do not overlap it with benchmarks.
- A new host directory for disposable plots, writable by container UID 1000, and space for images and node databases. Owning the directory is not sufficient if your host UID differs; arrange access for UID 1000 without making unrelated directories writable.
- NVIDIA: host driver, NVIDIA Container Toolkit, and the optional CUDA image. The default target is the A4000's `sm_86`; set `DGX_CUDA_ARCH` for another GPU.
- AMD: Linux render-node access under `/dev/dri`. The ordinary image includes Mesa Vulkan drivers.

Never mount production keys, wallets or chain databases. Disposable master keys are plaintext inside private volumes. The ordinary image does not require CUDA; the optional CUDA image currently targets Linux amd64 only.

## Prepare

From the repository root, choose a new directory you own:

```sh
export DGX_PLOTS_ROOT=/mnt/nvmep1/tmp/dgx-compose-plots
mkdir -p "$DGX_PLOTS_ROOT"
docker compose config --quiet
docker compose build init-stack
docker compose run --rm init-stack
docker compose run --rm --no-deps prepare-plots
```

Preparation creates one matching CPU-generated plot for each of the CPU, NVIDIA and AMD identities, sequentially. It needs no GPU. Append `--farmers cpu` to prepare only CPU initially; later repeat with `--farmers nvidia,amd`. Matching existing plots are retained, not overwritten. Metadata checks do not scan every compressed payload. An unrelated nonempty plot directory is rejected.

The initializer generates an immutable development manifest, separate node CAs and per-farmer identities. This chain activates PoS2 at genesis, removes the prefarm and uses lower starting work and difficulty for a small farm. Proof verification stays enabled, including real 1024-bit VDFs. The timelord is deliberately paced at 27 iterations/second to leave time for CPU farming. These settings do not change Chia or the production DGX preset.

Old version-1 stack volumes are rejected rather than silently changed to another chain. To preserve an existing run, use a separate `COMPOSE_PROJECT_NAME` and plots directory.

## Start and check

```sh
docker compose up -d
docker compose ps
docker compose logs --tail=100 introducer node-cpu farmer-cpu timelord
docker compose run --rm --no-deps check-stack --require-farmer cpu
```

The checker waits up to 30 minutes by default. It uses verified private-CA RPC, requires PoS2 genesis and at least height 3 on all three nodes, compares hashes at a common height, rejects nonzero genesis reward coins, and requires an accepted non-genesis block paying the CPU farmer's target. This does not claim that a wallet has synchronized or spent the reward. Adjust `--min-height` and `--timeout-seconds` deliberately.

| Service | Role |
| --- | --- |
| `init-stack` | Idempotent development identities and configuration; no block injection |
| `introducer` | Vetted discovery on internal port 8445 |
| `node-cpu`, `node-nvidia`, `node-amd` | Independent databases and listeners on internal port 8444 |
| `farmer-cpu` | CPU PoS2 recovery; local connection to its node |
| `farmer-nvidia` | Optional `nvidia` profile; native CUDA recovery |
| `farmer-amd` | Optional `amd` profile; Vulkan recovery |
| `timelord` | Regular CPU scheduler connected to `node-cpu` |
| `prepare-plots` | Opt-in sequential plot preparation |
| `check-stack` | Opt-in block-production acceptance check |

No host ports are published. The bridge is internal. Nodes register with the introducer; there is no manual full-node peer list. Farmers and the timelord share their assigned node's network namespace, not its filesystem or private CA signing key.

## NVIDIA and AMD

Build the ordinary image first, then the optional CUDA image. Its Dockerfile pins the CUDA base, cuda-oxide revision and Rust nightly; compilation does not need a GPU.

```sh
export DGX_CUDA_ARCH=sm_86
export DGX_NVIDIA_DEVICE=0
docker compose --profile nvidia build farmer-nvidia
docker compose --profile nvidia up -d
```

For AMD, select the render node and its host group:

```sh
export DGX_AMD_RENDER_NODE=/dev/dri/renderD128
export DGX_RENDER_GID="$(stat -c %g "$DGX_AMD_RENDER_NODE")"
docker compose --profile amd up -d
```

Use both profiles when both GPUs are available:

```sh
docker compose --profile nvidia --profile amd up -d
docker compose run --rm --no-deps check-stack --require-farmer cpu,nvidia,amd
```

Logs distinguish loaded plots, eligible qualities, recovered proofs and errors. Only chain acceptance establishes accepted blocks. Explicit CUDA/Vulkan selections do not silently use CPU hashing. Additional cards need separate farmers, nodes, device mappings and identities; scaling a service would share its device and identity.

`DGX_PLOTS_ROOT` supplies the common preparation directory. `DGX_CPU_PLOTS`, `DGX_NVIDIA_PLOTS`, and `DGX_AMD_PLOTS` can override individual read-only farming mounts, but those locations must contain matching plots for the generated identities and testnet hash domain.

## Restart and inspect

After a successful check, stop and restart without deleting volumes. Require a height beyond the previously reported result:

```sh
docker compose --profile nvidia --profile amd down
docker compose --profile nvidia --profile amd up -d
docker compose run --rm --no-deps check-stack --min-height 10 --require-farmer cpu,nvidia,amd
```

Choose a height beyond the previous result, not always 10, and use only available GPU profiles. This checks renewed advancement and common history; it does not force a competing-branch reorganization.

```sh
docker compose exec farmer-cpu curl --fail --silent \
  --cacert /service/ssl/ca/private_ca.crt \
  --cert /service/ssl/farmer/private_farmer.crt \
  --key /service/ssl/farmer/private_farmer.key \
  -H 'Content-Type: application/json' -d '{}' \
  https://localhost:8444/get_node_details
```

`docker compose down` preserves identities and databases. `down --volumes` deliberately destroys them and is not an automatic repair step. New identities need new matching plots; host plots are not removed by Compose. Prefer a new project name and a new plots directory for another independent run.

[Repository overview](../readme.md) · [Farmer](../farmer/README.md) · [Timelord](../timelord/README.md) · [Plotter](../plotter/README.md)
