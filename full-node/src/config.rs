use dg_xch_p2p::P2pSettings;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

// Which storage backend the `--db` URL selects. SQLite is the embedded Pi-floor backend (the only one built
// today); Postgres is the industrial scale-out backend, whose sqlx implementation is a dg_xch_stores concern
// not yet landed — a `postgres://` URL is accepted here but rejected at open with a clear error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Sqlite(PathBuf),
    Postgres(String),
    Mmap(PathBuf),
}

impl Backend {
    /// Parse a `--db` value: `sqlite://<path>` / `sqlite:<path>` → SQLite, `postgres://…` /
    /// `postgresql://…` → Postgres, a bare path → SQLite.
    #[must_use]
    pub fn parse(db: &str) -> Self {
        if let Some(rest) = db.strip_prefix("mmap://") {
            Backend::Mmap(PathBuf::from(rest))
        } else if let Some(rest) = db.strip_prefix("sqlite://") {
            Backend::Sqlite(PathBuf::from(rest))
        } else if let Some(rest) = db.strip_prefix("sqlite:") {
            Backend::Sqlite(PathBuf::from(rest))
        } else if db.starts_with("postgres://") || db.starts_with("postgresql://") {
            Backend::Postgres(db.to_string())
        } else {
            Backend::Sqlite(PathBuf::from(db))
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum RpcTlsMode {
    PrivateCa {
        ssl_dir: PathBuf,
    },
    #[default]
    Local,
}

impl RpcTlsMode {
    /// Parse `--rpc-tls`: `private-ca` or `local`.
    ///
    /// # Errors
    /// Returns an error string for an unrecognized mode.
    pub fn parse(mode: &str, ssl_dir: &str) -> Result<Self, String> {
        match mode.trim().to_ascii_lowercase().as_str() {
            "private" | "private-ca" => Ok(RpcTlsMode::PrivateCa {
                ssl_dir: PathBuf::from(ssl_dir),
            }),
            "" | "local" | "none" | "loopback" => Ok(RpcTlsMode::Local),
            other => Err(format!(
                "bad --rpc-tls {other:?} (expected `private-ca` or `local`)"
            )),
        }
    }
}

// The server's runtime configuration. The full node requires `listen == rpc` because Portfu owns
// one unified listener; `rpc` remains in this shared config for simulator compatibility.
#[derive(Clone, Debug)]
pub struct Config {
    pub performance: PerformanceConfig,
    pub listen: SocketAddr,
    pub rpc: SocketAddr,
    pub rpc_tls: RpcTlsMode,
    pub debug_endpoints: bool,
    pub introducer: Option<(String, u16)>,
    pub manual_peers: Vec<(String, u16)>,
    pub advertise: Option<SocketAddr>,
    pub backend: Backend,
    pub network_id: String,
    // Debug: directory to capture real sync data into for offline replay — the fetched weight proof
    // (`weight_proof_<tip_height>.bin`) and every downloaded block range (`blocks_<start>_<end>.bin`).
    // Lets the whole fast-sync pipeline be validated + profiled offline with no live peer.
    pub capture_dir: Option<PathBuf>,
    // `--genesis-sync`: validate the historical chain block by block from height 0. Disables the
    // weight-proof fast sync entirely — the long-sync acceptance path and the discovery
    // harness for old-regime divergences.
    pub genesis_sync: bool,
    // `--sync-from <height>`: anchor mid-chain and validate forward from there. 0 = off.
    // Composes the weight-proof summaries (whole-chain epoch schedule) with a headers-first pass
    // over a fetched span just below the anchor. Mutually exclusive with fast sync; independent
    // nodes can each take a disjoint chain segment.
    pub sync_from: u32,
    // `--uncompact`: enable the compact-VDF solicitation scan. OFF by default, matching the
    // network-wide `send_uncompact_interval: 0` default. With the flag on, the scan hands bulky
    // proofs to connected bluebox TIMELORDS via RequestCompactProofOfTime and consumes their
    // RespondCompactProofOfTime through the same validate/swap/re-gossip path as compact-VDF
    // gossip. With no timelord peer the scan runs and sends nothing.
    pub uncompact: bool,
    // `--prefetch-memory-mb <N>`: RAM budget (MiB) for resident sync-window block bodies.
    // `None` = 256 MiB. The full-node fetch width defaults to two requests per configured outbound
    // peer; `Some(N)` raises only the resident ceiling so a fetch-starved, large-RAM node can sustain
    // a deeper lookahead. At huge block sizes the byte budget collapses depth toward one window.
    pub prefetch_memory_mb: Option<u64>,
    // `--prefetch-max-inflight <N>`: optional cap on the AGGREGATE outstanding block-range requests
    // (the concurrency knob, a COUNT — distinct from the byte budget above). `None` = two requests
    // per configured outbound peer. Explicit values are spread across peers as `ceil(N / peers)`,
    // within the hard aggregate and per-peer ceilings.
    pub prefetch_max_inflight: Option<usize>,
    pub p2p: P2pSettings,
    // `--trusted-peer <node-id-hex>` (repeatable): the cert-hash node ids granted the trusted tier —
    // the `trusted_peers` map, keyed on `node_id.hex()`. A trusted peer gets the larger
    // subscription / response-item caps and high-priority transaction-queue placement.
    // Empty (the default) → no peer trusted by node id.
    pub trusted_peers: Vec<String>,
    pub trusted_cidrs: Vec<String>,
}

impl Config {
    /// Build a config from the parsed CLI strings.
    ///
    /// # Errors
    /// Returns an error string if `--listen`, `--rpc`, `--introducer`, `--peer`, or `--advertise` fail to
    /// parse.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        listen: &str,
        rpc: &str,
        introducer: Option<&str>,
        peers: &[String],
        advertise: Option<&str>,
        db: &str,
        network: &str,
        capture_dir: Option<&str>,
        genesis_sync: bool,
        sync_from: u32,
        uncompact: bool,
        prefetch_memory_mb: Option<u64>,
        prefetch_max_inflight: Option<usize>,
        p2p: P2pSettings,
        trusted_peers: &[String],
        trusted_cidrs: &[String],
    ) -> Result<Self, String> {
        let listen = SocketAddr::from_str(listen).map_err(|e| format!("bad --listen: {e}"))?;
        let rpc = SocketAddr::from_str(rpc).map_err(|e| format!("bad --rpc: {e}"))?;
        let introducer = introducer.map(parse_host_port).transpose()?;
        let manual_peers = peers
            .iter()
            .map(|p| parse_host_port(p))
            .collect::<Result<Vec<_>, _>>()?;
        let advertise = advertise
            .map(|a| SocketAddr::from_str(a).map_err(|e| format!("bad --advertise: {e}")))
            .transpose()?;
        p2p.validate()?;
        Ok(Self {
            performance: PerformanceConfig::default(),
            listen,
            rpc,
            introducer,
            manual_peers,
            advertise,
            backend: Backend::parse(db),
            network_id: network.to_string(),
            capture_dir: capture_dir.map(PathBuf::from),
            genesis_sync,
            sync_from,
            uncompact,
            prefetch_memory_mb,
            prefetch_max_inflight,
            p2p,
            trusted_peers: trusted_peers.to_vec(),
            trusted_cidrs: trusted_cidrs.to_vec(),
            rpc_tls: RpcTlsMode::default(),
            debug_endpoints: false,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PerformanceConfig {
    pub compute_workers: Option<usize>,
    pub validation_window_blocks: Option<u32>,
    pub validation_window_mb: u64,
    pub confirm_transaction_blocks: Option<usize>,
    pub sqlite_writer_cache_mb: Option<u64>,
    pub coalesce_coin_writes: bool,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            compute_workers: None,
            validation_window_blocks: None,
            validation_window_mb: 128,
            confirm_transaction_blocks: None,
            sqlite_writer_cache_mb: None,
            coalesce_coin_writes: false,
        }
    }
}

impl PerformanceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .compute_workers
            .is_some_and(|workers| workers == 0 || workers > 1024)
        {
            return Err("--compute-workers must be between 1 and 1024".into());
        }
        if self
            .validation_window_blocks
            .is_some_and(|blocks| !(1..=4096).contains(&blocks))
        {
            return Err("--validation-window-blocks must be between 1 and 4096".into());
        }
        if !(1..=65536).contains(&self.validation_window_mb) {
            return Err("--validation-window-mb must be between 1 and 65536".into());
        }
        if self
            .confirm_transaction_blocks
            .is_some_and(|blocks| !(1..=4096).contains(&blocks))
        {
            return Err("--confirm-transaction-blocks must be between 1 and 4096".into());
        }
        if self
            .sqlite_writer_cache_mb
            .is_some_and(|size| !(1..=1048576).contains(&size))
        {
            return Err("--sqlite-writer-cache-mb must be between 1 and 1048576".into());
        }
        Ok(())
    }
}

fn parse_host_port(s: &str) -> Result<(String, u16), String> {
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| format!("expected host:port, got {s}"))?;
    let port = port
        .parse::<u16>()
        .map_err(|e| format!("bad host:port port in {s}: {e}"))?;
    Ok((host.to_string(), port))
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
