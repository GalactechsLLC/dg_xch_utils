# dg_xch_timelord

CPU VDF workers for regular and compact Chia timelord services. This is a correctness-first implementation, not a mainnet-speed timelord. ASIC and GPU backends are not implemented.

## Install and run

From the repository root on Linux or macOS:

```sh
cargo install --path cli --locked
dgx init
dgx timelord run --config /home/user/.dgx/config/timelord.json
```

Create the JSON configuration using a client identity accepted by your node:

```json
{
  "chain": "mainnet",
  "fullnode_host": "localhost",
  "fullnode_port": 8444,
  "server_name": "localhost",
  "tls": {
    "certificate": "/path/to/public_timelord.crt",
    "private_key": "/path/to/public_timelord.key",
    "ca_certificate": "/path/to/node-server-ca.crt"
  },
  "max_iterations": 67108864,
  "job_timeout_seconds": 300,
  "reconnect_seconds": 5,
  "worker_memory_bytes": 67108864
}
```

The CA and server name must match the node's server certificate. Keep timelord access local or narrowly trusted on the node.

For compact proofs, use the same file:

```sh
dgx timelord compact --config /home/user/.dgx/config/timelord.json
```

The node must enable `--uncompact` to request compact proofs. `max_iterations` limits compact requests only; requests above it are skipped. Regular mode uses the chain's full iteration counts and can need longer worker deadlines.

## Integration

The regular scheduler handles signage points, infusion points, and end-of-slot proofs. Child-process workers allow stale work to be stopped after a new peak, disconnect, or deadline. Results are verified before submission.

`VdfBackend` separates scheduling from proving for future hardware support. Current CPU workers recompute successive signage proofs rather than maintaining a continuous squaring pipeline. Worker memory budgets exclude process overhead.

For a standalone proof request, use `dgx timelord prove --help`. The hidden worker commands are internal subprocess protocols.

[VDF primitives](../vdf/README.md) · [Full node](../full-node/README.md)
