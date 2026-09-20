# dg-xch-fuzz

Fuzz targets for CLVM parsing, execution, and serialization round trips.

## Status

This package is excluded from normal workspace builds. It requires a nightly Rust toolchain and cargo-fuzz. It does not start a node or validate a network deployment.

## Run

From this directory, with the toolchain and dependencies already installed:

```sh
cargo +nightly fuzz run parse_program -- -max_total_time=60 -rss_limit_mb=1024
cargo +nightly fuzz run run_program -- -max_total_time=60 -rss_limit_mb=1024
cargo +nightly fuzz run roundtrip -- -max_total_time=60 -rss_limit_mb=1024
```

Run in an isolated environment with no credentials or network access and explicit process/disk limits. Preserve a minimal failing input as a deterministic regression test after review. Fuzzing is resource-intensive and belongs after ordinary checks, not in every edit cycle.

[Repository overview](../readme.md) · [Core](../core/README.md)
