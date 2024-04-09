use async_trait::async_trait;
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs, load_certs_from_bytes, load_private_key,
    load_private_key_from_bytes, AllowAny, SslInfo, CHIA_CA_CRT, CHIA_CA_KEY,
};
use http::request::Parts;
use http::{Method, StatusCode};
use http_body_util::Full;
use hyper::body::{Body, Bytes, Incoming, SizeHint};
use hyper::header::HeaderValue;
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use log::error;
#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericCounterVec};
#[cfg(feature = "metrics")]
use prometheus::{HistogramOpts, HistogramVec, Opts, Registry};
use rustls::{RootCertStore, ServerConfig};
use std::env;
use std::io::{Error, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::select;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;

#[async_trait]
pub trait RpcHandler {
    async fn handle(
        &self,
        request: RpcRequest,
        response: Response<Full<Bytes>>,
        address: &SocketAddr,
    ) -> Result<Response<Full<Bytes>>, (Parts, HeaderMap, Error)>;
}

pub fn extract_parts_and_drop_body(req: RpcRequest) -> (Parts, HeaderMap) {
    match req.request_type {
        RequestType::Stream(r) => (r.into_parts().0, req.response_headers),
        RequestType::Sized(r) => (r.into_parts().0, req.response_headers),
    }
}

pub struct RpcRequest {
    pub request_type: RequestType,
    pub response_headers: HeaderMap,
}
impl RpcRequest {
    pub fn get_best_guess_public_ip(&self, address: &SocketAddr) -> String {
        if let Some(real_ip) = self.headers().get("x-real-ip") {
            format!("{:?}", real_ip)
        } else if let Some(forwards) = self.headers().get("x-forwarded-for") {
            format!("{:?}", forwards)
        } else {
            address.to_string()
        }
    }
    pub fn uri(&self) -> &Uri {
        match &self.request_type {
            RequestType::Sized(r) => r.uri(),
            RequestType::Stream(r) => r.uri(),
        }
    }
    pub fn headers(&self) -> &HeaderMap<HeaderValue> {
        match &self.request_type {
            RequestType::Sized(r) => r.headers(),
            RequestType::Stream(r) => r.headers(),
        }
    }
    pub fn method(&self) -> &Method {
        match &self.request_type {
            RequestType::Sized(r) => r.method(),
            RequestType::Stream(r) => r.method(),
        }
    }
    pub fn size_hint(&self) -> SizeHint {
        match &self.request_type {
            RequestType::Sized(r) => r.size_hint(),
            RequestType::Stream(r) => r.size_hint(),
        }
    }
}

// impl From<Request<Incoming>> for RpcRequest {
//     fn from(value: Request<Incoming>) -> Self {
//         Self {
//             request_type: RequestType::Stream(value),
//             response_headers: Default::default(),
//         }
//     }
// }
//
// impl From<Request<Full<Bytes>>> for RpcRequest {
//     fn from(value: Request<Full<Bytes>>) -> Self {
//         Self {
//             request_type: RequestType::Sized(value),
//             response_headers: Default::default(),
//         }
//     }
// }

pub enum RequestType {
    Stream(Request<Incoming>),
    Sized(Request<Full<Bytes>>),
}

#[cfg(feature = "metrics")]
#[non_exhaustive]
pub struct EndpointMetrics {
    pub total_requests: Arc<GenericCounterVec<AtomicU64>>,
    pub successful_requests: Arc<GenericCounterVec<AtomicU64>>,
    pub failed_requests: Arc<GenericCounterVec<AtomicU64>>,
    pub blocked_requests: Arc<GenericCounterVec<AtomicU64>>,
    pub average_request_time: Arc<HistogramVec>,
}
#[cfg(feature = "metrics")]
impl EndpointMetrics {
    pub fn new(registry: &Registry) -> Result<Self, prometheus::Error> {
        let labels = &["code", "method", "path"];
        Ok(Self {
            total_requests: Arc::new(
                GenericCounterVec::new(
                    Opts::new(
                        format!("total_requests"),
                        "Total requests server has handled",
                    ),
                    labels,
                )
                .map(|g: GenericCounterVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                    g
                })?,
            ),
            successful_requests: Arc::new(
                GenericCounterVec::new(
                    Opts::new(
                        format!("successful_requests"),
                        "Total successful requests server has handled",
                    ),
                    labels,
                )
                .map(|g: GenericCounterVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                    g
                })?,
            ),
            failed_requests: Arc::new(
                GenericCounterVec::new(
                    Opts::new(
                        format!("failed_requests"),
                        "Total failed requests server has handled",
                    ),
                    labels,
                )
                .map(|g: GenericCounterVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                    g
                })?,
            ),
            blocked_requests: Arc::new(
                GenericCounterVec::new(
                    Opts::new(
                        format!("blocked_requests"),
                        "Total blocked requests server has handled",
                    ),
                    labels,
                )
                .map(|g: GenericCounterVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                    g
                })?,
            ),
            average_request_time: Arc::new({
                let opts = HistogramOpts::new("average_request_time", "Average Request Time");
                HistogramVec::new(opts, labels).map(|h: HistogramVec| {
                    registry.register(Box::new(h.clone())).unwrap_or(());
                    h
                })?
            }),
        })
    }
}

