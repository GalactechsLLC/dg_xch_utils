pub mod config;
pub mod metrics;
pub mod probe;
mod routes;
pub mod state;
use config::Config;
use dg_xch_servers::discovery::{Policy, Role, Store};
use probe::Identity;
use state::{Summary, now};
use std::{
    io::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinSet,
};
pub struct App {
    pub config: Config,
    pub store: Store,
    summary_cache: Mutex<Option<(Instant, Summary)>>,
    seed_error: Mutex<Option<String>>,
    worker_tick: AtomicU64,
    started: Instant,
}
impl App {
    pub async fn connect(config: Config) -> Result<Arc<Self>, Error> {
        config.validate()?;
        let url = config
            .database_url
            .as_deref()
            .ok_or_else(|| Error::other("crawler requires PostgreSQL"))?;
        let store = Store::connect(url, config.network.clone()).await?;
        store
            .configure(
                Role::Crawler,
                Policy {
                    concurrency: config.concurrency,
                    interval_ms: config.interval_ms,
                    timeout_seconds: config.timeout_seconds,
                    revisit_seconds: config.revisit_seconds,
                    cooldown_seconds: 600,
                    allow_private: config.allow_private_addresses,
                },
            )
            .await?;
        Ok(Arc::new(Self {
            config,
            store,
            summary_cache: Mutex::new(None),
            seed_error: Mutex::new(None),
            worker_tick: AtomicU64::new(0),
            started: Instant::now(),
        }))
    }
    pub async fn summary(&self) -> Result<Summary, Error> {
        // Cache only small aggregates, never the peer catalog. Serialize refreshes per replica.
        let mut cache = self.summary_cache.lock().await;
        if let Some((at, value)) = &*cache
            && at.elapsed() < Duration::from_secs(5)
        {
            return Ok(value.clone());
        }
        let mut value = self.store.summary(self.config.recent_seconds).await?;
        value.seed_error = self.seed_error.lock().await.clone();
        *cache = Some((Instant::now(), value.clone()));
        Ok(value)
    }
    pub fn live(&self) -> bool {
        self.started
            .elapsed()
            .as_secs()
            .saturating_sub(self.worker_tick.load(Ordering::Relaxed))
            < 90
    }
    fn storage_failure(&self, operation: &str, error: &Error) {
        eprintln!(
            "{}",
            serde_json::json!({"event":"crawler_storage_error","timestamp":now(),"network":self.config.network,"operation":operation,"error":error.to_string()})
        );
    }
}
fn log_probe(config: &Config, endpoint: std::net::SocketAddr, obs: &probe::Observation) {
    use config::ProbeLog;
    if config.probe_log == ProbeLog::Off
        || (config.probe_log == ProbeLog::Errors && obs.error.is_none())
    {
        return;
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"crawler_probe","timestamp":now(),"network":config.network,"endpoint":endpoint,"family":if endpoint.is_ipv6(){"ipv6"}else{"ipv4"},"result":if obs.error.is_some(){"error"}else{"success"},"phase":obs.phase,"connected":obs.handshake.is_some(),"peer_response":obs.peer_request_ms.is_some(),"connect_ms":obs.connect_ms,"peer_request_ms":obs.peer_request_ms,"received_peers":obs.peers.len(),"error":obs.error.as_ref().map(|e|e.chars().take(256).collect::<String>())})
    );
}
async fn bootstrap(app: Arc<App>) {
    let mut errors = Vec::new();
    for seed in app.config.seeds() {
        match tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host(&seed)).await {
            Ok(Ok(addresses)) => {
                for endpoint in addresses.take(256) {
                    if config::admissible(endpoint, app.config.allow_private_addresses)
                        && let Err(error) = app
                            .store
                            .seen(endpoint, &format!("seed:{seed}"), None)
                            .await
                    {
                        errors.push(error.to_string());
                    }
                }
            }
            Ok(Err(error)) => errors.push(format!("{seed}: {error}")),
            Err(_) => errors.push(format!("{seed}: DNS lookup timed out")),
        }
    }
    *app.seed_error.lock().await = (!errors.is_empty()).then(|| errors.join("; "));
}
async fn crawl(app: Arc<App>, identity: Identity, mut stop: watch::Receiver<bool>) {
    let mut probes = JoinSet::new();
    let mut seeds = None;
    let mut poll = tokio::time::interval(Duration::from_millis(app.config.interval_ms));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut maintenance = tokio::time::interval(Duration::from_secs(30));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut bootstrap_clock = tokio::time::interval(Duration::from_secs(3600));
    loop {
        app.worker_tick
            .store(app.started.elapsed().as_secs(), Ordering::Relaxed);
        tokio::select! {
            _=stop.changed()=>break,
            _=bootstrap_clock.tick()=>{if seeds.as_ref().is_none_or(|t:&tokio::task::JoinHandle<()>|t.is_finished()){seeds=Some(tokio::spawn(bootstrap(app.clone())));}},
            _=poll.tick()=>if probes.len()<app.config.concurrency{
                match app.store.claim(Role::Crawler).await{
                    Ok(Some(claim))=>{let app=app.clone();let identity=identity.clone();probes.spawn(async move{
                        let obs=probe::probe(claim.endpoint,&app.config.network,&identity,Duration::from_secs(claim.policy.timeout_seconds)).await;
                        log_probe(&app.config,claim.endpoint,&obs);
                        if let Err(error)=app.store.finish(claim,obs,None).await{app.storage_failure("completion",&error);}
                    });},Ok(None)=>{},Err(error)=>app.storage_failure("claim",&error),
                }
            },
            Some(result)=probes.join_next(),if !probes.is_empty()=>{if let Err(error)=result{eprintln!("crawler probe task failed: {error}");}},
            _=maintenance.tick()=>{
                let result=async{app.store.maintenance(Role::Crawler,app.config.retention_days).await?;let summary=app.summary().await?;app.store.sample(&summary).await?;eprintln!("{}",serde_json::json!({"event":"crawler_summary","data":summary}));Ok::<_,Error>(())}.await;
                if let Err(error)=result{app.storage_failure("maintenance",&error);}
            },
        }
    }
    if let Some(task) = seeds {
        task.abort();
        let _ = task.await;
    }
    while let Some(result) = probes.join_next().await {
        if let Err(error) = result {
            eprintln!("crawler probe task failed: {error}");
        }
    }
}
pub async fn run(config: Config) -> Result<(), Error> {
    let app = App::connect(config).await?;
    let identity = tokio::task::spawn_blocking(Identity::generate)
        .await
        .map_err(Error::other)??;
    let server = portfu::prelude::ServerBuilder::new()
        .host(app.config.listen.ip().to_string())
        .port(app.config.listen.port())
        .global_state::<App>(app.clone())
        .service(routes::service(app.clone()))
        .build();
    eprintln!(
        "Chia crawler ({}) dashboard: http://{}/ ; storage: PostgreSQL",
        app.config.network, app.config.listen
    );
    let (stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(crawl(app, identity, stop_rx));
    let result = server.run().await;
    let _ = stop_tx.send(true);
    task.await.map_err(Error::other)?;
    result.map_err(|e| Error::other(e.to_string()))
}
