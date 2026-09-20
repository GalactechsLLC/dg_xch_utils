# dg_xch_pos1

Native PoS1 verification, plot reading, decompression, and plotting primitives.

## Status

The original v1 plotting pipeline is not a finished standalone plotter; later plotting phases remain incomplete. Plot parsers and compressed-plot paths still need continued malformed-input testing. This package does not contain PoS2 algorithms.

## Usage

Use `verifier` for proof checks and `plots::disk_plot` / `plots::plot_reader` for supported files. The parent `dg_xch_pos` facade preserves older imports. The integrated farmer uses these readers.

This package has no standalone service binary. From the repository root:

```sh
cargo check -p dg_xch_pos1
cargo doc -p dg_xch_pos1 --no-deps
```

Use a workspace/path dependency when developing against this checkout. Published crate versions may not include the current changes.

## Validation

```sh
cargo test -p dg_xch_pos1
```

Tests that require external fixtures, a GPU, or a service need their documented prerequisites; compiling is not an end-to-end network test.

## Related packages

- [Repository overview](../../readme.md)
- [farmer](../../farmer/README.md)

