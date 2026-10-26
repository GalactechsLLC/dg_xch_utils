use crate::server::NodeServices;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::WebsocketMsgStream;
use dg_xch_core::traits::SizedBytes;
use futures_util::stream::FusedStream;
use futures_util::{Sink, Stream};
use portfu::prelude::tokio_tungstenite::tungstenite as legacy;
use portfu::prelude::{ConnectionInfo, PortfuError, State, WebSocket, websocket};
use std::future::Future;
use std::io::Error;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_tungstenite::tungstenite::{self as current, Message};

// Portfu's pinned transport and the protocol core use different tungstenite
// releases. Move shared Bytes payloads across the boundary without copying, and
// retain raw frame headers (including fragmentation, extension bits and masks).
macro_rules! message_bridge {
    ($name:ident, $from:ident, $to:ident) => {
        fn $name(message: $from::Message) -> $to::Message {
            use $from::protocol::frame::coding as source;
            use $to::protocol::frame::coding as target;
            match message {
                $from::Message::Text(text) => {
                    let bytes: $from::Bytes = text.into();
                    $to::Message::Text(bytes.try_into().expect("validated websocket UTF-8"))
                }
                $from::Message::Binary(bytes) => $to::Message::Binary(bytes),
                $from::Message::Ping(bytes) => $to::Message::Ping(bytes),
                $from::Message::Pong(bytes) => $to::Message::Pong(bytes),
                $from::Message::Close(frame) => $to::Message::Close(frame.map(|frame| {
                    let bytes: $from::Bytes = frame.reason.into();
                    $to::protocol::CloseFrame {
                        code: u16::from(frame.code).into(),
                        reason: bytes.try_into().expect("validated websocket close UTF-8"),
                    }
                })),
                $from::Message::Frame(frame) => {
                    let header = frame.header();
                    let opcode = match header.opcode {
                        source::OpCode::Data(data) => target::OpCode::Data(match data {
                            source::Data::Continue => target::Data::Continue,
                            source::Data::Text => target::Data::Text,
                            source::Data::Binary => target::Data::Binary,
                            source::Data::Reserved(code) => target::Data::Reserved(code),
                        }),
                        source::OpCode::Control(control) => {
                            target::OpCode::Control(match control {
                                source::Control::Close => target::Control::Close,
                                source::Control::Ping => target::Control::Ping,
                                source::Control::Pong => target::Control::Pong,
                                source::Control::Reserved(code) => target::Control::Reserved(code),
                            })
                        }
                    };
                    let header = $to::protocol::frame::FrameHeader {
                        is_final: header.is_final,
                        rsv1: header.rsv1,
                        rsv2: header.rsv2,
                        rsv3: header.rsv3,
                        opcode,
                        mask: header.mask,
                    };
                    $to::Message::Frame($to::protocol::frame::Frame::from_payload(
                        header,
                        frame.into_payload(),
                    ))
                }
            }
        }
    };
}

message_bridge!(from_portfu, legacy, current);
message_bridge!(to_portfu, current, legacy);

type IoFuture<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;

/// Adapts Portfu's concurrently usable websocket handle to the stream/sink shape used by the
/// protocol core. Only one read and one write future are active at a time, matching Sink's
/// contract while retaining Portfu's independent read/write halves.
struct PortfuPeerTransport {
    socket: WebSocket,
    read: Option<IoFuture<Option<legacy::Message>>>,
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

    fn poll_write(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), current::Error>> {
        let Some(future) = self.write.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => {
                self.write = None;
                Poll::Ready(result.map_err(current::Error::Io))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Stream for PortfuPeerTransport {
    type Item = Result<Message, current::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.terminated {
            return Poll::Ready(None);
        }
        if self.read.is_none() {
            let socket = self.socket.clone();
            self.read = Some(Box::pin(async move { socket.next_message().await }));
        }
        let Some(future) = self.read.as_mut() else {
            return Poll::Ready(Some(Err(current::Error::Io(Error::other(
                "websocket read future unavailable",
            )))));
        };
        match future.as_mut().poll(cx) {
            Poll::Ready(Ok(Some(message))) => {
                self.read = None;
                Poll::Ready(Some(Ok(from_portfu(message))))
            }
            Poll::Ready(Ok(None)) => {
                self.read = None;
                self.terminated = true;
                Poll::Ready(None)
            }
            Poll::Ready(Err(error)) => {
                self.read = None;
                self.terminated = true;
                Poll::Ready(Some(Err(current::Error::Io(error))))
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
    type Error = current::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_write(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        if self.write.is_some() {
            return Err(Self::Error::Io(Error::other("websocket sink is not ready")));
        }
        let socket = self.socket.clone();
        self.write = Some(Box::pin(async move { socket.send(to_portfu(item)).await }));
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
        let Some(future) = self.close.as_mut() else {
            return Poll::Ready(Err(Self::Error::Io(Error::other(
                "websocket close future unavailable",
            ))));
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_bridge_preserves_payloads_and_close_codes() {
        for message in [
            Message::Text("peer text 🛰".into()),
            Message::Binary(vec![0, 255, 1].into()),
            Message::Ping(vec![2, 3].into()),
            Message::Pong(vec![4, 5].into()),
            Message::Close(None),
            Message::Close(Some(current::protocol::CloseFrame {
                code: 4001.into(),
                reason: "peer shutdown 🛰".into(),
            })),
        ] {
            let expected = message.clone();
            let original = match &message {
                Message::Binary(bytes) | Message::Ping(bytes) | Message::Pong(bytes) => {
                    Some(bytes.as_ptr())
                }
                _ => None,
            };
            let converted = to_portfu(message);
            if let Some(original) = original {
                assert_eq!(converted.clone().into_data().as_ptr(), original);
            }
            let roundtrip = from_portfu(converted);
            if let Some(original) = original {
                assert_eq!(roundtrip.clone().into_data().as_ptr(), original);
            }
            assert_eq!(roundtrip, expected);
        }
        for code in [0, 999, 1000, 1005, 1015, 2000, 3000, 4000, 5000, u16::MAX] {
            let message = Message::Close(Some(current::protocol::CloseFrame {
                code: code.into(),
                reason: "close reason".into(),
            }));
            assert_eq!(from_portfu(to_portfu(message.clone())), message);
        }
    }

    #[test]
    fn raw_frame_bridge_preserves_headers_and_payload_allocation() {
        use current::protocol::frame::{Frame, FrameHeader, coding::OpCode};
        for opcode in 0..=15 {
            for mask in [None, Some([1, 2, 3, 4])] {
                let header = FrameHeader {
                    is_final: false,
                    rsv1: true,
                    rsv2: true,
                    rsv3: true,
                    opcode: OpCode::from(opcode),
                    mask,
                };
                let payload: current::Bytes = vec![0, 255, 3, 4].into();
                let pointer = payload.as_ptr();
                let frame = Frame::from_payload(header.clone(), payload);
                let legacy::Message::Frame(converted) = to_portfu(Message::Frame(frame)) else {
                    panic!("raw frame must remain a raw frame");
                };
                assert_eq!(converted.payload().as_ptr(), pointer);
                let Message::Frame(roundtrip) = from_portfu(legacy::Message::Frame(converted))
                else {
                    panic!("raw frame must remain a raw frame");
                };
                assert_eq!(roundtrip.header(), &header);
                assert_eq!(roundtrip.payload(), &[0, 255, 3, 4]);
                assert_eq!(roundtrip.payload().as_ptr(), pointer);
            }
        }
    }
}
