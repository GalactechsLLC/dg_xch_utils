# dg_parser_macro

Compile-time parsing of embedded CLVM program hex.

This is a procedural macro, not a runtime parser service. Invalid embedded input is a build error. Runtime CLVM validation is in `dg_xch_core`.

## Usage

Use `parse_program_hex!` where an embedded program must be parsed during compilation. Prefer checked-in, reviewed puzzle bytes and compare their tree hashes in tests.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_parser_macro = { path = "../parser_macro" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [puzzles](../puzzles/README.md)
