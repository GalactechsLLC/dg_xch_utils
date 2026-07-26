#!/usr/bin/env python3
# Weight-proof validator differential oracle: confirm the 13 captured VDF differentials.
#
# Runs chia's chiavdf on the EXACT bytes dg_xch_vdf::verify_vdf receives internally (captured at the
# verify_vdf boundary). For every case chiavdf returns True while the pre-fix dg_xch returned False, so
# all 13 are genuine differentials on identical bytes — localizing the bugs to dg_xch's class-group
# arithmetic rather than a derivation/plumbing difference. These pinned the two dg_xch_vdf fixes.
#
# The case JSONs stay with the Rust test fixtures; this reads them from there (repo-root relative), so it
# can be run from anywhere:
#   python confirm_all.py
import glob
import json
import os
from pathlib import Path

from chiavdf import create_discriminant, verify_n_wesolowski

# tools/weight-proof-oracle/<this> -> repo root is two levels up; the data lives under the crate's tests.
REPO_ROOT = Path(__file__).resolve().parents[2]
VDF13_DIR = REPO_ROOT / "weight-proof" / "tests" / "fixtures" / "vdf13"

for path in sorted(glob.glob(os.path.join(VDF13_DIR, "*_case_*.json"))):
    c = json.load(open(path))
    ch = bytes.fromhex(c["challenge"]); xs = bytes.fromhex(c["x_s_input"]); pf = bytes.fromhex(c["proof"])
    it = c["num_iterations"]; wt = c["recursion_witness_type"]; db = c["discriminant_size_bits"]
    disc = int(create_discriminant(ch, db), 16)
    assert disc == int(c["dg_xch_discriminant_hex"], 16), "discriminant divergence"  # rules out derivation diff
    ok = verify_n_wesolowski(str(disc), xs, pf, it, db, wt)
    print(f"{os.path.basename(path):28} stage={c['stage']:11} wt={wt} chiavdf={ok} dg_xch={c['dg_xch_verify_vdf']}")
