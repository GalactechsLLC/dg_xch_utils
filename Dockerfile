FROM rust:1.98-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends cmake m4 pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
ARG FEATURES="sqlite,coin-index,hint"
RUN cargo build --locked --release -p dg_xch_cli --bin dg --features "$FEATURES" \
    && cargo build --locked --release -p dg_xch_farmer --bins -p dg_xch_timelord -p dg_xch_introducer --features dg_xch_farmer/vulkan \
    && cargo build --locked --release -p dg_xch_plotter --features vulkan \
    && cargo build --locked --release -p dg_xch_dev_tools --bin dg_xch_stack_init --bin dg_xch_stack_plot --bin dg_xch_stack_check --features vulkan

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl libvulkan1 mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 1000 -m node \
    && install -d -o node -g node /data /service /plots /common \
       /stack/common /stack/node-cpu /stack/node-nvidia /stack/node-amd \
       /stack/farmer-cpu /stack/farmer-nvidia /stack/farmer-amd /stack/introducer /stack/timelord
COPY --from=builder /build/target/release/dg /usr/local/bin/dg
COPY --from=builder /build/target/release/dg_xch_farmer /usr/local/bin/dg_xch_farmer
COPY --from=builder /build/target/release/dg_xch_stack_init /usr/local/bin/dg_xch_stack_init
COPY --from=builder /build/target/release/dg_xch_stack_plot /usr/local/bin/dg_xch_stack_plot
COPY --from=builder /build/target/release/dg_xch_stack_check /usr/local/bin/dg_xch_stack_check
COPY --from=builder /build/target/release/dg_xch_timelord /usr/local/bin/dg_xch_timelord
COPY --from=builder /build/target/release/dg_xch_introducer /usr/local/bin/dg_xch_introducer
COPY --from=builder /build/target/release/dg_xch_plotter /usr/local/bin/dg_xch_plotter
USER 1000
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/dg", "full-node"]
