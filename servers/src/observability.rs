//! Explicit Portfu monitoring services, isolated from process-wide route inventory.
use portfu::prelude::{
    PortfuError, Request, Response, ServerHandle, Service, ServiceBuilder, ServiceTrait,
    http::{Method, StatusCode},
};
use prometheus::{Encoder, Gauge, Registry, TextEncoder};
use std::{future::Future, io::Error, net::SocketAddr, pin::Pin, sync::Arc};

pub struct Snapshot {
    pub healthy: bool,
    pub message: String,
    pub metrics: String,
}
pub type SnapshotFn =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Snapshot, Error>> + Send>> + Send + Sync>;
struct Endpoint(SnapshotFn);
pub fn service(snapshot: SnapshotFn) -> Service {
    ServiceBuilder::new("/*")
        .name("monitoring")
        .handler(Arc::new(Endpoint(snapshot)))
        .build()
}
impl ServiceTrait for Endpoint {
    fn name(&self) -> &str {
        "monitoring"
    }
    fn serve<'a>(
        &'a self,
        request: &'a mut Request,
    ) -> Pin<Box<dyn Future<Output = Result<Response, PortfuError>> + Send + 'a>> {
        Box::pin(async move {
            if !matches!(*request.method(), Method::GET | Method::HEAD) {
                return Ok(Response::from_status_and_message(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "read-only monitoring endpoint",
                ));
            }
            let path = request.uri().path();
            if !matches!(path, "/metrics" | "/health") {
                return Ok(Response::not_found("unknown monitoring endpoint"));
            }
            let snapshot =
                tokio::time::timeout(std::time::Duration::from_secs(5), (self.0)()).await;
            let snapshot = match snapshot {
                Ok(Ok(snapshot)) => snapshot,
                _ => {
                    return Ok(Response::from_status_and_message(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "service state unavailable",
                    ));
                }
            };
            Ok(if path == "/metrics" {
                Response::ok(snapshot.metrics)
                    .content_type("text/plain; version=0.0.4; charset=utf-8")
            } else {
                Response::from_status_and_message(
                    if snapshot.healthy {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    },
                    snapshot.message,
                )
                .content_type("text/plain; charset=utf-8")
            })
        })
    }
}
pub fn encode(registry: &Registry) -> Result<String, Error> {
    let mut bytes = Vec::new();
    TextEncoder::new()
        .encode(&registry.gather(), &mut bytes)
        .map_err(Error::other)?;
    String::from_utf8(bytes).map_err(Error::other)
}
/// Snapshot measurements are gauges, including database counts that can decrease.
pub fn gauges(prefix: &str, values: &[(&str, &str, f64)]) -> Result<String, Error> {
    let registry = Registry::new();
    for (name, help, value) in values {
        let gauge = Gauge::new(format!("{prefix}_{name}"), *help).map_err(Error::other)?;
        gauge.set(*value);
        registry.register(Box::new(gauge)).map_err(Error::other)?;
    }
    encode(&registry)
}
pub struct Monitor {
    handle: ServerHandle,
    task: Option<tokio::task::JoinHandle<Result<(), PortfuError>>>,
}
impl Monitor {
    pub fn start(listen: SocketAddr, snapshot: SnapshotFn) -> Self {
        let server = portfu::prelude::ServerBuilder::new()
            .host(listen.ip().to_string())
            .port(listen.port())
            .service(service(snapshot))
            .build();
        let handle = server.handle();
        let task = Some(tokio::spawn(server.run()));
        Self { handle, task }
    }
    pub fn finished(&self) -> bool {
        self.task.as_ref().is_none_or(|t| t.is_finished())
    }
    pub async fn stop(mut self) -> Result<(), Error> {
        self.handle.shutdown();
        if let Some(task) = self.task.take() {
            task.await
                .map_err(Error::other)?
                .map_err(|e| Error::other(e.to_string()))?;
        }
        Ok(())
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.handle.shutdown();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub async fn run_with(
    operation: impl Future<Output = Result<(), Error>>,
    listen: Option<SocketAddr>,
    snapshot: SnapshotFn,
) -> Result<(), Error> {
    let Some(listen) = listen else {
        return operation.await;
    };
    let mut monitor = Monitor::start(listen, snapshot);
    let task = monitor
        .task
        .as_mut()
        .ok_or_else(|| Error::other("monitoring task missing"))?;
    tokio::pin!(operation);
    tokio::select! {
        result=&mut operation => { result.and(monitor.stop().await) },
        result=task => {
            result.map_err(Error::other)?.map_err(|e|Error::other(e.to_string()))?;
            Err(Error::other("monitoring server stopped unexpectedly"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    async fn get(addr: SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        socket
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            socket.read_to_end(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        String::from_utf8(bytes).unwrap()
    }
    #[tokio::test]
    async fn metrics_and_health_follow_state_and_routes_are_isolated() {
        let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = reservation.local_addr().unwrap();
        drop(reservation);
        let healthy = Arc::new(AtomicBool::new(true));
        let copy = healthy.clone();
        let callback: SnapshotFn = Arc::new(move || {
            let healthy = copy.clone();
            Box::pin(async move {
                Ok(Snapshot {
                    healthy: healthy.load(Ordering::Relaxed),
                    message: "runtime state".into(),
                    metrics: gauges("test_service", &[("jobs", "Observed jobs", 3.0)])?,
                })
            })
        });
        let monitor = Monitor::start(addr, callback);
        let start = std::time::Instant::now();
        loop {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                break;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(get(addr, "/health").await.starts_with("HTTP/1.1 200"));
        let metrics = get(addr, "/metrics").await;
        assert!(metrics.contains("test_service_jobs 3\n"));
        assert!(metrics.contains("text/plain; version=0.0.4"));
        healthy.store(false, Ordering::Relaxed);
        assert!(get(addr, "/health").await.starts_with("HTTP/1.1 503"));
        assert!(
            get(addr, "/get_blockchain_state")
                .await
                .starts_with("HTTP/1.1 404")
        );
        monitor.stop().await.unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    }
    #[tokio::test]
    async fn monitoring_bind_failure_fails_the_runtime() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let callback: SnapshotFn = Arc::new(|| {
            Box::pin(async {
                Ok(Snapshot {
                    healthy: true,
                    message: "ok".into(),
                    metrics: String::new(),
                })
            })
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_with(std::future::pending(), Some(address), callback),
        )
        .await
        .unwrap();
        assert!(result.is_err());
    }
}
