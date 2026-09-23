# dg_xch_serialize

Chia wire encoding, protocol versions, and bounded decoding helpers.

Serialization validates representation, not authorization or consensus. Keep peer-supplied lengths bounded before allocating and validate the decoded values in the owning service.

## Usage

Implement or derive `ChiaSerialize`, select an explicit `ChiaProtocolVersion`, and encode/decode through that trait. Use `parse_vec_limited` when a protocol field needs a collection bound. Network entry points must also bound the enclosing frame and reject trailing bytes when their message contract requires it.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_serialize = { path = "../serialize" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../cli/README.md) to run services.

## Related packages

- [Repository overview](../readme.md)
- [core](../core/README.md)
