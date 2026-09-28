# dg_logger

Logging used by the command-line applications and services.

Application logs may contain operational data. Do not include mnemonics, private keys, passwords, transaction authorization, or full secret-bearing configuration objects.

## Usage

Create a `DruidGardenLoggerBuilder` and configure the logger before starting worker tasks. The `color` feature is enabled by default; disable default features for plain output.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_logger = { path = "../logging" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [cli](../cli/README.md)
