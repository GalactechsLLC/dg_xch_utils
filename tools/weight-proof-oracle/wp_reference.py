#!/usr/bin/env python3
"""
Generates reference weight-proof fixtures with Chia's Python validator.

Pass the target chain's genesis challenge explicitly when regenerating fixtures.

ENVIRONMENT (only needed to regenerate goldens):
    python3.12 -m venv .venv
    .venv/bin/pip install bitstring sortedcontainers chiabip158 \
                          aiohttp chiapos chiavdf click
    # chia (2.7.x) importable on PYTHONPATH (or `pip install -e .`). Only the weight_proof import path
    # is needed, not a full node.

USAGE (data lives under the crate's tests; pass explicit paths):
    wp_reference.py weight-proof/tests/fixtures/weight_proof_mainnet_9054698.bin \
        --out weight-proof/tests/fixtures/weight_proof_mainnet_9054698.golden.json
    # default genesis = chia mainnet ccd5bb71...; writes the .golden.json and a sibling .hashes.txt
"""
from __future__ import annotations

import argparse
import json
import sys

MAINNET_GENESIS = "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("bin", help="weight proof .bin (WeightProof streamable bytes)")
    ap.add_argument("--genesis", default=MAINNET_GENESIS, help="target-chain GENESIS_CHALLENGE hex")
    ap.add_argument("--out", default=None, help="golden JSON output path (default: stdout)")
    args = ap.parse_args()

    import random

    from chia.types.weight_proof import WeightProof
    from chia.full_node.weight_proof import (
        _validate_sub_epoch_summaries,
        _map_sub_epoch_summaries,
        _get_weights_for_sampling,
        _sample_sub_epoch,
        WeightProofHandler,
    )
    from chia.consensus.default_constants import DEFAULT_CONSTANTS

    genesis = type(DEFAULT_CONSTANTS.GENESIS_CHALLENGE).fromhex(args.genesis)
    # Pin per target chain — never trust chia's in-code sha256("") placeholder default.
    constants = DEFAULT_CONSTANTS.replace(
        GENESIS_CHALLENGE=genesis, AGG_SIG_ME_ADDITIONAL_DATA=genesis
    )

    data = open(args.bin, "rb").read()
    wp = WeightProof.from_bytes(data)

    # Serialization-fidelity check: chia streamable must round-trip the bytes dg_xch_utils produced.
    reser = bytes(wp)
    round_trip_identical = reser == data

    tip = wp.recent_chain_data[-1]

    # --- Phase 2 oracle: reconstruct + validate the sub-epoch-summary chain ---
    summaries, weight_list = _validate_sub_epoch_summaries(constants, wp)
    phase2_accept = summaries is not None

    ses = []
    if phase2_accept:
        for i, s in enumerate(summaries):
            ses.append(
                {
                    "i": i,
                    "ses_hash": s.get_hash().hex(),
                    "reward_chain_hash": s.reward_chain_hash.hex(),
                    "num_blocks_overflow": int(s.num_blocks_overflow),
                    "new_difficulty": None if s.new_difficulty is None else int(s.new_difficulty),
                    "new_sub_slot_iters": None
                    if s.new_sub_slot_iters is None
                    else int(s.new_sub_slot_iters),
                }
            )

    # --- Phase 1 oracle: the RNG-determined sampled sub-epoch INDEX SET ---
    # Seed = summaries[-2].get_hash() (ref weight_proof.py:587). Reproduce validate_sub_epoch_sampling's
    # set computation and dump the selected indices (the security-critical output the verifier must match).
    phase1 = {"computed": False}
    if phase2_accept:
        seed = summaries[-2].get_hash()
        rng = random.Random(seed)
        tip = wp.recent_chain_data[-1]
        wtc = _get_weights_for_sampling(rng, tip.weight, wp.recent_chain_data)
        sampled = []
        for idx in range(1, len(weight_list)):
            if _sample_sub_epoch(weight_list[idx - 1], weight_list[idx], wtc):
                sampled.append(idx - 1)
                if len(sampled) == WeightProofHandler.MAX_SAMPLES:
                    break
        provided = sorted({s.sub_epoch_n for s in wp.sub_epoch_segments})
        phase1 = {
            "computed": True,
            "seed": seed.hex(),
            "weight_to_check_count": (None if wtc is None else len(wtc)),
            "max_samples": WeightProofHandler.MAX_SAMPLES,
            "sampled_sub_epochs": sampled,
            "provided_segment_sub_epochs": provided,
            "sampled_equals_provided": sampled == provided,
        }

    # --- Phase 3 oracle: accumulated summary weight must equal the on-chain weight at the sub-epoch
    # boundary block (ref _validate_summaries_weight). ---
    phase3 = {"computed": False}
    if phase2_accept:
        _, total_weight, _ = _map_sub_epoch_summaries(
            constants.SUB_EPOCH_BLOCKS, constants.GENESIS_CHALLENGE, wp.sub_epochs, constants.DIFFICULTY_STARTING
        )
        num_over = summaries[-1].num_blocks_overflow
        ses_end_height = (len(summaries) - 1) * constants.SUB_EPOCH_BLOCKS + num_over - 1
        boundary = [b for b in wp.recent_chain_data if b.reward_chain_block.height == ses_end_height]
        phase3 = {
            "computed": True,
            "total_weight": int(total_weight),
            "ses_end_height": int(ses_end_height),
            "boundary_block_found": len(boundary),
            "boundary_weight": (None if not boundary else int(boundary[-1].reward_chain_block.weight)),
        }

    golden = {
        "fixture_bytes": len(data),
        "round_trip_identical": round_trip_identical,
        "genesis_challenge": args.genesis,
        "sub_epochs": len(wp.sub_epochs),
        "segments": len(wp.sub_epoch_segments),
        "recent_blocks": len(wp.recent_chain_data),
        "tip_height": int(tip.height),
        "tip_weight": int(tip.weight),
        "phase2_sub_epoch_summaries": {
            "accept": phase2_accept,
            "count": len(ses),
            "first_ses_hash": ses[0]["ses_hash"] if ses else None,
            "last_ses_hash": ses[-1]["ses_hash"] if ses else None,
            "summary_hash_chain": [e["ses_hash"] for e in ses],
        },
        "phase1_sampling": phase1,
        "phase3_summaries_weight": phase3,
    }

    out = json.dumps(golden, indent=2)
    if args.out:
        with open(args.out, "w") as fh:
            fh.write(out)
        # Also emit a dependency-free flat hash list (one hex ses_hash per line) so the Rust harness
        # can gate the full summary-hash chain with std::fs + hex, no JSON parser in dev-deps.
        hashes_path = args.out.rsplit(".json", 1)[0] + ".hashes.txt"
        with open(hashes_path, "w") as fh:
            fh.write("\n".join(e["ses_hash"] for e in ses))
            if ses:
                fh.write("\n")
        print(
            f"wrote {args.out} + {hashes_path}: phase2_accept={phase2_accept} "
            f"summaries={len(ses)} round_trip={round_trip_identical}",
            file=sys.stderr,
        )
    else:
        print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
