use crate::Config;
use dg_xch_servers::discovery::{Counts, Store};
use portfu::prelude::{
    PortfuError, Request, Response, Service, ServiceBuilder, ServiceTrait,
    http::{Method, StatusCode},
};
use prometheus::{
    Encoder, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts,
    Registry, TextEncoder,
};
use std::{
    io::Error,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub struct Metrics {
    registry: Registry,
    pub connections: IntCounter,
    pub rejected_connections: IntCounterVec,
    pub handshakes: IntCounter,
    pub requests: IntCounter,
    pub responses: IntCounter,
    pub peers_sent: IntCounter,
    pub skipped_probes: IntCounterVec,
    pub registrations: IntCounter,
    pub expirations: IntCounter,
    pub probe_attempts: IntCounter,
    pub probe_duration: Histogram,
    pub response_size: Histogram,
    active_sessions: IntGauge,
    active_probes: IntGauge,
    sessions: IntCounterVec,
    probes: IntCounterVec,
    known_peers: IntGauge,
    catalog_peers: IntGauge,
    address_family: IntGaugeVec,
    cooldowns: IntGauge,
    pending_verification: IntGauge,
    uptime: IntGauge,
    started: Instant,
}
impl Metrics {
    pub fn new(config: &Config) -> Result<Arc<Self>, Error> {
        let registry = Registry::new_custom(
            None,
            Some(std::collections::HashMap::from([(
                "network".into(),
                config.chain.handshake_network_id().map_err(Error::other)?,
            )])),
        )
        .map_err(Error::other)?;
        macro_rules! metric {
            ($metric:expr) => {{
                let metric = $metric.map_err(Error::other)?;
                registry
                    .register(Box::new(metric.clone()))
                    .map_err(Error::other)?;
                metric
            }};
        }
        macro_rules! counter {
            ($name:literal, $help:literal) => {
                metric!(IntCounter::new(concat!("chia_introducer_", $name), $help))
            };
        }
        macro_rules! gauge {
            ($name:literal, $help:literal) => {
                metric!(IntGauge::new(concat!("chia_introducer_", $name), $help))
            };
        }
        macro_rules! labels {
            ($name:literal, $help:literal, $label:literal) => {
                metric!(IntCounterVec::new(
                    Opts::new(concat!("chia_introducer_", $name), $help),
                    &[$label]
                ))
            };
        }
        let start = gauge!("start_time_seconds", "Unix timestamp at service startup");
        start.set(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()) as i64,
        );
        let connection_capacity = gauge!(
            "connection_capacity",
            "Configured maximum concurrent sessions"
        );
        connection_capacity.set(config.max_connections as i64);
        let ttl = gauge!(
            "peer_ttl_seconds",
            "Lifetime of a successful peer verification"
        );
        ttl.set(config.peer_ttl_seconds as i64);
        gauge!(
            "probe_concurrency",
            "Configured shared outbound probe limit"
        )
        .set(config.probe_concurrency as i64);
        gauge!(
            "probe_interval_ms",
            "Configured shared minimum spacing between outbound probes"
        )
        .set(config.probe_interval_ms as i64);
        let metrics = Arc::new(Self {
            connections: counter!(
                "connections_total",
                "Accepted TCP connections before admission checks"
            ),
            rejected_connections: labels!(
                "connections_rejected_total",
                "Rejected TCP connections",
                "reason"
            ),
            handshakes: counter!(
                "handshakes_total",
                "Validated full-node handshakes on our network"
            ),
            requests: counter!(
                "peer_requests_total",
                "Valid introducer peer requests received"
            ),
            responses: counter!(
                "peer_responses_total",
                "Peer responses successfully written to the socket"
            ),
            peers_sent: counter!(
                "peers_sent_total",
                "Total advertised endpoint entries successfully sent"
            ),
            skipped_probes: labels!(
                "probes_skipped_total",
                "Advertised endpoints not probed",
                "reason"
            ),
            registrations: counter!(
                "registrations_total",
                "Successful verified peer registrations including refreshes"
            ),
            expirations: counter!(
                "peers_expired_total",
                "Successful verifications whose TTL has expired"
            ),
            probe_attempts: counter!(
                "probe_attempts_total",
                "Outbound endpoint verification attempts"
            ),
            probe_duration: metric!(Histogram::with_opts(
                HistogramOpts::new(
                    "chia_introducer_probe_duration_seconds",
                    "Verification duration for successful, failed, timed out or cancelled probes"
                )
                .buckets(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0])
            )),
            response_size: metric!(Histogram::with_opts(
                HistogramOpts::new(
                    "chia_introducer_response_peers",
                    "Number of endpoints in successfully written responses"
                )
                .buckets(vec![0.0, 1.0, 5.0, 10.0, 25.0, 50.0, 100.0])
            )),
            active_sessions: gauge!(
                "active_connections",
                "Admitted sessions currently in flight"
            ),
            active_probes: gauge!(
                "active_probes",
                "Outbound verification probes currently in flight"
            ),
            sessions: labels!(
                "sessions_total",
                "Session outcomes; errors include TLS, protocol and write errors",
                "result"
            ),
            probes: labels!("probes_total", "Outbound verification outcomes", "result"),
            catalog_peers: gauge!(
                "known_peers",
                "Shared per-network known endpoints, including expired or unverified peers"
            ),
            known_peers: gauge!(
                "verified_peers",
                "Shared per-network endpoints with unexpired successful verification"
            ),
            address_family: metric!(IntGaugeVec::new(
                Opts::new(
                    "chia_introducer_verified_address_family",
                    "Unexpired verified endpoints by IP family"
                ),
                &["family"]
            )),
            pending_verification: gauge!(
                "pending_verification",
                "Unverified endpoints queued for background verification"
            ),
            cooldowns: gauge!(
                "probe_cooldowns",
                "IPs currently under the 60 second probe cooldown"
            ),
            uptime: gauge!("uptime_seconds", "Monotonic service uptime"),
            registry,
            started: Instant::now(),
        });
        for reason in ["capacity", "duplicate_ip"] {
            metrics.rejected_connections.with_label_values(&[reason]);
        }
        for reason in ["invalid_address", "ip_cooldown"] {
            metrics.skipped_probes.with_label_values(&[reason]);
        }
        for result in ["success", "error", "timeout", "cancelled"] {
            metrics.sessions.with_label_values(&[result]);
            metrics.probes.with_label_values(&[result]);
        }
        metrics.address_family.with_label_values(&["ipv4"]);
        metrics.address_family.with_label_values(&["ipv6"]);
        Ok(metrics)
    }
    pub fn session(&self) -> Guard {
        Guard::new(self.active_sessions.clone(), self.sessions.clone())
    }
    pub fn probe(&self) -> Guard {
        self.probe_attempts.inc();
        Guard::new(self.active_probes.clone(), self.probes.clone())
    }
    pub fn update_counts(&self, counts: &Counts) {
        self.known_peers.set(counts.verified);
        self.pending_verification.set(counts.pending_verification);
        self.catalog_peers.set(counts.known);
        self.cooldowns.set(counts.cooldowns);
        self.address_family
            .with_label_values(&["ipv4"])
            .set(counts.ipv4);
        self.address_family
            .with_label_values(&["ipv6"])
            .set(counts.ipv6);
    }
    pub fn render(&self) -> Result<String, Error> {
        self.uptime.set(self.started.elapsed().as_secs() as i64);
        let mut bytes = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut bytes)
            .map_err(Error::other)?;
        String::from_utf8(bytes).map_err(Error::other)
    }
}
// Capture guards in the task before spawning so aborting an unpolled task also decrements gauges.
pub struct Guard {
    active: IntGauge,
    outcomes: IntCounterVec,
    finished: bool,
}
impl Guard {
    fn new(active: IntGauge, outcomes: IntCounterVec) -> Self {
        active.inc();
        Self {
            active,
            outcomes,
            finished: false,
        }
    }
    pub fn finish(&mut self, result: &'static str) {
        self.outcomes.with_label_values(&[result]).inc();
        self.finished = true;
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.active.dec();
        if !self.finished {
            self.outcomes.with_label_values(&["cancelled"]).inc();
        }
    }
}
struct Endpoint {
    metrics: Arc<Metrics>,
    store: Store,
    config: Arc<Config>,
}
pub fn server(
    config: Arc<Config>,
    metrics: Arc<Metrics>,
    store: Store,
) -> Option<portfu::prelude::Server> {
    let listen = config.metrics_listen?;
    let service: Service = ServiceBuilder::new("/*")
        .name("introducer_metrics")
        .handler(Arc::new(Endpoint {
            metrics,
            store,
            config,
        }))
        .build();
    Some(
        portfu::prelude::ServerBuilder::new()
            .host(listen.ip().to_string())
            .port(listen.port())
            .service(service)
            .build(),
    )
}
impl ServiceTrait for Endpoint {
    fn name(&self) -> &str {
        "introducer_metrics"
    }
    fn serve<'a>(
        &'a self,
        request: &'a mut Request,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Response, PortfuError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if !matches!(*request.method(), Method::GET | Method::HEAD) {
                return Ok(Response::from_status_and_message(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "read-only metrics endpoint",
                ));
            }
            if let Some(asset) = crate::dashboard::asset(request.uri().path()) {
                return Ok(asset);
            }
            match request.uri().path() {
                "/api/stats" => {
                    let counts = self.store.counts().await.map_err(PortfuError::Io)?;
                    let network = &self.store.network;
                    let read = |counter: &IntCounterVec, label: &str| {
                        counter.with_label_values(&[label]).get()
                    };
                    let stats = serde_json::json!({
                        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
                        "network": network, "uptime_seconds": self.metrics.started.elapsed().as_secs(),
                        "known_peers": counts.known, "verified_peers": counts.verified, "storage": "postgres", "pending_verification":counts.pending_verification,
                        "ipv4_peers": counts.ipv4, "ipv6_peers": counts.ipv6,
                        "connection_capacity": self.config.max_connections, "peer_ttl_seconds": self.config.peer_ttl_seconds,
                        "connections": self.metrics.connections.get(), "handshakes": self.metrics.handshakes.get(),
                        "peer_requests": self.metrics.requests.get(), "peer_responses": self.metrics.responses.get(),
                        "peers_sent": self.metrics.peers_sent.get(), "registrations": self.metrics.registrations.get(),
                        "expired_peers": self.metrics.expirations.get(), "probe_cooldowns": counts.cooldowns,
                        "active_connections": self.metrics.active_sessions.get(), "active_probes": self.metrics.active_probes.get(),
                        "probe_attempts": self.metrics.probe_attempts.get(),
                        "probe_successes": read(&self.metrics.probes, "success"), "probe_errors": read(&self.metrics.probes, "error"),
                        "probe_timeouts": read(&self.metrics.probes, "timeout"),
                        "session_errors": read(&self.metrics.sessions, "error"), "session_timeouts": read(&self.metrics.sessions, "timeout"),
                        "rejected_capacity": read(&self.metrics.rejected_connections, "capacity"),
                        "rejected_duplicate_ip": read(&self.metrics.rejected_connections, "duplicate_ip"),
                    });
                    let text =
                        serde_json::to_vec(&stats).map_err(|e| PortfuError::Io(Error::other(e)))?;
                    Ok(crate::dashboard::response(text, "application/json"))
                }
                "/metrics" => {
                    self.metrics
                        .update_counts(&self.store.counts().await.map_err(PortfuError::Io)?);
                    let text = self.metrics.render().map_err(PortfuError::Io)?;
                    Ok(Response::ok(text).content_type("text/plain; version=0.0.4; charset=utf-8"))
                }
                "/health" => Ok(Response::ok("ok").content_type("text/plain")),
                "/ready" => {
                    let ready = self.store.healthy().await;
                    Ok(Response::from_status_and_message(
                        if ready {
                            StatusCode::OK
                        } else {
                            StatusCode::SERVICE_UNAVAILABLE
                        },
                        if ready {
                            "ready"
                        } else {
                            "backend unavailable"
                        },
                    )
                    .content_type("text/plain"))
                }
                _ => Ok(Response::not_found("unknown introducer metrics endpoint")),
            }
        })
    }
}
