use async_trait::async_trait;
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs, load_certs_from_bytes, load_private_key,
    load_private_key_from_bytes, AllowAny, SslInfo, CHIA_CA_CRT, CHIA_CA_KEY,
};
use http::request::Parts;
use http_body_util::Full;
use hyper::body::{Body, Bytes, Incoming, SizeHint};
use hyper::header::HeaderValue;
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper::{HeaderMap, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use log::error;
#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericCounter};
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
use tokio_rustls::TlsAcceptor;

#[async_trait]
pub trait RpcHandler {
    async fn handle(&self, req: RequestType) -> Result<Response<Full<Bytes>>, (Parts, Error)>;
}

pub fn extract_parts_and_drop_body(req: RequestType) -> Parts {
    match req {
        RequestType::Stream(r) => r.into_parts().0,
        RequestType::Sized(r) => r.into_parts().0,
    }
}

pub enum RequestType {
    Stream(Request<Incoming>),
    Sized(Request<Full<Bytes>>),
}
impl RequestType {
    pub fn uri(&self) -> &Uri {
        match self {
            RequestType::Sized(r) => r.uri(),
            RequestType::Stream(r) => r.uri(),
        }
    }
    pub fn headers(&self) -> &HeaderMap<HeaderValue> {
        match self {
            RequestType::Sized(r) => r.headers(),
            RequestType::Stream(r) => r.headers(),
        }
    }
    pub fn size_hint(&self) -> SizeHint {
        match self {
            RequestType::Sized(r) => r.size_hint(),
            RequestType::Stream(r) => r.size_hint(),
        }
    }
}

pub enum MiddleWareResult {
    Stream(Request<Incoming>),
    Sized(Request<Full<Bytes>>),
    Failure(Parts, Response<Full<Bytes>>),
}

#[async_trait]
pub trait MiddleWare {
    async fn handle(
        &self,
        req: RequestType,
        address: &SocketAddr,
    ) -> Result<MiddleWareResult, Error>;

    async fn set_enabled(&self, enabled: bool);
    async fn is_enabled(&self) -> bool;
}

#[derive(Default)]
pub struct DefaultRpcHandler {}
#[async_trait]
impl RpcHandler for DefaultRpcHandler {
    async fn handle(&self, _: RequestType) -> Result<Response<Full<Bytes>>, (Parts, Error)> {
        Ok(Response::new(Full::new(Bytes::from(
            "HTTP NOT SUPPORTED ON THIS ENDPOINT",
        ))))
    }
}

pub struct RpcServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl_info: Option<SslInfo>,
}

#[cfg(feature = "metrics")]
pub struct RpcMetrics {
    pub handled_by_middleware: Arc<GenericCounter<AtomicU64>>,
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
    let mut req = RequestType::Stream(req);
    let middleware_arc = server.middleware.clone();
    for middleware in middleware_arc.as_slice() {
        if middleware.is_enabled().await {
            match middleware.handle(req, &address).await? {
                MiddleWareResult::Stream(r) => {
                    req = RequestType::Stream(r);
                }
                MiddleWareResult::Sized(r) => {
                    req = RequestType::Sized(r);
                }
                MiddleWareResult::Failure(_parts, res) => {
                    return Ok(res);
                }
            }
        }
    }
    match server.handler.handle(req).await {
        Ok(res) => Ok(res),
        Err((_parts, e)) => Err(e),
    }
}
