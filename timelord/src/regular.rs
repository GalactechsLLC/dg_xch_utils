use crate::backend::{CpuBackend, VdfBackend};
use crate::scheduler::{Output, Scheduler, WorkPlan, WorkResult};
use crate::service::{Config, Socket, connect};
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_servers::transport::{decode_exact, encode, shutdown_signal};
use futures_util::{SinkExt, StreamExt};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;

async fn execute(
    backend: Arc<dyn VdfBackend>,
    plan: WorkPlan,
) -> Result<(WorkPlan, WorkResult), Error> {
    let duplicate_reward = plan.reward == plan.challenge;
    let challenge = backend.prove(plan.challenge.clone());
    let reward = async {
        if duplicate_reward {
            Ok(None)
        } else {
            backend.prove(plan.reward.clone()).await.map(Some)
        }
    };
    let infused = async {
        match &plan.infused {
            Some(request) => backend.prove(request.clone()).await.map(Some),
            None => Ok(None),
        }
    };
    let (challenge, reward, infused) = tokio::try_join!(challenge, reward, infused)?;
    let reward = reward.unwrap_or_else(|| challenge.clone());
    Ok((
        plan,
        WorkResult {
            challenge,
            reward,
            infused,
        },
    ))
}

async fn send(socket: &mut Socket, message: Message) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "timelord send timed out"))?
        .map_err(Error::other)
}

async fn send_output(socket: &mut Socket, output: Output) -> Result<(), Error> {
    let message = match output {
        Output::SignagePoint(point) => {
            encode(ProtocolMessageTypes::NewSignagePointVdf, &point, None)?
        }
        Output::InfusionPoint(point) => {
            eprintln!("Submitting infusion point {}", point.unfinished_reward_hash);
            encode(ProtocolMessageTypes::NewInfusionPointVdf, &point, None)?
        }
        Output::EndOfSubSlot(slot) => {
            eprintln!(
                "Submitting end of sub-slot {}",
                slot.end_of_sub_slot_bundle.challenge_chain.hash()?
            );
            encode(ProtocolMessageTypes::NewEndOfSubSlotVdf, &slot, None)?
        }
    };
    send(socket, message).await
}

async fn session(config: &Config, tls: Arc<rustls::ClientConfig>) -> Result<(), Error> {
    let chain = config.chain.resolve().map_err(Error::other)?;
    let mut scheduler = Scheduler::new(
        chain.constants,
        chain.allows_bootstrap,
        config.max_iterations_per_second,
    )?;
    let mut socket = connect(config, tls).await?;
    let backend: Arc<dyn VdfBackend> = Arc::new(CpuBackend {
        timeout: Duration::from_secs(config.job_timeout_seconds),
        memory_bytes: config.worker_memory_bytes,
    });
    let mut workers: JoinSet<Result<(WorkPlan, WorkResult), Error>> = JoinSet::new();
    let mut active: Option<WorkPlan> = None;
    let mut ready: Option<(WorkPlan, WorkResult)> = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut clock = tokio::time::interval(Duration::from_millis(50));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let connected = Instant::now();
    let mut last_message = Instant::now();
    eprintln!("Authenticated full node; regular CPU timelord awaiting chain state");
    loop {
        if scheduler.needs_resync() {
            return Err(Error::new(
                ErrorKind::TimedOut,
                "infusion was not confirmed; reconnecting for authoritative chain state",
            ));
        }
        let next = scheduler.next_plan()?;
        if active.as_ref().is_some_and(|current| {
            next.as_ref().is_none_or(|next| {
                current.generation != next.generation
                    || current.total_iters != next.total_iters
                    || current.event != next.event
            })
        }) {
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            active = None;
            ready = None;
        }
        if active.is_none()
            && let Some(plan) = next
        {
            active = Some(plan.clone());
            workers.spawn(execute(backend.clone(), plan));
        }
        if ready
            .as_ref()
            .is_some_and(|(plan, _)| Instant::now() >= plan.not_before)
            && let Some((plan, result)) = ready.take()
        {
            if let Some(output) = scheduler.finish(plan, result)? {
                send_output(&mut socket, output).await?;
            }
            active = None;
            continue;
        }
        tokio::select! {
            _ = clock.tick() => {
                if !scheduler.initialized() && connected.elapsed() > Duration::from_secs(30) {
                    return Err(Error::new(ErrorKind::TimedOut, "full node has not authorized chain work; reconnecting"));
                }
            }
            _ = heartbeat.tick() => {
                if last_message.elapsed() > Duration::from_secs(90) {
                    return Err(Error::new(ErrorKind::TimedOut, "full node heartbeat expired"));
                }
                send(&mut socket, Message::Ping(Vec::new().into())).await?;
            }
            Some(completed) = workers.join_next(), if !workers.is_empty() => {
                match completed {
                    Ok(Ok((plan, result))) if plan.generation == scheduler.generation() => ready = Some((plan, result)),
                    Ok(Ok(_)) => {},
                    Ok(Err(error)) => return Err(error),
                    Err(error) if error.is_cancelled() => {},
                    Err(error) => return Err(Error::other(error)),
                }
            }
            incoming = socket.next() => {
                last_message = Instant::now();
                let incoming = incoming.ok_or_else(|| Error::new(ErrorKind::ConnectionAborted, "full node disconnected"))?.map_err(Error::other)?;
                match incoming {
                    Message::Binary(bytes) => {
                        let message: ChiaMessage = decode_exact(&bytes)?;
                        match message.msg_type {
                            ProtocolMessageTypes::NewGenesisTimelord => { scheduler.set_genesis(decode_exact(message.data.as_slice())?)?; }
                            ProtocolMessageTypes::NewPeakTimelord => { scheduler.set_peak(decode_exact(message.data.as_slice())?)?; }
                            ProtocolMessageTypes::NewUnfinishedBlockTimelord => {
                                if let Err(error) = scheduler.add_unfinished(decode_exact(message.data.as_slice())?) {
                                    eprintln!("Rejected unfinished timelord work: {error}");
                                }
                            }
                            ProtocolMessageTypes::RequestCompactProofOfTime => {},
                            _ => return Err(Error::new(ErrorKind::InvalidData, "unexpected regular timelord message")),
                        }
                    }
                    Message::Ping(payload) => send(&mut socket, Message::Pong(payload)).await?,
                    Message::Pong(_) => {},
                    Message::Close(_) => return Err(Error::new(ErrorKind::ConnectionAborted, "full node closed connection")),
                    _ => return Err(Error::new(ErrorKind::InvalidData, "unexpected WebSocket message")),
                }
            }
        }
    }
}

pub async fn serve(config: Config) -> Result<(), Error> {
    config.validate()?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tls = config.tls.client()?;
    loop {
        tokio::select! {
            result = shutdown_signal() => return result,
            result = session(&config, tls.clone()) => {
                if let Err(error) = result { eprintln!("Regular timelord disconnected: {error}"); }
            }
        }
        tokio::select! {
            result = shutdown_signal() => return result,
            _ = tokio::time::sleep(Duration::from_secs(config.reconnect_seconds)) => {},
        }
    }
}
