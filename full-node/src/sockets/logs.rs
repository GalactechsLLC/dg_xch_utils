use dg_logger::DruidGardenLogger;
use log::{Level, debug};
use portfu::prelude::{Message, Path, PortfuError, State, WebSocket, websocket};
use std::str::FromStr;

/// Stream log events at or above `{level}` as JSON, one message per event.
#[websocket("/ws/logs/{level}", client_trust = "rpc-clients")]
pub async fn log_stream(
    socket: WebSocket,
    level: Path,
    logger: State<DruidGardenLogger>,
) -> Result<(), PortfuError> {
    let level = level.inner();
    let level = Level::from_str(level.as_str())
        .map_err(|e| PortfuError::Parsing(format!("{level} is not a valid log level: {e:?}")))?;
    let mut bus = logger.0.subscribe();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<dg_logger::LogEvent>(256);
    std::thread::spawn(move || {
        while let Ok(event) = bus.recv() {
            if tx.blocking_send(event).is_err() {
                break;
            }
        }
    });
    loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break };
                if event.level <= level {
                    let json = serde_json::to_string(&event)
                        .map_err(|e| PortfuError::Internal(format!("serialize log event: {e}")))?;
                    if socket.send_text(json).await.is_err() {
                        break;
                    }
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
    debug!("log stream closed");
    Ok(())
}
