use crate::backend::{CpuBackend, VdfBackend};
use crate::scheduler::{Output, Scheduler, UnfinishedDisposition, WorkPlan, WorkResult};
use crate::service::{Config, Socket, connect};
use dg_xch_core::protocols::timelord::{
    NewGenesisTimelord, NewPeakTimelord, NewUnfinishedBlockTimelord,
};
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
) -> Result<(WorkPlan, WorkResult, Duration), Error> {
    let started = Instant::now();
    let duplicate_reward = plan.reward == plan.challenge;
    let chains = 1 + usize::from(!duplicate_reward) + usize::from(plan.infused.is_some());
    let threads = std::thread::available_parallelism()
        .map_or(1, |value| value.get())
        .min(4)
        / chains;
    let threads = threads.max(1);
    let challenge = backend.prove_with_threads(plan.challenge.clone(), threads);
    let reward = async {
        if duplicate_reward {
            Ok(None)
        } else {
            backend
                .prove_with_threads(plan.reward.clone(), threads)
                .await
                .map(Some)
        }
    };
    let infused = async {
        match &plan.infused {
            Some(request) => backend
                .prove_with_threads(request.clone(), threads)
                .await
                .map(Some),
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
        started.elapsed(),
    ))
}

async fn send(socket: &mut Socket, message: Message) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(5), socket.send(message))
        .await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "timelord send timed out"))?
        .map_err(Error::other)
}

async fn send_output(socket: &mut Socket, output: Output) -> Result<(), Error> {
    let (message, context) = match output {
        Output::SignagePoint(point) => (
            encode(ProtocolMessageTypes::NewSignagePointVdf, &point, None)?,
            format!(
                "kind=signage_point sp_index={} challenge={} cc_iterations={} rc_iterations={}",
                point.index_from_challenge,
                point.challenge_chain_sp_vdf.challenge,
                point.challenge_chain_sp_vdf.number_of_iterations,
                point.reward_chain_sp_vdf.number_of_iterations
            ),
        ),
        Output::InfusionPoint(point) => (
            encode(ProtocolMessageTypes::NewInfusionPointVdf, &point, None)?,
            format!(
                "kind=infusion_point unfinished_hash={} cc_iterations={} rc_iterations={}",
                point.unfinished_reward_hash,
                point.challenge_chain_ip_vdf.number_of_iterations,
                point.reward_chain_ip_vdf.number_of_iterations
            ),
        ),
        Output::EndOfSubSlot(slot) => (
            encode(ProtocolMessageTypes::NewEndOfSubSlotVdf, &slot, None)?,
            format!(
                "kind=end_of_sub_slot challenge={}",
                slot.end_of_sub_slot_bundle.challenge_chain.hash()?
            ),
        ),
    };
    eprintln!("timelord event=output_ready {context}");
    let started = Instant::now();
    send(socket, message).await?;
    eprintln!(
        "timelord event=output_sent {context} send_ms={}",
        started.elapsed().as_millis()
    );
    Ok(())
}

