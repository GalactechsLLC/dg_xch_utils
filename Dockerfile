# dg-xch-node — a Chia full node (Galactechs dg_xch stack, spec 031).
# Base images are docker.io/library (routed through the in-cluster mirror).
FROM rust:1.96-bookworm AS builder
# C deps: blst (BLS), aws-lc-rs (sqlx rustls TLS) needs cmake, bundled sqlite + zstd need cc.
RUN apt-get update && apt-get install -y --no-install-recommends cmake m4 pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
# Backend profile: default is the embedded SQLite tier + coin-index/hint services.
# The pg image passes FEATURES=postgres (implies coin-index + hint + the sqlx-Postgres store).
ARG FEATURES="sqlite,coin-index,hint"
RUN cargo build --release -p dg_xch_cli --bin dg --features "$FEATURES"
# Symbols stay: the node serves /debug/flamegraph and a stripped binary makes it unreadable.

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 1000 -m node && mkdir -p /data && chown node:node /data
COPY --from=builder /build/target/release/dg /usr/local/bin/dg
USER 1000
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/dg", "full-node"]
