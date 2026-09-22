# dg_parser_macro

Compile-time parsing of embedded CLVM program hex.

## Status

This is a procedural macro, not a runtime parser service. Invalid embedded input is a build error. Runtime CLVM validation is in `dg_xch_core`.

## Usage

Use `parse_program_hex!` where an embedded program must be parsed during compilation. Prefer checked-in, reviewed puzzle bytes and compare their tree hashes in tests.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_parser_macro
cargo doc -p dg_parser_macro --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_parser_macro = { path = "../parser_macro" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_parser_macro
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [puzzles](../puzzles/README.md)
