# Weight-proof differential oracle (optional, removable)

**This directory is optional. Deleting it does not affect the build or any test.**
`rm -rf tools/weight-proof-oracle/` and the `dg_xch_weight_proof` crate still compiles and every test still passes. Nothing in the Rust sources or tests imports, shells out to, or otherwise depends on anything here.

## What this is
These Python scripts are the reference oracle used to build and re-verify the golden test vectors the Rust weight-proof tests check against. They run chia's own implementation (`chiavdf`, and chia's `weight_proof.py`) over the exact bytes the Rust validator consumes, and emit the golden files committed under `weight-proof/tests/fixtures/`. They are the "verifier" half of a writer-not-equal-verifier differential gate: the Rust port is the writer, chia's reference (via these scripts) is the independent verifier. This is how the two `dg_xch_vdf` bugs fixed in this work and the 23,579-entry sub-epoch-summary chain were caught. They call chia's published packages; no chia source is copied here.

## Why the Rust tests don't need them
The Rust tests are self-contained: they read frozen golden fixtures under `weight-proof/tests/fixtures/` (the `.golden.json`, `.golden.hashes.txt`, the `.bin`, and the VDF case JSONs) — committed data these scripts produced once. The tests never invoke Python, so CI needs no Python, no chia, no venv. These scripts are needed only to regenerate the goldens if a fixture or the reference ever changes.

## Files
| Script | Purpose | Produces / verifies |
|---|---|---|
| `wp_reference.py` | Runs chia's weight-proof validation over the mainnet proof; emits the golden ses-hash chain + metadata | `weight_proof_mainnet_9054698.golden.json`, `.golden.hashes.txt` |
| `vdf_differential_confirm.py` | Confirms one VDF differential (chiavdf True vs pre-fix dg_xch False on identical bytes) | verifies `vdf_differential_case.json` |
| `confirm_all.py` | Confirms all 13 VDF differentials | verifies `vdf13/*_case_*.json` |
| `fuzz_accept_invalid.py` | Accept-invalid fuzz: mutates canonical forms to non-canonical, requires dg_xch and chiavdf to reject identically | drives the `vdf_verify_batch` example |

Each script resolves the fixtures (which stay under `weight-proof/tests/fixtures/`) relative to the repo root from its own location, so it can be run from anywhere. All commands below are shown from the repo root.

## Environment (only to regenerate goldens)
```
python3.12 -m venv .venv
.venv/bin/pip install chiavdf chiapos bitstring sortedcontainers chiabip158 aiohttp click
# chia (2.7.x) importable on PYTHONPATH for wp_reference.py
```

Pin the genesis challenge: chia's in-code `DEFAULT_CONSTANTS.GENESIS_CHALLENGE` is the `sha256("")` placeholder, not mainnet's `ccd5bb71...`. Validating a real mainnet proof against the placeholder silently rejects (reconstructs the ses-hash chain from the wrong seed). Pin `GENESIS_CHALLENGE` to the target chain, exactly as the Rust validator does. `wp_reference.py` defaults `--genesis` to mainnet's `ccd5bb71...`.

## Regenerate the goldens
```
.venv/bin/python tools/weight-proof-oracle/wp_reference.py \
    weight-proof/tests/fixtures/weight_proof_mainnet_9054698.bin \
    --out weight-proof/tests/fixtures/weight_proof_mainnet_9054698.golden.json
# writes the .golden.json and a sibling .golden.hashes.txt (one ses hash per line)
# use --genesis <hex> for a non-mainnet chain
```

## Re-run the VDF differential checks
```
# one captured VDF differential (defaults to the committed case JSON):
.venv/bin/python tools/weight-proof-oracle/vdf_differential_confirm.py

# all 13 captured VDF differentials:
.venv/bin/python tools/weight-proof-oracle/confirm_all.py

# accept-invalid fuzz — first build the batch verifier, then run:
cargo build -p dg_xch_weight_proof --example vdf_verify_batch --release
.venv/bin/python tools/weight-proof-oracle/fuzz_accept_invalid.py
# --dgxch <path> overrides the batch binary; --seed <n> changes the mutation seed
```

## To remove the Python entirely
```
rm -rf tools/weight-proof-oracle/
```
That's it. The golden fixtures remain under `weight-proof/tests/fixtures/` and the Rust test suite stays green. You lose only the ability to regenerate those goldens from chia's reference.
