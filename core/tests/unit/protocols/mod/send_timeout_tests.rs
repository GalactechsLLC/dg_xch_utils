use super::{SEND_TIMEOUT, timeout_send};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

// A ready sink (a peer draining normally): the write completes and round-trips Ok.
#[tokio::test]
async fn send_round_trips_on_a_ready_sink() {
    let mut sink = futures_util::sink::drain::<Message>();
    let msg = Message::Binary(vec![1, 2, 3].into());
    let out = timeout_send(&mut sink, msg, SEND_TIMEOUT).await;
    assert!(out.is_ok(), "a draining sink must accept the write");
}

// A never-ready sink models a peer whose TCP receive window is full — the exact backpressure that
// used to wedge the sender under the connection write lock. The bounded write must resolve to a
// timeout error, never hang.
#[tokio::test]
async fn send_times_out_on_a_stalled_sink() {
    struct StalledSink;
    impl futures_util::Sink<Message> for StalledSink {
        type Error = std::io::Error;
        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
    }
    let mut sink = StalledSink;
    let msg = Message::Binary(vec![].into());
    let out = timeout_send(&mut sink, msg, Duration::from_millis(50)).await;
    assert!(
        out.is_err(),
        "a stalled sink must time out, not hang the sender"
    );
}
