# dg_xch_pool

A Portfu reference pool with SQLite accounting, pooling v1, and opt-in experimental v2. Run it through `dgx pool`.

This is development software, not ready to hold real pool funds. Reward claims and payouts work in local integration checks, but reorganization recovery, public-service hardening, and long-running validation remain unfinished.

## Install and configure

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx pool --config /home/user/.dgx/config/pool.json
```

Create the configuration first. This mainnet example leaves automatic payouts disabled; replace the target and certificate paths:

```json
{
  "chain": "mainnet",
  "trusted_genesis_header_hash": "d780d22c7a87c9e01d98b49a0910f6701c3b95015741316b3fda042e5d7b81d2",
  "listen": "127.0.0.1:8448",
  "database": "/home/user/.dgx/pool/pool.sqlite",
  "name": "Chia reference pool",
  "description": "Reference implementation for evaluation",
  "target_puzzle_hash": "REPLACE_WITH_POOL_WALLET_PUZZLE_HASH",
  "relative_lock_height": 100,
  "minimum_difficulty": 1,
  "fee_basis_points": 0,
  "authentication_token_timeout": 5,
  "partial_time_limit": 30,
  "max_concurrent_requests": 16,
  "enable_experimental_v2": false,
  "pool_memoization": "80",
  "node_host": "localhost",
  "node_port": 8555,
  "node_tls": {
    "ssl_crt_path": "/absolute/path/private_full_node.crt",
    "ssl_key_path": "/absolute/path/private_full_node.key",
    "ssl_ca_crt_path": "/absolute/path/private_ca.crt"
  },
  "tls": {
    "domain": "localhost",
    "certificate": "/absolute/path/pool.crt",
    "private_key": "/absolute/path/pool.key"
  },
  "payouts": null
}
```

Create the private database directory before starting. The node must be synchronized and match the trusted genesis **header hash**. Port 8555 is the usual Chia reference RPC port; a local `dgx` node defaults to 8444.

Farmers require HTTPS. For a private CA, add its certificate explicitly to the farmer's `pool_ca_certificates`; do not disable certificate verification.

## Rewards and payouts

To enable the reward worker, replace `payouts: null` with:

```json
{"key_file": "/path/to/pool-payout-key.hex", "confirmations": 6, "transaction_fee": 0}
```

The file holds a 32-byte standard-wallet secret key in hex whose puzzle hash matches `target_puzzle_hash`, not a mnemonic or raw master key. Use owner-only permissions (`0600` on Unix).

Accounting uses integer mojos. Rewards are divided by unpaid accepted points at the recorded claim cutoff, after explicit fees; this is neither PPS nor PPLNS. Signed payouts are journaled before broadcast. Back up the database and key, and do not change chain/target/fee settings on an existing database.

The worker currently bounds scans to 1,000 farmers and 1,000 reward records. There is no archival or administrative recovery command.

## APIs and protocol versions

- V1: `GET /pool_info`, `GET/POST/PUT /farmer`, and `POST /partial`.
- V2: equivalent routes under `/v2`, plus `GET /v2/auth`; enable with `enable_experimental_v2: true`.
- `GET /pool_stats`: registration, partial, claim, and payout counters.

Both versions verify membership, proof validity, difficulty, and freshness. Duplicate partials do not earn points twice. Registration and setting changes are explicit authenticated operations.

V2 uses different PlotNFT puzzles and synthetic-key authentication; v1 plots and credentials cannot simply be relabeled. It follows pinned [CHIP-0059](https://github.com/Chia-Network/chips/blob/3f7a2cc8b74c037e8d5fcce2fb9ff721b692c54e/CHIPs/chip-0059.md), [pool2-reference](https://github.com/Chia-Network/pool2-reference/tree/acc8803abf75e942d8f37157fd10c2b4988072f6), and [Chia puzzle vectors](https://github.com/Chia-Network/chia-blockchain/tree/23d9f9d282ffb0220f04a74cade227b42dab97d6), not a finalized compatibility promise.

[Farmer](../farmer/README.md) · [Puzzle helpers](../puzzles/README.md) · [Integration stack](../docker/README.md)
