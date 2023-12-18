use async_trait::async_trait;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use std::io::Error;
use std::marker::PhantomData;
use std::sync::Arc;

#[async_trait]
pub trait RpcHandler<T> {
    async fn handle(
        &self,
        req: Request<Incoming>,
        shared_connection_data: Arc<T>,
    ) -> Result<Response<Full<Bytes>>, Error>;
}

#[derive(Default)]
pub struct DefaultRpcHandler<T> {
    _p: PhantomData<T>,
}
#[async_trait]
impl<T: Send + Sync> RpcHandler<T> for DefaultRpcHandler<T> {
    async fn handle(
        &self,
        _: Request<Incoming>,
        _: Arc<T>,
    ) -> Result<Response<Full<Bytes>>, Error> {
        Ok(Response::new(Full::new(Bytes::from(
            "HTTP NOT SUPPORTED ON THIS ENDPOINT",
        ))))
    }
}

impl<T> DefaultRpcHandler<T> {
    pub fn new() -> Self {
        DefaultRpcHandler { _p: PhantomData }
    }
}
