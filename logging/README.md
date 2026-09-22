# dg_logger

Logging used by the command-line applications and services.

## Status

Application logs may contain operational data. Do not include mnemonics, private keys, passwords, transaction authorization, or full secret-bearing configuration objects.

## Usage

Create a `DruidGardenLoggerBuilder` and configure the logger before starting worker tasks. The `color` feature is enabled by default; disable default features for plain output.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_logger
cargo doc -p dg_logger --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_logger = { path = "../logging" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_logger
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [cli](../cli/README.md)