#[cfg(feature = "metrics")]
pub struct RpcMetrics {
    pub request_metrics: Arc<EndpointMetrics>,
}

pub enum MiddleWareResult {
    Continue(RpcRequest),
    Bypass(RpcRequest),
    Failure(Parts, Response<Full<Bytes>>),
}

#[async_trait]
pub trait MiddleWare {
    async fn handle(
        &self,
        req: RpcRequest,
        address: &SocketAddr,
    ) -> Result<MiddleWareResult, Error>;

    fn name(&self) -> &str;
    fn set_enabled(&self, enabled: bool);
    fn is_enabled(&self) -> bool;
}

#[derive(Default)]
pub struct DefaultRpcHandler {}
#[async_trait]
impl RpcHandler for DefaultRpcHandler {
    async fn handle(
        &self,
        _: RpcRequest,
        mut response: Response<Full<Bytes>>,
        _: &SocketAddr,
    ) -> Result<Response<Full<Bytes>>, (Parts, HeaderMap, Error)> {
        *response.body_mut() = Full::new(Bytes::from("HTTP NOT SUPPORTED ON THIS ENDPOINT"));
        Ok(response)
    }
}

pub struct RpcServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl_info: Option<SslInfo>,
}

pub struct RpcServer {
    pub socket_address: SocketAddr,
    pub server_config: Arc<ServerConfig>,
    pub handler: Arc<dyn RpcHandler + Send + Sync + 'static>,
    pub middleware: Arc<Vec<Box<dyn MiddleWare + Send + Sync + 'static>>>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<RpcMetrics>,
}
impl RpcServer {
    pub fn new(
        config: &RpcServerConfig,
        handler: Arc<dyn RpcHandler + Send + Sync + 'static>,
        #[cfg(feature = "metrics")] metrics: Arc<RpcMetrics>,
    ) -> Result<Self, Error> {
        let middleware: Vec<Box<dyn MiddleWare + Send + Sync + 'static>> = vec![];
        Self::new_with_middleware(
            config,
            handler,
            Arc::new(middleware),
            #[cfg(feature = "metrics")]
            metrics,
        )
    }
    pub fn new_with_middleware(
        config: &RpcServerConfig,
        handler: Arc<dyn RpcHandler + Send + Sync + 'static>,
        middleware: Arc<Vec<Box<dyn MiddleWare + Send + Sync + 'static>>>,
        #[cfg(feature = "metrics")] metrics: Arc<RpcMetrics>,
    ) -> Result<Self, Error> {
        let server_config = Self::init(config)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("Invalid Cert: {:?}", e)))?;
        let socket_address = Self::init_socket(config)?;
        Ok(RpcServer {
            socket_address,
            server_config,
            handler,
            middleware,
            #[cfg(feature = "metrics")]
            metrics,
        })
    }

    pub async fn run(self, run: Arc<AtomicBool>) -> Result<(), Error> {
        let server = Arc::new(self);
        let listener = TcpListener::bind(server.socket_address).await?;
        let acceptor = TlsAcceptor::from(server.server_config.clone());
        let mut http = Builder::new();
        http.keep_alive(true);
        while run.load(Ordering::Relaxed) {
            select!(
                res = listener.accept() => {
                    match res {
                        Ok((stream, address)) => {
                            match acceptor.accept(stream).await {
                                Ok(stream) => {
                                    let server = server.clone();
                                    let service = service_fn(move |req| {
                                        let server = server.clone();
                                        connection_handler(server, req, address)
                                    });
                                    let connection = http.serve_connection(TokioIo::new(stream), service);
                                    tokio::spawn( async move {
                                        if let Err(err) = connection.await {
                                            error!("Error serving connection: {:?}", err);
                                        }
                                        Ok::<(), Error>(())
                                    });
                                }
                                Err(e) => {
                                    error!("Error accepting connection: {:?}", e);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Error accepting connection: {:?}", e);
                        }
                    }
                },
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            )
        }
        Ok(())
    }

    pub fn init(config: &RpcServerConfig) -> Result<Arc<ServerConfig>, Error> {
        let (certs, key, root_certs) = if let Some(ssl_info) = &config.ssl_info {
            (
                load_certs(&format!(
                    "{}/{}",
                    &ssl_info.root_path, &ssl_info.certs.private_crt
                ))?,
                load_private_key(&format!(
                    "{}/{}",
                    &ssl_info.root_path, &ssl_info.certs.private_key
                ))?,
                load_certs(&format!(
                    "{}/{}",
                    &ssl_info.root_path, &ssl_info.ca.private_crt
                ))?,
            )
        } else if let (Some(crt), Some(key)) = (
            env::var("PRIVATE_CA_CRT").ok(),
            env::var("PRIVATE_CA_KEY").ok(),
        ) {
            let (cert_bytes, key_bytes) =
                generate_ca_signed_cert_data(crt.as_bytes(), key.as_bytes())?;
            (
                load_certs_from_bytes(&cert_bytes)?,
                load_private_key_from_bytes(&key_bytes)?,
                load_certs_from_bytes(crt.as_bytes())?,
            )
        } else {
            let (cert_bytes, key_bytes) =
                generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())?;
            (
                load_certs_from_bytes(&cert_bytes)?,
                load_private_key_from_bytes(&key_bytes)?,
                load_certs_from_bytes(CHIA_CA_CRT.as_bytes())?,
            )
        };
        let mut root_cert_store = RootCertStore::empty();
        for cert in root_certs {
            root_cert_store.add(&cert).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid Root Cert for Server: {:?}", e),
                )
            })?;
        }
        Ok(Arc::new(
            ServerConfig::builder()
                .with_safe_defaults()
                .with_client_cert_verifier(AllowAny::new(root_cert_store))
                .with_single_cert(certs, key)
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("Invalid Cert for Server: {:?}", e),
                    )
                })?,
        ))
    }

    pub fn init_socket(config: &RpcServerConfig) -> Result<SocketAddr, Error> {
        Ok(SocketAddr::from((
            Ipv4Addr::from_str(if config.host == "localhost" {
                "127.0.0.1"
            } else {
                &config.host
            })
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to parse Host: {:?}", e),
                )
            })?,
            config.port,
        )))
    }
}

