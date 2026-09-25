# dg_xch_macros

The `ChiaSerial` derive macro for protocol types.

This is a procedural macro, not an executable. Generated parsers share the serialization library's constraints; a successful derive is not proof that an untrusted message is semantically valid.

## Usage

Derive `ChiaSerial` on supported Rust structs and enums alongside `dg_xch_serialize`. Field order and version-dependent fields must match the wire protocol. Use `cargo doc -p dg_xch_serialize --no-deps` for the serialization interface.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_macros = { path = "../macros" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [serialize](../serialize/README.md)
