use crate::chain::NodeChain;
use crate::service::PoolService;
use crate::store::PoolStore;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_core::protocols::pool::GetPoolInfoResponse;
use portfu::prelude::{ServerBuilder, TlsIdentity};
use serde::{Deserialize, Serialize};
use std::io::{Error, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    #[serde(default)]
    pub chain: ChainSelection,
    pub trusted_genesis_header_hash: Bytes32,
    pub listen: SocketAddr,
    pub database: PathBuf,
    pub name: String,
    pub description: String,
    pub target_puzzle_hash: Bytes32,
    pub relative_lock_height: u32,
    pub minimum_difficulty: u64,
    pub fee_basis_points: u16,
    pub authentication_token_timeout: u8,
    pub partial_time_limit: u32,
    pub max_concurrent_requests: usize,
    #[serde(default)]
    pub enable_experimental_v2: bool,
    pub pool_memoization: SerializedProgram,
    pub node_host: String,
    pub node_port: u16,
    pub node_tls: ClientSSLConfig,
    pub tls: Option<PoolTls>,
    #[serde(default)]
    pub payouts: Option<PayoutConfig>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayoutConfig {
    pub key_file: PathBuf,
    pub confirmations: u32,
    pub transaction_fee: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolTls {
    pub domain: String,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

pub fn read_bounded(path: &Path) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::other("pool configuration or TLS file exceeds 1 MiB"));
    }
    Ok(bytes)
}

impl PoolConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if self.minimum_difficulty == 0
            || self.fee_basis_points > 10_000
            || !(1..=60).contains(&self.authentication_token_timeout)
            || !(1..=600).contains(&self.partial_time_limit)
            || !(1..=256).contains(&self.max_concurrent_requests)
            || self.relative_lock_height == 0
            || self.relative_lock_height > 1000
            || self.listen.port() == 0
            || self.node_port == 0
            || self.name.is_empty()
            || self.name.len() > 128
            || self.description.len() > 4096
            || self.trusted_genesis_header_hash == Bytes32::default()
            || self.target_puzzle_hash == Bytes32::default()
        {
            return Err(Error::other(
                "invalid pool configuration limits or identity",
            ));
        }
        if self.tls.is_none() && !self.listen.ip().is_loopback() {
            return Err(Error::other("a non-loopback pool listener requires TLS"));
        }
        if self.pool_memoization.to_bytes().len() > 4096 {
            return Err(Error::other("pool memoization exceeds 4 KiB"));
        }
        self.pool_memoization.to_program()?;
        if self
            .payouts
            .as_ref()
            .is_some_and(|payouts| !(1..=1000).contains(&payouts.confirmations))
        {
            return Err(Error::other(
                "pool reward confirmations must be between 1 and 1000",
            ));
        }
        self.chain.constants().map_err(Error::other)?;
        Ok(())
    }
}

pub async fn serve(config: PoolConfig) -> Result<(), Error> {
    config.validate()?;
    let payout_key = config
        .payouts
        .as_ref()
        .map(|payouts| {
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            if std::fs::symlink_metadata(&payouts.key_file)?
                .file_type()
                .is_symlink()
            {
                return Err(Error::other("pool payout key must not be a symbolic link"));
            }
            let file = options.open(&payouts.key_file)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() > 128 {
                return Err(Error::other("pool payout key must be a small regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(Error::other(
                        "pool payout key must not be accessible to other users",
                    ));
                }
            }
            let mut encoded = zeroize::Zeroizing::new(Vec::new());
            file.take(129).read_to_end(&mut encoded)?;
            if encoded.len() > 128 {
                return Err(Error::other("pool payout key exceeds size limit"));
            }
            let encoded = std::str::from_utf8(&encoded)
                .map_err(|_| Error::other("invalid payout key encoding"))?;
            let bytes = zeroize::Zeroizing::new(
                hex::decode(encoded.trim())
                    .map_err(|_| Error::other("invalid payout key encoding"))?,
            );
            let key = blst::min_pk::SecretKey::from_bytes(&bytes)
                .map_err(|_| Error::other("invalid payout secret key"))?;
            if dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk(
                key.sk_to_pk().to_bytes().into(),
            )? != config.target_puzzle_hash
            {
                return Err(Error::other(
                    "pool payout key does not control the configured target puzzle hash",
                ));
            }
            Ok::<_, Error>(key)
        })
        .transpose()?;
    let constants = config.chain.constants().map_err(Error::other)?;
    let chain = Arc::new(NodeChain {
        client: Arc::new(FullnodeClient::new_verified(
            &config.node_host,
            config.node_port,
            10,
            Some(config.node_tls.clone()),
            &None,
        )?),
        constants,
        trusted_genesis: config.trusted_genesis_header_hash,
        target: config.target_puzzle_hash,
        relative_lock_height: config.relative_lock_height,
        pool_memoization: config.pool_memoization.clone(),
        partial_time_limit: config.partial_time_limit,
    });
    chain.check_network().await?;
    let mut store = PoolStore::open(
        &config.database,
        constants.genesis_challenge,
        config.target_puzzle_hash,
    )
    .await?;
    store
        .bind_configuration(
            &serde_json::to_string(&(
                config.trusted_genesis_header_hash,
                config.relative_lock_height,
                &config.pool_memoization,
                config.fee_basis_points,
                config
                    .payouts
                    .as_ref()
                    .map(|payouts| payouts.transaction_fee),
            ))
            .map_err(Error::other)?,
        )
        .await?;
    let pool = Arc::new(PoolService {
        info: GetPoolInfoResponse {
            name: config.name,
            description: config.description,
            logo_url: String::new(),
            minimum_difficulty: config.minimum_difficulty,
            relative_lock_height: config.relative_lock_height,
            protocol_version: 1,
            fee: format!(
                "{}.{:04}",
                config.fee_basis_points / 10_000,
                config.fee_basis_points % 10_000
            ),
            target_puzzle_hash: config.target_puzzle_hash,
            authentication_token_timeout: config.authentication_token_timeout,
        },
        fee_basis_points: config.fee_basis_points,
        pool_memoization: hex::encode(config.pool_memoization.to_bytes()),
        enable_v2: config.enable_experimental_v2,
        store: Mutex::new(store),
        chain: chain.clone(),
        requests: Semaphore::new(config.max_concurrent_requests),
    });
    let worker = match (config.payouts, payout_key) {
        (Some(payouts), Some(key)) => Some(crate::worker::RewardWorker {
            pool: pool.clone(),
            chain,
            key,
            confirmations: payouts.confirmations,
            transaction_fee: payouts.transaction_fee,
        }),
        _ => None,
    };
    let mut server = ServerBuilder::new()
        .host(config.listen.ip().to_string())
        .port(config.listen.port())
        .http_header_read_timeout(Duration::from_secs(10))
        .service(crate::http::service(pool));
    if let Some(tls) = config.tls {
        server = server.tls_identity(TlsIdentity::new(
            tls.domain,
            read_bounded(&tls.certificate)?,
            read_bounded(&tls.private_key)?,
        ));
    }
    let task = worker.map(|worker| tokio::spawn(worker.run()));
    let result = server
        .build()
        .run()
        .await
        .map_err(|error| Error::other(error.to_string()));
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
    result
}
