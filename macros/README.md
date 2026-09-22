# dg_xch_macros

The `ChiaSerial` derive macro for protocol types.

## Status

This is a procedural macro, not an executable. Generated parsers share the serialization library's constraints; a successful derive is not proof that an untrusted message is semantically valid.

## Usage

Derive `ChiaSerial` on supported Rust structs and enums alongside `dg_xch_serialize`. Field order and version-dependent fields must match the wire protocol. Use `cargo doc -p dg_xch_serialize --no-deps` for the serialization interface.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_macros
cargo doc -p dg_xch_macros --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_macros = { path = "../macros" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_macros
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [serialize](../serialize/README.md)
