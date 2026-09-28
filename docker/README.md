# Integration stack

An isolated Compose setup for three nodes, independent CPU/NVIDIA/AMD farmers, an introducer, a CPU timelord, and an optional reference pool.

**This is a disposable local simulation, not a Chia mainnet deployment.** For normal Chia setup, use the [dgx installation guide](../readme.md). Never mount production keys, wallets, or chain databases here.

## Prepare and start

You need Docker Engine with Compose, at least 16 GiB available for sequential plot preparation, and a mounted plot directory writable by container UID 1000. Verify the mount and free space before letting Docker use it.

From the repository root, choose a new project name and an existing scratch location:

```sh
export COMPOSE_PROJECT_NAME=chia-integration
export DGX_PLOTS_ROOT=/path/to/mounted/scratch/plots
mkdir -p "$DGX_PLOTS_ROOT"
docker compose build init-stack
docker compose run --rm init-stack
docker compose run --rm --no-deps prepare-plots
docker compose up -d
docker compose run --rm --no-deps check-stack --min-height 100 --require-farmer cpu
```

Arrange directory ownership for UID 1000 before preparation. The initializer creates test identities and databases; preparation makes matching k28 strength-2 plots. Existing matching plots are retained. Do not reuse a directory containing unrelated files.

Services run through `dgx` inside the image. The separate preparation/check tools are developer utilities. The bridge is internal with no host ports published by default.

The checker requires accepted blocks and matching history, not just healthy containers. Its default timeout is 30 minutes; use `--timeout-seconds` for longer runs.

## Optional GPUs

For NVIDIA, install the host driver and NVIDIA Container Toolkit first. Configure the runtime following [NVIDIA's instructions](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html); any Docker restart interrupts running containers.

```sh
export DGX_CUDA_ARCH=sm_86
export DGX_NVIDIA_DEVICE=0
docker compose --profile nvidia build farmer-nvidia
docker compose --profile nvidia up -d
```

Select the architecture for your card; `sm_86` matches the A4000. The CUDA image currently targets Linux amd64.

For AMD, choose the actual render node and its group:

```sh
export DGX_AMD_RENDER_NODE=/dev/dri/renderD128
export DGX_RENDER_GID="$(stat -c %g "$DGX_AMD_RENDER_NODE")"
docker compose --profile amd up -d
```

Use both profiles when both GPUs are ready. Each farmer connects to its own node. Explicit GPU backends do not silently fall back to CPU.

## Pooling

The `pooling` profile adds the reference pool. CPU uses v1; NVIDIA and AMD use experimental v2. First let the ordinary stack earn at least three mature, unspent CPU-farmer rewards. Leave farming active while the PlotNFT launches confirm.

```sh
docker compose --profile pooling run --rm --no-deps setup-pool prepare
docker compose --profile pooling up -d --no-deps --wait pool
docker compose --profile pooling run --rm --no-deps setup-pool register
```

Keep the same Compose project and its volumes. Use a **new** writable directory for portable plots; old pool-public-key plots cannot be converted:

```sh
export DGX_PLOTS_ROOT=/path/to/mounted/scratch/pool-plots
mkdir -p "$DGX_PLOTS_ROOT"
```

Arrange UID 1000 access, then prepare and switch farmers:

```sh
docker compose --profile prepare run --rm --no-deps prepare-plots --pooling
export DGX_FARMER_CONFIG=pool-config.yaml
docker compose --profile pooling --profile nvidia --profile amd up -d --no-deps pool farmer-cpu farmer-nvidia farmer-amd
docker compose run --rm --no-deps check-stack --pooling \
  --min-height 100 --timeout-seconds 14400 \
  --require-farmer cpu,nvidia,amd
```

Omit unavailable GPU profiles/services and adjust `--require-farmer` accordingly; that checks only the selected farmers. Retain the exported paths and config selection for later restarts. Individual `DGX_CPU_PLOTS`, `DGX_NVIDIA_PLOTS`, and `DGX_AMD_PLOTS` overrides take precedence over `DGX_PLOTS_ROOT`.

A pooling pass requires accepted partials and confirmed on-chain payouts to every selected farmer. The reference worker still lacks automatic reorganization recovery; see [pool limitations](../pool/README.md).

## Stop and inspect

```sh
docker compose ps
docker compose logs --tail=100 node-cpu farmer-cpu timelord
docker compose --profile pooling --profile nvidia --profile amd stop
```

Stopping preserves containers, identities, databases, and plots. Do not remove volumes to troubleshoot a stalled chain. Keep backups of the pool journal and signing key alongside the node data.

[Developer tools](../tools/README.md) · [Farmer](../farmer/README.md) · [Pool](../pool/README.md)