async fn connection_handler(
    server: Arc<RpcServer>,
    req: Request<Incoming>,
    address: SocketAddr,
) -> Result<Response<Full<Bytes>>, Error> {
    #[cfg(feature = "metrics")]
    let start = Instant::now();
    let mut req: RpcRequest = RpcRequest {
        request_type: RequestType::Stream(req),
        response_headers: Default::default(),
    };
    let middleware_arc = server.middleware.clone();
    for middleware in middleware_arc.as_slice() {
        if middleware.is_enabled() {
            match middleware.handle(req, &address).await? {
                MiddleWareResult::Bypass(r) => {
                    req = r;
                    break;
                }
                MiddleWareResult::Continue(r) => {
                    req = r;
                    continue;
                }
                MiddleWareResult::Failure(parts, res) => {
                    #[cfg(feature = "metrics")]
                    {
                        match res.status() {
                            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::TOO_MANY_REQUESTS => {
                                set_blocked_metrics(
                                    server.clone(),
                                    res.status().as_str(),
                                    parts.method.as_str(),
                                    parts.uri.path(),
                                    start,
                                );
                            }
                            _ => {
                                set_failed_metrics(
                                    server.clone(),
                                    res.status().as_str(),
                                    parts.method.as_str(),
                                    parts.uri.path(),
                                    start,
                                );
                            }
                        }
                    }
                    return Ok(res);
                }
            }
        }
    }
    #[cfg(feature = "metrics")]
    let (method, path) = { (req.method().to_string(), req.uri().path().to_string()) };
    let response = Response::builder()
        .header(
            "Access-Control-Allow-Origin",
            req.headers()
                .get("origin")
                .unwrap_or(&HeaderValue::from_static("*")),
        )
        .header(
            "Access-Control-Allow-Credentials",
            HeaderValue::from_static("true"),
        )
        .header(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("POST, GET"),
        )
        .body(Full::new(Bytes::new()))
        .expect("Failed to create default request");
    match server.handler.handle(req, response, &address).await {
        Ok(response) => {
            #[cfg(feature = "metrics")]
            {
                set_success_metrics(
                    server.clone(),
                    response.status().as_str(),
                    &method,
                    &path,
                    start,
                );
            }
            Ok(response)
        }
        Err((_parts, _headers, e)) => {
            #[cfg(feature = "metrics")]
            {
                set_failed_metrics(
                    server.clone(),
                    StatusCode::INTERNAL_SERVER_ERROR.as_str(),
                    &method,
                    &path,
                    start,
                );
            }
            Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("Access-Control-Allow-Origin", "*")
                .body(Full::new(Bytes::from(format!("{e:?}"))))
                .expect("Failed to create 500 body"))
        }
    }
}

