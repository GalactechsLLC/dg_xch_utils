use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::unsized_bytes::UnsizedBytes;
use dg_xch_core::protocols::{
    ChiaMessage, ProtocolMessageTypes, WebsocketConnection, WebsocketMsgStream,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Message, protocol::Role};

struct HeartbeatLogger(AtomicUsize);

impl log::Log for HeartbeatLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let message = record.args().to_string();
        if message.starts_with("Invalid Message:")
            && (message.contains("Ping(") || message.contains("Pong("))
        {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) {}
}

static LOGGER: HeartbeatLogger = HeartbeatLogger(AtomicUsize::new(0));

#[tokio::test]
async fn heartbeats_reply_and_preserve_protocol_delivery_without_errors() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Error);
    tokio::time::timeout(Duration::from_secs(3), async {
        let (server_io, client_io) = tokio::io::duplex(4096);
        let server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let mut client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let (connection, mut reader) = WebsocketConnection::new(
            WebsocketMsgStream::Boxed(Box::new(server.fuse())),
            Arc::new(RwLock::new(HashMap::new())),
            Arc::new(Bytes32::from([1; 32])),
            Arc::new(RwLock::new(HashMap::new())),
            None,
            Arc::from("heartbeat test"),
        );
        let (id, response) = connection.register_request().unwrap();
        let reading = tokio::spawn(async move {
            reader.run(Arc::new(AtomicBool::new(true))).await;
        });

        for payload in [Vec::new(), b"timelord heartbeat".to_vec()] {
            client
                .send(Message::Ping(payload.clone().into()))
                .await
                .unwrap();
            assert_eq!(
                client.next().await.unwrap().unwrap(),
                Message::Pong(payload.into()),
                "the transport must echo the ping payload"
            );
        }
        client
            .send(Message::Pong(b"unsolicited pong".to_vec().into()))
            .await
            .unwrap();
        let reply = ChiaMessage {
            msg_type: ProtocolMessageTypes::RespondBlocks,
            id: Some(id),
            data: UnsizedBytes::new(Vec::new()),
        };
        client
            .send(Message::Binary(
                reply
                    .to_bytes(ChiaProtocolVersion::default())
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        assert_eq!(response.await.unwrap().id, Some(id));
        client.close(None).await.unwrap();
        reading.await.unwrap();
        assert_eq!(LOGGER.0.load(Ordering::Relaxed), 0);
    })
    .await
    .expect("heartbeats and subsequent protocol messages must not stall");
}
