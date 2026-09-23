# dg-xch-fuzz

Developer-only fuzz targets for CLVM parsing, execution, and serialization. No node or wallet is needed.

## Install and run

Install the harness, then run from this directory with a nightly Rust toolchain:

```sh
cargo install cargo-fuzz --locked
cargo +nightly fuzz run parse_program -- -max_total_time=60 -rss_limit_mb=1024
cargo +nightly fuzz run run_program -- -max_total_time=60 -rss_limit_mb=1024
cargo +nightly fuzz run roundtrip -- -max_total_time=60 -rss_limit_mb=1024
```

This package is outside normal workspace builds. Run it without credentials or network access, with disk/process limits, and retain reviewed minimal failures as regression fixtures.

[Core library](../core/README.md) · [Repository overview](../readme.md)
