# Monitoring a dg_xch Full Node (Prometheus + Grafana)

Every node exposes Prometheus text metrics at `/metrics` on the shared Portfu HTTPS listener
(`--listen`, default `0.0.0.0:8444`).

## The metrics

| Series | Type | Meaning |
|---|---|---|
| `fullnode_peak_height` | gauge | Confirmed local peak height |
| `fullnode_claimed_peak_height` | gauge | Highest peer-announced network tip |
| `fullnode_blocks_downloaded_total` | counter | Bodies fetched by the sync pipeline |
| `fullnode_blocks_confirmed_total` | counter | Blocks validated + confirmed into the store |
| `fullnode_reservations_reclaimed_total` | counter | Reservation windows reclaimed from stalled peers |
| `fullnode_peak_reservation_window` | gauge | Peak in-flight window identifiers (memory-bound proof) |
| `fullnode_peak_inflight_blocks` | gauge | Peak blocks held by the headers-first body downloader; zero when genesis follow sync bypasses it |
| `fullnode_process_resident_bytes` | gauge | Process RSS |
| `fullnode_outbound_peers` / `fullnode_inbound_peers` | gauge | Live peer connections |
| `fullnode_window_vdf_micros` | gauge | Last window's all-core VDF drain wall time |
| `fullnode_window_body_micros` | gauge | Last window's parallel CLVM+BLS precompute wall time |
| `fullnode_window_confirm_micros` | gauge | Last window's batched store confirm wall time |
| `fullnode_follow_fetch_wait_micros_total` | counter | Producer-side network wait; may overlap consumer processing |
| `fullnode_follow_step_micros_total` | counter | Follow consumer-cycle wall time, including queue wait and validation |
| `fullnode_sync_from_height` | gauge | Configured `--sync-from` anchor (0 = genesis node) |
| `fullnode_net_messages_in_total{msg=...}` | counter | Messages received, by protocol type — the gossip-health series |
| `fullnode_net_messages_out_total{msg=...}` | counter | Messages sent, by protocol type |
| `fullnode_net_bytes_in_total` / `_out_total` | counter | Peer-link payload bytes, both directions |
| `fullnode_mempool_size` / `_cost` / `_max_total_cost` | gauge | Mempool residency (chia-exporter aligned) |
| `fullnode_current_signage_point` | gauge | Latest accepted SP index (0-63) — the within-slot heartbeat |
| `fullnode_signage_points_total` | counter | Signage points accepted since startup |
| `fullnode_last_reorg_depth` | gauge | Depth of the most recent reorg (0 = none observed) |

**The headline number — blocks per minute:**

```promql
rate(fullnode_blocks_confirmed_total[5m]) * 60
```

The three `window_*_micros` gauges are the per-backend differentiators: on a healthy
node `window.vdf` dominates in old eras, `window.body` grows in transaction-dense eras,
and `window.confirm` is where a slow store shows up (watch it on the mmap/Pi profile).

## Scraping

**Plain Prometheus** (a Pi, a lone VM, docker-compose):

```yaml
scrape_configs:
  - job_name: dg-xch-node
    static_configs:
      - targets: ["<node-host>:8444"]
    scheme: https
    tls_config:
      insecure_skip_verify: true # or configure ca_file with the Chia network CA
```

The dashboards key on the `job` label — name the job after the node
(for example `dg-xch-node-pg`, `dg-xch-node-mm`) or relabel to match, and the per-node pages
light up unchanged.

**Kubernetes with the Prometheus operator**: each node's
Service must expose the shared server port, and one ServiceMonitor covers the namespace:

```yaml
# on each node Service
ports:
  - { name: server, port: 8444, protocol: TCP }
---
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata: { name: dg-xch-nodes, namespace: <your-ns> }
spec:
  selector: { matchLabels: {} }          # every Service in the namespace
  namespaceSelector: { matchNames: [<your-ns>] }
  endpoints:
    - { port: server, path: /metrics, scheme: https, interval: 15s }
```

With the operator, `job` = the Service name — which is exactly what the dashboards
expect (one job per node).

## The dashboards

Checked in under [`grafana/`](grafana/):

| File | Page |
|---|---|
| `dg-xch-sync-overview.json` | All nodes side by side — blocks/min, heights, tip distance, RSS, peers, reclaim rate |

Load it with a manual import (any Grafana): Dashboards → New → Import → upload
the JSON. The dashboard has a `DS_PROMETHEUS` datasource variable — pick your
Prometheus at import time. On kube-prometheus-stack, wrap the JSON in a
ConfigMap labeled `grafana_dashboard: "1"` in the namespace the sidecar watches
and it appears automatically.

The JSON in `grafana/` is the source of truth — edit there and re-import.

## Sanity check without Grafana

```bash
curl -ks https://localhost:8444/metrics | grep -E 'fullnode_(peak_height|blocks_confirmed_total|window_)'
```

If `fullnode_blocks_confirmed_total` is climbing, the node is syncing; everything
else is presentation.