async fn session(
    config: &Config,
    tls: Arc<rustls::ClientConfig>,
    metrics: Arc<crate::monitoring::State>,
) -> Result<(), Error> {
    let chain = config.chain.resolve().map_err(Error::other)?;
    let mut scheduler = Scheduler::new(
        chain.constants,
        chain.allows_bootstrap,
        config.max_iterations_per_second,
    )?;
    metrics
        .attempts
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut socket = connect(config, tls).await?;
    let _connection = metrics.connect();
    let backend: Arc<dyn VdfBackend> = Arc::new(CpuBackend::new(
        Duration::from_secs(config.job_timeout_seconds),
        config.worker_memory_bytes,
    ));
    let mut workers: JoinSet<Result<(WorkPlan, WorkResult, Duration), Error>> = JoinSet::new();
    let mut active: Option<WorkPlan> = None;
    let mut active_started: Option<Instant> = None;
    let mut superseded_logged = false;
    let mut ready: Option<(WorkPlan, WorkResult)> = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut clock = tokio::time::interval(Duration::from_millis(50));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let connected = Instant::now();
    let mut last_message = Instant::now();
    eprintln!(
        "timelord event=connected node={}:{} state=awaiting_chain_state",
        config.fullnode_host, config.fullnode_port
    );
    loop {
        if scheduler.needs_resync() {
            return Err(Error::new(
                ErrorKind::TimedOut,
                "infusion was not confirmed; reconnecting for authoritative chain state",
            ));
        }
        let next = scheduler.next_plan()?;
        if active.as_ref().is_some_and(|current| {
            next.as_ref()
                .is_none_or(|next| current.generation != next.generation)
        }) {
            if let Some(plan) = &active {
                eprintln!(
                    "timelord event=work_cancelled generation={} kind={:?} total_iters={} elapsed_ms={} reason=chain_generation_changed",
                    plan.generation,
                    plan.event,
                    plan.total_iters,
                    active_started.map_or(0, |started| started.elapsed().as_millis())
                );
            }
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            active = None;
            active_started = None;
            ready = None;
        }
        // A late candidate can change the next event without changing the chain origin.
        // Finish the incremental step already running and retain its history, then prove the
        // earlier target from checkpoints. Killing it here would throw away the whole cache.
        if !superseded_logged
            && let (Some(current), Some(next)) = (&active, &next)
            && (current.total_iters != next.total_iters || current.event != next.event)
        {
            eprintln!(
                "timelord event=work_superseded generation={} current={:?} next={:?} preserve_history=true",
                current.generation, current.event, next.event
            );
            superseded_logged = true;
        }
        if active.is_none()
            && let Some(plan) = next.as_ref()
        {
            let plan = plan.clone();
            eprintln!(
                "timelord event=work_started generation={} kind={:?} total_iters={} challenge={} cc_iterations={} rc_iterations={} shared_cc_rc={} icc_iterations={}",
                plan.generation,
                plan.event,
                plan.total_iters,
                plan.challenge.challenge,
                plan.challenge.iterations,
                plan.reward.iterations,
                plan.challenge == plan.reward,
                plan.infused
                    .as_ref()
                    .map_or(0, |request| request.iterations)
            );
            superseded_logged = false;
            active_started = Some(Instant::now());
            active = Some(plan.clone());
            let backend = backend.clone();
            let job = metrics.job();
            workers.spawn(async move {
                let _job = job;
                execute(backend, plan).await
            });
        }
        if ready.as_ref().is_some_and(|(plan, _)| {
            Instant::now() >= plan.not_before
                || next.as_ref().is_none_or(|next| {
                    plan.total_iters != next.total_iters
                        || plan.event != next.event
                        || plan.generation != next.generation
                })
        }) && let Some((plan, result)) = ready.take()
        {
            if let Some(output) = scheduler.finish(plan, result)? {
                send_output(&mut socket, output).await?;
                metrics
                    .outputs
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                eprintln!("timelord event=work_discarded reason=superseded preserve_history=true");
            }
            active = None;
            active_started = None;
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
                if let Some(plan) = &active {
                    eprintln!("timelord event=work_status generation={} kind={:?} total_iters={} elapsed_seconds={} state={}", plan.generation, plan.event, plan.total_iters, active_started.map_or(0, |started| started.elapsed().as_secs()), if ready.is_some() { "waiting_to_send" } else { "computing" });
                }
                send(&mut socket, Message::Ping(Vec::new().into())).await?;
            }
            Some(completed) = workers.join_next(), if !workers.is_empty() => {
                match completed {
                    Ok(Ok((plan, result, elapsed))) if plan.generation == scheduler.generation() => {
                        eprintln!("timelord event=work_completed generation={} kind={:?} total_iters={} elapsed_ms={} effective_cc_iterations_per_second={:.0}", plan.generation, plan.event, plan.total_iters, elapsed.as_millis(), plan.challenge.iterations as f64 / elapsed.as_secs_f64().max(f64::EPSILON));
                        metrics.completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        ready = Some((plan, result));
                    },
                    Ok(Ok(_)) => {},
                    Ok(Err(error)) => { eprintln!("timelord event=work_failed error={error}"); metrics.failed.fetch_add(1,std::sync::atomic::Ordering::Relaxed); return Err(error); },
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
                            ProtocolMessageTypes::NewGenesisTimelord => {
                                let genesis: NewGenesisTimelord = decode_exact(message.data.as_slice())?;
                                eprintln!("timelord event=genesis_received challenge={} difficulty={} sub_slot_iters={}", genesis.genesis_challenge, genesis.difficulty, genesis.sub_slot_iters);
                                let changed = scheduler.set_genesis(genesis)?;
                                eprintln!("timelord event=chain_state_applied generation={} changed={changed}", scheduler.generation());
                            }
                            ProtocolMessageTypes::NewPeakTimelord => {
                                let peak: NewPeakTimelord = decode_exact(message.data.as_slice())?;
                                eprintln!("timelord event=peak_received height={} total_iters={} difficulty={} sub_slot_iters={}", peak.reward_chain_block.height, peak.reward_chain_block.total_iters, peak.difficulty, peak.sub_slot_iters);
                                let changed = scheduler.set_peak(peak)?;
                                eprintln!("timelord event=chain_state_applied generation={} changed={changed}", scheduler.generation());
                            }
                            ProtocolMessageTypes::NewUnfinishedBlockTimelord => {
                                let unfinished: NewUnfinishedBlockTimelord = decode_exact(message.data.as_slice())?;
                                let hash = unfinished.reward_chain_block.hash()?;
                                eprintln!("timelord event=unfinished_block_received unfinished_hash={hash} sp_index={} total_iters={}", unfinished.reward_chain_block.signage_point_index, unfinished.reward_chain_block.total_iters);
                                match scheduler.add_unfinished_with_disposition(unfinished) {
                                    Ok(UnfinishedDisposition::Accepted) => eprintln!("timelord event=unfinished_block_accepted unfinished_hash={hash}"),
                                    Ok(UnfinishedDisposition::Ignored(reason)) => eprintln!("timelord event=unfinished_block_ignored unfinished_hash={hash} reason={reason}"),
                                    Err(error) => eprintln!("timelord event=unfinished_block_rejected unfinished_hash={hash} error={error}"),
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
    let metrics = Arc::new(crate::monitoring::State::default());
    let snapshot = metrics.snapshot("regular");
    let listen = if config.disable_metrics {
        None
    } else {
        Some(
            config
                .metrics_listen
                .unwrap_or_else(|| std::net::SocketAddr::from(([0, 0, 0, 0], 8582))),
        )
    };
    dg_xch_servers::observability::run_with(async {
    loop {
        tokio::select! {
            result = shutdown_signal() => return result,
            result = session(&config, tls.clone(), metrics.clone()) => {
                if let Err(error) = result { eprintln!("timelord event=disconnected error={error}"); }
            }
        }
        tokio::select! {
            result = shutdown_signal() => return result,
            _ = tokio::time::sleep(Duration::from_secs(config.reconnect_seconds)) => {},
        }
    }
    }, listen, snapshot).await
}
