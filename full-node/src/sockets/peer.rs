use crate::server::NodeServices;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::WebsocketMsgStream;
use dg_xch_core::traits::SizedBytes;
use futures_util::stream::FusedStream;
use futures_util::{Sink, Stream};
use portfu::prelude::{ConnectionInfo, Message, PortfuError, State, WebSocket, websocket};
use std::future::Future;
use std::io::Error;
use std::pin::Pin;
use std::task::{Context, Poll};

type IoFuture<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;

/// Adapts Portfu's concurrently usable websocket handle to the stream/sink shape used by the
/// protocol core. Only one read and one write future are active at a time, matching Sink's
/// contract while retaining Portfu's independent read/write halves.
struct PortfuPeerTransport {
    socket: WebSocket,
    read: Option<IoFuture<Option<Message>>>,
    write: Option<IoFuture<()>>,
    close: Option<IoFuture<()>>,
    terminated: bool,
}

impl PortfuPeerTransport {
    fn new(socket: WebSocket) -> Self {
        Self {
            socket,
            read: None,
            write: None,
            close: None,
            terminated: false,
        }
    }

    fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), portfu::prelude::tokio_tungstenite::tungstenite::Error>> {
        let Some(future) = self.write.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => {
                self.write = None;
                Poll::Ready(
                    result.map_err(portfu::prelude::tokio_tungstenite::tungstenite::Error::Io),
                )
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Stream for PortfuPeerTransport {
    type Item = Result<Message, portfu::prelude::tokio_tungstenite::tungstenite::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.terminated {
            return Poll::Ready(None);
        }
        if self.read.is_none() {
            let socket = self.socket.clone();
            self.read = Some(Box::pin(async move { socket.next_message().await }));
        }
        let future = self.read.as_mut().expect("read future initialized");
        match future.as_mut().poll(cx) {
            Poll::Ready(Ok(Some(message))) => {
                self.read = None;
                Poll::Ready(Some(Ok(message)))
            }
            Poll::Ready(Ok(None)) => {
                self.read = None;
                self.terminated = true;
                Poll::Ready(None)
            }
            Poll::Ready(Err(error)) => {
                self.read = None;
                self.terminated = true;
                Poll::Ready(Some(Err(
                    portfu::prelude::tokio_tungstenite::tungstenite::Error::Io(error),
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl FusedStream for PortfuPeerTransport {
    fn is_terminated(&self) -> bool {
        self.terminated
    }
}

impl Sink<Message> for PortfuPeerTransport {
    type Error = portfu::prelude::tokio_tungstenite::tungstenite::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_write(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        if self.write.is_some() {
            return Err(Self::Error::Io(Error::other("websocket sink is not ready")));
        }
        let socket = self.socket.clone();
        self.write = Some(Box::pin(async move { socket.send(item).await }));
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_write(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.poll_write(cx) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        if self.close.is_none() {
            let socket = self.socket.clone();
            self.close = Some(Box::pin(async move { socket.close().await }));
        }
        let future = self.close.as_mut().expect("close future initialized");
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => {
                self.close = None;
                self.terminated = true;
                Poll::Ready(result.map_err(Self::Error::Io))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[websocket(
    "/ws",
    client_trust = "chia-peers",
    max_message_size = 67_108_864,
    max_frame_size = 67_108_864,
    upgrade_timeout_ms = 5_000
)]
pub async fn peer_socket(
    socket: WebSocket,
    connection: ConnectionInfo,
    services: State<NodeServices>,
) -> Result<(), PortfuError> {
    let identity = connection
        .client_identity
        .ok_or_else(|| PortfuError::Unauthorized("Chia client certificate required".to_string()))?;
    let peer_id = Bytes32::new(identity.sha256_fingerprint);
    services
        .0
        .peer_server
        .handle_stream(
            connection.peer_addr,
            peer_id,
            WebsocketMsgStream::Boxed(Box::new(PortfuPeerTransport::new(socket))),
            services.0.peer_run.clone(),
        )
        .await
        .map_err(|error| PortfuError::Internal(format!("peer websocket failed: {error}")))
}
