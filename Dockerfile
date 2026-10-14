FROM rust:1.98-trixie AS build-base
RUN apt-get update && apt-get install -y --no-install-recommends cmake m4 pkg-config clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
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

FROM build-base AS builder
RUN apt-get update && apt-get install -y --no-install-recommends glslang-tools \
    && rm -rf /var/lib/apt/lists/*
ARG FEATURES="hint,timelord,vulkan"
RUN cargo build --locked --release -p dg_xch_cli --bin dgx --no-default-features --features "$FEATURES" \
    && cargo build --locked --release -p dg_xch_dev_tools --bin dg_xch_stack_init --bin dg_xch_stack_plot --bin dg_xch_stack_check --bin dg_xch_stack_pool --features vulkan

FROM debian:trixie-slim AS runtime-base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
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

# Preserve the ordinary build and existing Compose behavior.
FROM runtime AS stack
