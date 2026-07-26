#!/usr/bin/env python3
# Weight-proof validator differential oracle: confirm one VDF differential.
#
# Runs chia's OWN chiavdf on the EXACT bytes dg_xch_vdf::verify_vdf receives internally at a failing VDF
# (captured at the verify_vdf boundary, incl. dg_xch's derived discriminant). Proves:
#   * dg_xch's discriminant == chiavdf's discriminant for the same challenge  (no derivation diff)
#   * chiavdf.verify_n_wesolowski(identical bytes) == True                    (the proof is valid)
#   * dg_xch_vdf::verify_vdf(identical bytes) == False                        (the pre-fix bug)
# => localizes to dg_xch_vdf's class-group arithmetic (square/multiply/get_b), not plumbing.
#
# The case JSON stays with the Rust test fixtures; read from there by default (repo-root relative):
#   python vdf_differential_confirm.py
#   python vdf_differential_confirm.py /path/to/other_case.json
import json
import sys
from pathlib import Path

from chiavdf import create_discriminant, verify_n_wesolowski

# tools/weight-proof-oracle/<this> -> repo root is two levels up; the case JSON lives with the crate tests.
REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CASE = REPO_ROOT / "weight-proof" / "tests" / "fixtures" / "vdf_differential_case.json"

case = json.load(open(sys.argv[1] if len(sys.argv) > 1 else DEFAULT_CASE))
challenge = bytes.fromhex(case["challenge"])
x_s       = bytes.fromhex(case["x_s_input"])          # input_el.data (100 bytes)
proof     = bytes.fromhex(case["proof"])              # info.output.data(100) || witness (n-weso)
iters     = case["num_iterations"]
wt        = case["recursion_witness_type"]
dbits     = case["discriminant_size_bits"]

chia_disc = int(create_discriminant(challenge, dbits), 16)
dg_disc   = int(case["dg_xch_discriminant_hex"], 16)
print("discriminant identical (dg_xch vs chiavdf):", chia_disc == dg_disc)
print("chiavdf verify_n_wesolowski:", verify_n_wesolowski(str(chia_disc), x_s, proof, iters, dbits, wt))
print("dg_xch verify_vdf (captured):", case["dg_xch_verify_vdf"])
print(f"params: x_s={len(x_s)}B proof={len(proof)}B iters={iters} witness_type={wt} disc_bits={dbits}")
