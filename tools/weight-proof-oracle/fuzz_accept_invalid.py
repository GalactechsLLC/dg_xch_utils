#!/usr/bin/env python3
# Weight-proof validator differential oracle: accept-invalid VDF fuzz.
#
# Mutates the (canonical) form encodings in each base VDF proof to NON-CANONICAL / invalid, then requires
# dg_xch AND chiavdf to reject IDENTICALLY. The critical failure is any mutation dg_xch ACCEPTS that
# chiavdf REJECTS (malleability: a non-canonical encoding sneaking past dg_xch's roundtrip guard while
# chiavdf rejects it). dg_xch verdicts come from the crate's vdf_verify_batch example (stdin JSONL).
#
#   # 1) build the dg_xch batch verifier:
#   cargo build -p dg_xch_weight_proof --example vdf_verify_batch --release
#   # 2) run the fuzz (needs chiavdf importable):
#   python fuzz_accept_invalid.py
#   # or point at a non-default batch binary / seed:
#   python fuzz_accept_invalid.py --dgxch /path/to/vdf_verify_batch --seed 1
import argparse
import json
import os
import random
import subprocess
import sys
from pathlib import Path

from chiavdf import create_discriminant, verify_n_wesolowski

# tools/weight-proof-oracle/<this> -> repo root is two levels up; base vectors live with the crate tests,
# the batch example is built into the workspace target/ dir.
REPO_ROOT = Path(__file__).resolve().parents[2]
ALL13_JSONL = REPO_ROOT / "weight-proof" / "tests" / "fixtures" / "vdf13" / "all13.jsonl"
DEFAULT_DGXCH = REPO_ROOT / "target" / "release" / "examples" / "vdf_verify_batch"

FORM = 100
def chia(c):
    disc = int(create_discriminant(bytes.fromhex(c["challenge"]), c["discriminant_size_bits"]), 16)
    try:
        return verify_n_wesolowski(str(disc), bytes.fromhex(c["x_s_input"]), bytes.fromhex(c["proof"]),
                                   c["num_iterations"], c["discriminant_size_bits"], c["recursion_witness_type"])
    except Exception:
        return False

def mutations(base, rng):
    """Yield non-canonical / corrupted variants of a valid proof."""
    p = bytes.fromhex(base["proof"])
    # (a) flip the BQFC t-sign / b-sign bits of each form header (byte 0 of each 100-byte form) -> non-canonical
    for form_off in range(0, len(p), FORM):
        for bit in (0b01, 0b10):  # B_SIGN, T_SIGN
            m = bytearray(p); m[form_off] ^= bit
            yield dict(base, proof=m.hex())
    # (b) random single-byte flips in the form regions
    for _ in range(24):
        m = bytearray(p); i = rng.randrange(len(p)); m[i] ^= 1 << rng.randrange(8)
        yield dict(base, proof=m.hex())

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dgxch", default=str(DEFAULT_DGXCH), help="path to the vdf_verify_batch example binary")
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    bases = [json.loads(l) for l in open(ALL13_JSONL) if l.strip()]
    rng = random.Random(a.seed)
    cases = []
    for b in bases:
        for i, m in enumerate(mutations(b, rng)):
            m["id"] = f"{bases.index(b)}_{i}"; cases.append(m)
    # dg_xch verdicts (batch)
    inp = "\n".join(json.dumps(c) for c in cases) + "\n"
    res = subprocess.run([a.dgxch], input=inp, capture_output=True, text=True, check=True)
    dg = {json.loads(l)["id"]: json.loads(l)["dg_xch"] for l in res.stdout.splitlines() if l.strip()}
    crit = 0; mism = 0; total = 0
    for c in cases:
        total += 1
        d = dg[c["id"]]; ch = chia(c)
        if d and not ch:
            crit += 1; print(f"CRITICAL accept-invalid: id={c['id']} dg_xch=True chiavdf=False")
        elif d != ch:
            mism += 1; print(f"mismatch (non-critical): id={c['id']} dg_xch={d} chiavdf={ch}")
    print(f"\n{total} mutations | CRITICAL(dg_accept/chia_reject)={crit} | other-mismatch={mism}")
    sys.exit(1 if crit else 0)

if __name__ == "__main__":
    main()
