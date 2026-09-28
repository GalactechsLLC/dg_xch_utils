# dg_xch_pos1

Native PoS1 verification, plot reading, decompression, and plotting primitives.

The original v1 plotting pipeline is not a finished standalone plotter; later plotting phases remain incomplete. Plot parsers and compressed-plot paths still need continued malformed-input testing. This package does not contain PoS2 algorithms.

## Usage

Use `verifier` for proof checks and `plots::disk_plot` / `plots::plot_reader` for supported files. The parent `dg_xch_pos` facade preserves older imports. The integrated farmer uses these readers.

From another top-level workspace crate, add a local dependency:

```toml
[dependencies]
dg_xch_pos1 = { path = "../proof_of_space/pos1" }
```

Adjust the dependency path for your project. This is a library; install [dgx](../../cli/README.md) to run services.

## Related packages

- [Repository overview](../../readme.md)
- [farmer](../../farmer/README.md)
