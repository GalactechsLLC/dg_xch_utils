use crate::state::Summary;
use prometheus::{
    Encoder, Gauge, GaugeVec, IntCounter, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};
use std::{collections::BTreeMap, io::Error};

pub fn render(s: &Summary) -> Result<String, Error> {
    let registry = Registry::new_custom(
        None,
        Some(std::collections::HashMap::from([
            ("network".into(), s.network.clone()),
            ("scope".into(), "network".into()),
        ])),
    )
    .map_err(Error::other)?;
    for (name, help, value) in [
        (
            "reliable_full_nodes",
            "Full nodes with qualifying peaks and sufficient historical reliability",
            s.reliable as i64,
        ),
        (
            "ignored_endpoints",
            "Endpoints temporarily ignored based on reliability",
            s.ignored as i64,
        ),
        (
            "banned_endpoints",
            "Endpoints suspended based on long-term reliability",
            s.banned as i64,
        ),
        (
            "attempted_endpoints",
            "Retained endpoints attempted by the crawler",
            s.attempted as i64,
        ),
        (
            "attempted_connected_endpoints",
            "Attempted retained endpoints with at least one observed connection",
            s.attempted_connected as i64,
        ),
        (
            "never_attempted_endpoints",
            "Retained endpoints not yet attempted by the crawler",
            s.never_attempted as i64,
        ),
        (
            "latest_failed_endpoints",
            "Retained endpoints whose latest completed probe failed",
            s.failed_latest as i64,
        ),
        (
            "pending_retry_endpoints",
            "Failed endpoints waiting for their retry deadline",
            s.pending_retry as i64,
        ),
        (
            "configured_concurrency",
            "Configured crawl concurrency, shared per network in PostgreSQL mode",
            s.configured_concurrency as i64,
        ),
        (
            "peak_wait_milliseconds",
            "Separate NewPeak observation deadline after peer discovery",
            s.peak_wait_ms as i64,
        ),
        (
            "tcp_connect_timeout_seconds",
            "Configured TCP connection deadline; also bounded by the connection deadline",
            s.tcp_connect_timeout_seconds as i64,
        ),
        (
            "interval_milliseconds",
            "Minimum spacing between crawl attempts",
            s.interval_ms as i64,
        ),
        (
            "ipv6_enabled",
            "One if outbound IPv6 probing is enabled for this crawler network",
            i64::from(s.enable_ipv6),
        ),
        (
            "ipv6_known_endpoints",
            "Known IPv6 endpoints",
            s.ipv6_known as i64,
        ),
        (
            "ipv6_attempted_endpoints",
            "IPv6 endpoints attempted by the crawler",
            s.ipv6_attempted as i64,
        ),
        (
            "known_endpoints",
            "Retained gossiped or seeded IP and port endpoints, not a global node census",
            s.known as i64,
        ),
        (
            "total_nodes_5_days",
            "Distinct IPs with an accepted advertised gossip timestamp within five days; reachability is not required",
            s.total_nodes_5_days as i64,
        ),
        (
            "ipv4_nodes_5_days",
            "Distinct IPv4 addresses with an accepted advertised gossip timestamp within five days",
            s.ipv4_nodes_5_days as i64,
        ),
        (
            "ipv6_nodes_5_days",
            "Distinct IPv6 addresses with an accepted advertised gossip timestamp within five days, including when IPv6 probing is disabled",
            s.ipv6_nodes_5_days as i64,
        ),
        (
            "ever_connected_endpoints",
            "Retained endpoints with at least one valid network handshake",
            s.ever_connected as i64,
        ),
        (
            "recent_endpoints",
            "Endpoints with a valid handshake within the configured recent window",
            s.recent as i64,
        ),
        (
            "recent_node_identities",
            "Distinct TLS certificate SHA256 fingerprints among recent endpoints",
            s.unique_recent_identities as i64,
        ),
        (
            "due_endpoints",
            "Eligible endpoints due for probing, before global concurrency and rate limits",
            s.due as i64,
        ),
        (
            "inflight_probes",
            "Active outbound probes",
            s.inflight as i64,
        ),
        (
            "recent_window_seconds",
            "Window defining recent observations",
            s.recent_window_seconds as i64,
        ),
        (
            "storage_healthy",
            "One if PostgreSQL aggregate queries succeeded",
            i64::from(s.storage_error.is_none()),
        ),
        (
            "bootstrap_healthy",
            "One if the last bootstrap resolution had no errors",
            i64::from(s.seed_error.is_none()),
        ),
    ] {
        let metric = IntGauge::new(format!("chia_crawler_{name}"), help).map_err(Error::other)?;
        metric.set(value);
        registry.register(Box::new(metric)).map_err(Error::other)?;
    }
    let failures = IntGaugeVec::new(
        Opts::new(
            "chia_crawler_latest_failed_phase",
            "Latest retained probe failures by stage",
        ),
        &["phase"],
    )
    .map_err(Error::other)?;
    for phase in [
        "unknown",
        "tcp_connect",
        "tls",
        "websocket",
        "handshake",
        "peer_request",
        "verification",
    ] {
        failures
            .with_label_values(&[phase])
            .set(*s.error_phases.get(phase).unwrap_or(&0) as i64);
    }
    registry
        .register(Box::new(failures))
        .map_err(Error::other)?;
    let t = &s.totals;
    for (name, help, value) in [
        (
            "attempts_total",
            "Outbound probe attempts, persisted across restarts",
            t.attempts,
        ),
        (
            "connections_total",
            "Valid Chia handshakes, persisted across restarts",
            t.connections,
        ),
        (
            "peer_responses_total",
            "Valid correlated peer-list responses",
            t.peer_responses,
        ),
        (
            "failures_total",
            "Probes failing connection, handshake or peer request",
            t.failures,
        ),
        (
            "gossip_received_total",
            "Cumulative entries in accepted peer responses, including duplicates and rejected addresses",
            t.gossip_received,
        ),
        (
            "gossip_rejected_total",
            "Rejected addresses or malformed gossip hosts",
            t.gossip_rejected,
        ),
        ("pruned_total", "Endpoints removed by retention", t.pruned),
    ] {
        let metric = IntCounter::new(format!("chia_crawler_{name}"), help).map_err(Error::other)?;
        metric.inc_by(value);
        registry.register(Box::new(metric)).map_err(Error::other)?;
    }
    for (name, help, value) in [
        (
            "connect_duration_seconds_total",
            "Cumulative time spent establishing successful Chia connections",
            t.connect_seconds_sum,
        ),
        (
            "peer_request_duration_seconds_total",
            "Cumulative time spent on successful peer requests",
            t.request_seconds_sum,
        ),
    ] {
        let metric =
            prometheus::Counter::new(format!("chia_crawler_{name}"), help).map_err(Error::other)?;
        metric.inc_by(value);
        registry.register(Box::new(metric)).map_err(Error::other)?;
    }
    for (name, help, value) in [
        (
            "recent_average_connect_seconds",
            "Mean latest connection time among recently connected endpoints",
            s.average_connect_ms,
        ),
        (
            "recent_average_peer_request_seconds",
            "Mean latest request response time among recent responders; not ICMP ping",
            s.average_peer_request_ms,
        ),
    ] {
        if let Some(value) = value {
            let metric = Gauge::new(format!("chia_crawler_{name}"), help).map_err(Error::other)?;
            metric.set(value / 1000.0);
            registry.register(Box::new(metric)).map_err(Error::other)?;
        }
    }
    let families = GaugeVec::new(
        Opts::new(
            "chia_crawler_recent_address_family",
            "Recent endpoints by IP family",
        ),
        &["family"],
    )
    .map_err(Error::other)?;
    families
        .with_label_values(&["ipv4"])
        .set(s.ipv4_recent as f64);
    families
        .with_label_values(&["ipv6"])
        .set(s.ipv6_recent as f64);
    registry
        .register(Box::new(families))
        .map_err(Error::other)?;
    for (name, help, label, counts) in [
        (
            "chia_crawler_recent_versions",
            "Recent endpoints by reported software version; top 32 and other",
            "version",
            &s.versions,
        ),
        (
            "chia_crawler_recent_protocols",
            "Recent endpoints by reported protocol version; top 32 and other",
            "protocol",
            &s.protocols,
        ),
    ] {
        let metric = GaugeVec::new(Opts::new(name, help), &[label]).map_err(Error::other)?;
        let mut values = bounded_counts(counts, 32);
        let extra = if label == "version" {
            s.other_versions
        } else {
            s.other_protocols
        };
        if extra > 0 {
            if let Some((_, value)) = values.iter_mut().find(|(name, _)| name == "other") {
                *value += extra;
            } else {
                values.push(("other".into(), extra));
            }
        }
        for (key, count) in values {
            metric.with_label_values(&[&key]).set(count as f64);
        }
        registry.register(Box::new(metric)).map_err(Error::other)?;
    }
    let mut bytes = Vec::new();
    TextEncoder::new()
        .encode(&registry.gather(), &mut bytes)
        .map_err(Error::other)?;
    String::from_utf8(bytes).map_err(Error::other)
}
pub fn bounded_counts(counts: &BTreeMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
    let mut pairs: Vec<_> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let other: usize = pairs.iter().skip(limit).map(|(_, n)| n).sum();
    pairs.truncate(limit);
    // Reserved aggregate label cannot collide with peer-supplied values.
    for (label, _) in &mut pairs {
        *label = format!("reported:{label}");
    }
    if other > 0 {
        pairs.push(("other".into(), other));
    }
    pairs
}
