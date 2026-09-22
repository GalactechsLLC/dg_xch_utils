# dg_xch_serialize

Chia wire encoding, protocol versions, and bounded decoding helpers.

## Status

Serialization validates representation, not authorization or consensus. Keep peer-supplied lengths bounded before allocating and validate the decoded values in the owning service.

## Usage

Implement or derive `ChiaSerialize`, select an explicit `ChiaProtocolVersion`, and encode/decode through that trait. Use `parse_vec_limited` when a protocol field needs a collection bound. Network entry points must also bound the enclosing frame and reject trailing bytes when their message contract requires it.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_serialize
cargo doc -p dg_xch_serialize --no-deps
```

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_serialize = { path = "../serialize" }
```

Adjust the path for an external application. Published crate versions may not include this checkout's APIs.

## Validation

```sh
cargo test -p dg_xch_serialize
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../readme.md)
- [core](../core/README.md)