#[cfg(feature = "metrics")]
pub fn set_failed_metrics(
    server: Arc<RpcServer>,
    code: &str,
    method: &str,
    path: &str,
    start: Instant,
) {
    server
        .metrics
        .request_metrics
        .failed_requests
        .with_label_values(&[code, method, path])
        .inc();
    common_metrics(server, code, method, path, start);
}

#[cfg(feature = "metrics")]
pub fn set_success_metrics(
    server: Arc<RpcServer>,
    code: &str,
    method: &str,
    path: &str,
    start: Instant,
) {
    server
        .metrics
        .request_metrics
        .successful_requests
        .with_label_values(&[code, method, path])
        .inc();
    common_metrics(server, code, method, path, start);
}

#[cfg(feature = "metrics")]
pub fn set_blocked_metrics(
    server: Arc<RpcServer>,
    code: &str,
    method: &str,
    path: &str,
    start: Instant,
) {
    server
        .metrics
        .request_metrics
        .blocked_requests
        .with_label_values(&[code, method, path])
        .inc();
    common_metrics(server, code, method, path, start);
}

#[cfg(feature = "metrics")]
pub fn common_metrics(
    server: Arc<RpcServer>,
    code: &str,
    method: &str,
    path: &str,
    start: Instant,
) {
    server
        .metrics
        .request_metrics
        .average_request_time
        .with_label_values(&[code, method, path])
        .observe(duration_to_seconds(Instant::now().duration_since(start)));
    server
        .metrics
        .request_metrics
        .total_requests
        .with_label_values(&[code, method, path])
        .inc();
}

#[inline]
pub fn duration_to_seconds(d: Duration) -> f64 {
    let nanos = f64::from(d.subsec_nanos()) / 1e9;
    d.as_secs() as f64 + nanos
}
