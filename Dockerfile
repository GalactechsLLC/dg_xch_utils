FROM rust:1.99.1-trixie AS build-base
RUN apt-get update && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends cmake m4 pkg-config clang libclang-dev python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .

# Preserve complete source and notices, including the LGPL relinking materials.
FROM build-base AS compliance-builder
RUN python3 docker/package-compliance.py --output /opt/dgx-compliance

# Full-node images build only the CPU CLI, without stack utilities or GPU tooling.
FROM build-base AS full-node-builder
ARG FULL_NODE_FEATURES="hint"
RUN cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features --features "$FULL_NODE_FEATURES"

# Peer discovery services share the CPU-only CLI build.
FROM build-base AS discovery-builder
RUN cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features

# CPU-only private PoS2 network: node, farmer, plotter and regular timelord.
FROM build-base AS pos2-test-builder
RUN cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features --features "hint,timelord"

# Shared CPU-only stack utilities, including the explicit chain-profile checks.
FROM pos2-test-builder AS stack-cpu-builder
RUN cargo build --locked --release -p dg_xch_dev_tools \
    --bin dg_xch_stack_init --bin dg_xch_stack_plot --bin dg_xch_stack_check

FROM build-base AS falcon-builder
# COPY preserves source mtimes; refresh them so cached artifacts never hide edits
# made while a preceding image build was still compiling.
RUN --mount=type=cache,id=dgx-falcon-target,target=/build/target \
    find . -path './target' -prune -o -type f -name '*.rs' -exec touch {} + \
    && cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features --features "hint,timelord,falcon" \
    && cargo build --locked --release -p dg_xch_dev_tools --features falcon \
       --bin dg_xch_stack_init --bin dg_xch_stack_plot --bin dg_xch_stack_check \
    && mkdir -p /build/falcon-bin \
    && cp target/release/dgx target/release/dg_xch_stack_init target/release/dg_xch_stack_plot target/release/dg_xch_stack_check /build/falcon-bin/

FROM build-base AS labrador-builder
# COPY preserves source mtimes; refresh them so cached artifacts never hide edits
# made while a preceding image build was still compiling.
RUN --mount=type=cache,id=dgx-falcon-target,target=/build/target \
    find . -path './target' -prune -o -type f -name '*.rs' -exec touch {} + \
    && cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features --features "hint,timelord,labrador" \
    && cargo build --locked --release -p dg_xch_dev_tools --features labrador \
       --bin dg_xch_stack_init --bin dg_xch_stack_plot --bin dg_xch_stack_check \
    && cargo build --locked --release -p dg_xch_labrador --features backend --bin dg_xch_labrador_worker \
    && mkdir -p /build/labrador-bin \
    && cp target/release/dg_xch_labrador_worker target/release/dgx target/release/dg_xch_stack_init target/release/dg_xch_stack_plot target/release/dg_xch_stack_check /build/labrador-bin/

