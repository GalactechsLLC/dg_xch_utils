# dg_xch_timelord

Native VDF proof workers and an authenticated compact-proof service for dg_xch chains.

## Status

The worker generates real Wesolowski proofs with `dg_xch_vdf` and verifies them before returning them. The compact service connects to a full node, checks its TLS identity and chain handshake, handles `RequestCompactProofOfTime`, and returns matching normalized proofs. CPU work runs in a separate child process so deadlines, shutdown, disconnects, and superseded generations can stop it rather than leave detached work running.

**This is not a complete regular timelord.** It does not emit signage points, infusion points, or end-of-slot bundles. It cannot start a chain or advance an empty database. Full regular operation still needs the challenge/reward/infused-chain scheduler, slot and deficit transitions, overflow handling, epoch changes, authenticated genesis readiness, and live-chain validation. No synthetic proofs or reduced production consensus parameters are used to hide that gap.

## Usage

The [Docker development stack](../docker/README.md) provides a configured compact service and isolated full nodes. To run separately:

```sh
cargo run -p dg_xch_timelord -- compact --config /path/to/timelord.json
```

The JSON configuration has these fields:

| Field | Meaning |
| --- | --- |
| `chain` | Complete chain definition, identical to the full node's chain file. |
| `fullnode_host`, `fullnode_port` | Full-node TCP destination. |
| `server_name` | TLS server identity to verify, normally `localhost` for a local node. |
| `tls.certificate`, `tls.private_key` | Client identity accepted by the node's peer endpoint. |
| `tls.ca_certificate` | CA certificate that signed the node's server certificate. |
| `max_iterations` | Per-request work ceiling, from 1 through 67,108,864. Larger requests are skipped, not shortened. |
| `job_timeout_seconds` | Worker deadline, from 1 through 3,600 seconds. |
| `reconnect_seconds` | Reconnection delay, from 1 through 300 seconds. |

The compact service runs one worker at a time with no unbounded queue. A conservative starting configuration uses 1,048,576 iterations, a 300-second deadline, and a 5-second reconnect delay. Larger Chia proofs may exceed that limit; native proving throughput has not been established for production operation.

The full node must enable `--uncompact` to request compact proofs. It accepts timelord connections from loopback or explicitly trusted networks only. Prefer a local node; do not expose timelord access through broad trusted CIDRs. The public Chia client CA is not secret and is not proof of administrative authority.

For one isolated proof, prepare a request JSON containing `generation`, a 32-byte hex `challenge`, `input` with a 100-byte hex `data` field, `iterations`, and `discriminant_bits`, then run:

```sh
cargo run -p dg_xch_timelord -- prove \
  --request /path/to/vdf-request.json --timeout-seconds 300
```

The identity input has byte `08` followed by 99 zero bytes. The command writes the VDF info and proof as JSON to stdout. It does not submit them to a node. The hidden `worker` command is the subprocess protocol, not a network API.

## Development

```sh
cargo test -p dg_xch_timelord
```

Small tests check chain-derived genesis parameters, a real low-cost proof and verifier agreement, and work-limit rejection. Production-size proving and a real two-node block-production test remain necessary before regular timelord operation can be considered ready.
