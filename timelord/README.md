# dg_xch_timelord

CPU VDF proof workers and authenticated regular and compact timelord services.

## Status

The regular service schedules challenge-chain, reward-chain, and infused-challenge-chain work. It emits signage points, infusion points, and end-of-slot bundles, handles overflow candidates and epoch changes, and replaces stale work when its full node announces a new peak. New chains start only after a matching full node explicitly authorizes genesis work; public Chia networks never bootstrap an invented genesis.

The worker generates real Wesolowski proofs with `dg_xch_vdf`. Proofs are checked against their exact challenge, input, and iteration count before submission. CPU work runs in child processes so deadlines, shutdown, disconnects, reorgs, and earlier infusion requests can stop superseded computation. Regular work has no compact-mode iteration ceiling and uses memory-budgeted checkpoints. At most three chain proofs run concurrently; identical genesis challenge/reward work is shared.

This is a correctness-first CPU implementation, not a production-speed timelord. Successive signage proofs currently recompute from the last peak or slot boundary rather than sharing a continuous squaring pipeline. The `VdfBackend` interface separates scheduling from proof generation for a future hardware adapter, but no ASIC or `hw_vdf_client` backend is implemented. Live-chain compatibility and full multi-node production require integration validation; source implementation alone is not evidence that those tests passed.

The compact service remains available separately. It handles `RequestCompactProofOfTime` and returns matching normalized proofs, with one worker and a bounded request size.

## Usage

The [Docker development stack](../docker/README.md) supplies a separate development chain and isolated full nodes. To run a regular timelord separately:

```sh
cargo run --release -p dg_xch_timelord -- run --config /path/to/timelord.json
```

The JSON configuration has these fields:

| Field | Meaning |
| --- | --- |
| `chain` | `"mainnet"` by default, another supported Chia network name, `"dgx"`, or a custom chain-definition object identical to the node's definition. |
| `fullnode_host`, `fullnode_port` | Full-node TCP destination. |
| `server_name` | TLS server identity to verify, normally `localhost` for a local node. |
| `tls.certificate`, `tls.private_key` | Client identity accepted by the node's peer endpoint. |
| `tls.ca_certificate` | CA certificate that signed the node's server certificate. |
| `max_iterations` | Compact-mode request ceiling, from 1 through 67,108,864. Regular mode uses the chain's complete iteration counts instead. |
| `job_timeout_seconds` | Per-proof worker deadline, from 1 through 86,400 seconds. |
| `reconnect_seconds` | Reconnection delay, from 1 through 300 seconds. |
| `worker_memory_bytes` | Per-worker checkpoint/workspace budget, 128 KiB through 8 GiB; defaults to 64 MiB. Process/runtime overhead is additional. |
| `max_iterations_per_second` | Optional positive local pacing limit. Omit or use `null` for unrestricted output. This delays real SP/IP/EOS proofs; it does not alter consensus or shorten VDFs. |

Regular mode waits for a node-confirmed peak after submitting an infusion. If confirmation does not arrive, it reconnects for authoritative state instead of inventing a local block. An idle connection without authorized genesis work or an existing peak also reconnects. A low-iteration development chain can use pacing to leave CPU farmers time to recover proofs; production defaults do not impose this limit.

For compact proofs, use the same configuration with:

```sh
cargo run --release -p dg_xch_timelord -- compact --config /path/to/timelord.json
```

A compact-only starting configuration can use `max_iterations` of 1,048,576, a 300-second deadline, and a 5-second reconnect delay. Requests beyond its ceiling are skipped, not shortened. Larger Chia proofs may exceed that limit.

The full node must enable `--uncompact` to request compact proofs; regular operation does not require it. It accepts timelord connections from loopback or explicitly trusted networks only. Prefer a local node; do not expose timelord access through broad trusted CIDRs. The public Chia client CA is not secret and is not proof of administrative authority. The node remains responsible for consensus acceptance, including activation rules when a reconnecting timelord lacks transaction-history context.

For one isolated proof, prepare a request JSON containing `generation`, a 32-byte hex `challenge`, `input` with a 100-byte hex `data` field, `iterations`, and `discriminant_bits`, then run:

```sh
cargo run -p dg_xch_timelord -- prove \
  --request /path/to/vdf-request.json --timeout-seconds 300
```

The identity input has byte `08` followed by 99 zero bytes. The command writes the VDF info and proof as JSON to stdout. It does not submit them to a node. The hidden `worker` and `regular-worker` commands are subprocess protocols, not network APIs.

## Development

```sh
cargo test -p dg_xch_timelord
```

Targeted tests cover real bounded-memory proofs, explicit genesis authorization, SP/IP/EOS proof lengths, infused-chain transitions, overflow and epoch handling, stale generations, pacing, and invalid backend output. Production-sized throughput, long reorg/epoch sequences, and a complete multi-node block-production run remain separate validation requirements.