FROM build-base AS builder
RUN apt-get update && apt-get install -y --no-install-recommends glslang-tools \
    && rm -rf /var/lib/apt/lists/*
ARG FEATURES="hint,timelord,vulkan"
RUN cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features --features "$FEATURES" \
    && cargo build --locked --release -p dg_xch_dev_tools --bin dg_xch_stack_init --bin dg_xch_stack_plot --bin dg_xch_stack_check --bin dg_xch_stack_pool --features vulkan

FROM debian:trixie-20261020-slim AS runtime-base
COPY --from=compliance-builder /opt/dgx-compliance/ /usr/local/share/dgx/
COPY --from=compliance-builder /opt/dgx-compliance/licenses/ /usr/local/share/licenses/dgx/
COPY LICENSE /usr/local/share/licenses/dgx/LICENSE-APACHE-2.0
COPY licenses/ /usr/local/share/licenses/dgx/
COPY labrador/native/LICENSE labrador/native/LICENSE-fdlibm labrador/native/LICENSE-overlay /usr/local/share/licenses/dgx/labrador/
RUN apt-get update && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 1000 -m node \
    && install -d -o node -g node /data /service /plots /common \
       /stack/common /stack/node-cpu /stack/node-nvidia /stack/node-amd \
       /stack/farmer-cpu /stack/farmer-nvidia /stack/farmer-amd /stack/introducer /stack/timelord /stack/pool
USER 1000
WORKDIR /data
ENV DGX_CONFIG_DIR=/data/config \
    DGX_FULL_NODE_DB=sqlite:///data/chain.db \
    DGX_FULL_NODE_SSL_DIR=/data/config/ssl
ENTRYPOINT ["/usr/local/bin/dgx", "full-node"]

# Select a service with docker build --target <service>. Arguments append to that service.
FROM runtime-base AS full-node
COPY --from=full-node-builder /build/target/release/dgx /usr/local/bin/dgx
EXPOSE 8444

FROM runtime-base AS crawler
COPY --from=discovery-builder /build/target/release/dgx /usr/local/bin/dgx
EXPOSE 8580
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8580/health || exit 1
ENTRYPOINT ["/usr/local/bin/dgx", "crawler"]
CMD ["--listen", "0.0.0.0:8580"]

FROM runtime-base AS introducer
COPY --from=discovery-builder /build/target/release/dgx /usr/local/bin/dgx
EXPOSE 8445 8581
ENTRYPOINT ["/usr/local/bin/dgx", "introducer"]

# The shared stack and GPU services retain their Vulkan runtime and developer utilities.
FROM runtime-base AS runtime
USER root
RUN apt-get update && apt-get install -y --no-install-recommends libvulkan1 mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/dgx /usr/local/bin/dgx
COPY --from=builder /build/target/release/dg_xch_stack_init /usr/local/bin/dg_xch_stack_init
COPY --from=builder /build/target/release/dg_xch_stack_plot /usr/local/bin/dg_xch_stack_plot
COPY --from=builder /build/target/release/dg_xch_stack_check /usr/local/bin/dg_xch_stack_check
COPY --from=builder /build/target/release/dg_xch_stack_pool /usr/local/bin/dg_xch_stack_pool
USER 1000

FROM runtime AS farmer
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/dgx", "farmer"]
FROM farmer AS farmer-cpu
FROM farmer AS farmer-amd

FROM runtime AS plotter
ENTRYPOINT ["/usr/local/bin/dgx", "plotter"]
FROM plotter AS plotter-cpu
FROM plotter AS plotter-vulkan
ENV DGX_PLOTTER_BACKEND=vulkan

FROM runtime AS timelord
EXPOSE 8582
ENTRYPOINT ["/usr/local/bin/dgx", "timelord", "run"]
FROM runtime AS timelord-compact
EXPOSE 8583
ENTRYPOINT ["/usr/local/bin/dgx", "timelord", "compact"]

FROM runtime AS pool
EXPOSE 8448
ENTRYPOINT ["/usr/local/bin/dgx", "pool"]

FROM runtime AS init
ENTRYPOINT ["/usr/local/bin/dgx", "init"]
CMD ["--non-interactive", "--data-dir", "/data", "--plots-dir", "/plots"]

FROM runtime AS init-stack
ENTRYPOINT ["/usr/local/bin/dg_xch_stack_init"]
CMD ["--root", "/stack"]
FROM runtime AS prepare-plots
ENTRYPOINT ["/usr/local/bin/dg_xch_stack_plot"]
CMD ["--root", "/stack", "--plots-root", "/plots"]
FROM runtime AS check-stack
ENTRYPOINT ["/usr/local/bin/dg_xch_stack_check"]
CMD ["--root", "/stack"]
FROM runtime AS setup-pool
ENTRYPOINT ["/usr/local/bin/dg_xch_stack_pool"]
CMD ["--root", "/stack", "prepare"]

FROM runtime-base AS pos2-test
COPY --from=pos2-test-builder /build/target/release/dgx /usr/local/bin/dgx
EXPOSE 58444 8080 8582
ENTRYPOINT ["/usr/local/bin/dgx"]

FROM runtime-base AS stack-cpu
COPY --from=stack-cpu-builder /build/target/release/dgx /usr/local/bin/dgx
COPY --from=stack-cpu-builder /build/target/release/dg_xch_stack_init /usr/local/bin/dg_xch_stack_init
COPY --from=stack-cpu-builder /build/target/release/dg_xch_stack_plot /usr/local/bin/dg_xch_stack_plot
COPY --from=stack-cpu-builder /build/target/release/dg_xch_stack_check /usr/local/bin/dg_xch_stack_check

# Explicit CPU-only experimental Falcon network.
FROM runtime-base AS falcon-stack
COPY --from=falcon-builder /build/falcon-bin/dgx /usr/local/bin/dgx
COPY --from=falcon-builder /build/falcon-bin/dg_xch_stack_init /usr/local/bin/dg_xch_stack_init
COPY --from=falcon-builder /build/falcon-bin/dg_xch_stack_plot /usr/local/bin/dg_xch_stack_plot
COPY --from=falcon-builder /build/falcon-bin/dg_xch_stack_check /usr/local/bin/dg_xch_stack_check

FROM runtime-base AS labrador-stack
COPY --from=labrador-builder /build/labrador-bin/ /usr/local/bin/

# Keep the ordinary stack last: Docker uses the final stage when no target is supplied.
FROM runtime AS stack
