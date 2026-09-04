use crate::server::ActiveNode;
use portfu::prelude::{Message, PortfuError, State, WebSocket, websocket};

#[websocket("/ws/status")]
pub async fn status_stream(
    socket: WebSocket,
    active: State<ActiveNode>,
) -> Result<(), PortfuError> {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if socket.send_text(active.0.status_json().await).await.is_err() {
                    break;
                }
            }
            msg = socket.next_message() => {
                match msg {
                    Ok(Some(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    Ok(Some(Message::Close(_))) | Ok(None) | Err(_) => break,
                    Ok(Some(_)) => {}
                }
            }
        }
    }
    Ok(())
}
