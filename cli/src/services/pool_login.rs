use clap::Args;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::{io::Error, path::PathBuf};

#[derive(Args)]
pub struct LoginArgs {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    launcher_id: Option<Bytes32>,
    /// Link another PlotNFT by proving ownership with its existing farmer key.
    #[arg(long, conflicts_with = "unlink")]
    link_config: Option<PathBuf>,
    #[arg(long, requires = "link_config")]
    link_launcher_id: Option<Bytes32>,
    /// Separate this PlotNFT from its linked account and revoke existing sessions.
    #[arg(long)]
    unlink: bool,
}

#[cfg(feature = "falcon")]
async fn run_falcon(
    args: &LoginArgs,
    config: dg_xch_farmer::farmer::config::Config,
) -> Result<(), Error> {
    use dg_xch_core::protocols::pool_falcon as wire;
    use dg_xch_farmer::farmer::config::Config;
    use rand::RngExt;
    fn select(config: &Config, launcher: Option<Bytes32>) -> Result<&wire::PoolConfig, Error> {
        let pools: Vec<_> = config
            .falcon_pools
            .iter()
            .filter(|pool| launcher.is_none_or(|id| id == pool.launcher_id))
            .collect();
        if pools.len() != 1 {
            return Err(Error::other(
                "select exactly one configured PlotNFT with --launcher-id",
            ));
        }
        Ok(pools[0])
    }
    config.validate_keys()?;
    let pool = select(&config, args.launcher_id)?;
    let genesis = config.constants()?.genesis_challenge;
    let keys = config
        .falcon_keys()?
        .ok_or_else(|| Error::other("Falcon keys required"))?;
    let linked = args.link_config.as_deref().map(load_config).transpose()?;
    let linked_pool = linked
        .as_ref()
        .map(|other| select(other, args.link_launcher_id))
        .transpose()?;
    if let (Some(other), Some(other_pool)) = (&linked, linked_pool) {
        other.validate_keys()?;
        if other.constants()?.genesis_challenge != genesis
            || other_pool.target_puzzle_hash != pool.target_puzzle_hash
            || other_pool.pool_url.trim_end_matches('/') != pool.pool_url.trim_end_matches('/')
        {
            return Err(Error::other(
                "linked PlotNFTs must belong to the same chain and pool",
            ));
        }
    }
    let proof = wire::BrowserProof {
        launcher_id: pool.launcher_id,
        linked_launcher_id: linked_pool.map(|other| other.launcher_id),
        action: if linked.is_some() {
            wire::BrowserAction::Link
        } else if args.unlink {
            wire::BrowserAction::Unlink
        } else {
            wire::BrowserAction::Login
        },
        nonce: hex::encode(rand::rng().random::<[u8; 32]>()),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(Error::other)?
            .as_secs(),
    };
    let linked_authorization = linked
        .as_ref()
        .map(|other| {
            let keys = other
                .falcon_keys()?
                .ok_or_else(|| Error::other("Falcon keys required"))?;
            wire::sign(
                &keys.wallet,
                genesis,
                pool.target_puzzle_hash,
                "web_link",
                proof.clone(),
            )
            .map(|signed| signed.authorization)
        })
        .transpose()?;
    let request = wire::BrowserAuthorization {
        owner: wire::sign(
            &keys.wallet,
            genesis,
            pool.target_puzzle_hash,
            "web_login",
            proof,
        )?,
        linked_authorization,
    };
    let ticket: wire::BrowserTicket = config
        .pool_client()?
        .browser_login(&pool.pool_url, request)
        .await
        .map_err(|error| Error::other(error.error_message))?;
    print_ticket(&pool.pool_url, &ticket)
}

fn load_config(path: &std::path::Path) -> Result<dg_xch_farmer::farmer::config::Config, Error> {
    super::config::load(
        "FARMER",
        Some(path),
        super::config::Format::Yaml,
        1024 * 1024,
    )
}

pub async fn run(args: &LoginArgs) -> Result<(), Error> {
    let config = load_config(&args.config)?;
    if config.constants()?.signature_profile.is_bls() {
        run_bls(args, config).await
    } else {
        #[cfg(feature = "falcon")]
        {
            run_falcon(args, config).await
        }
        #[cfg(not(feature = "falcon"))]
        {
            Err(Error::other(
                "this chain requires a build with the falcon feature",
            ))
        }
    }
}

fn select_bls(
    config: &dg_xch_farmer::farmer::config::Config,
    launcher: Option<Bytes32>,
) -> Result<
    (
        &dg_xch_core::config::PoolWalletConfig,
        blst::min_pk::SecretKey,
    ),
    Error,
> {
    let pools: Vec<_> = config
        .pool_info
        .iter()
        .filter(|p| launcher.is_none_or(|id| id == p.launcher_id))
        .collect();
    if pools.len() != 1 {
        return Err(Error::other(
            "select exactly one configured PlotNFT with --launcher-id",
        ));
    }
    let pool = pools[0];
    for entry in &config.farmer_info {
        if let Some(bytes) = entry.owner_secret_key {
            let key = blst::min_pk::SecretKey::from_bytes(bytes.as_ref())
                .map_err(|_| Error::other("invalid PlotNFT owner key"))?;
            if dg_xch_core::blockchain::sized_bytes::Bytes48::from(key.sk_to_pk().to_bytes())
                == pool.owner_public_key
            {
                return Ok((pool, key));
            }
        }
    }
    Err(Error::other(
        "the configured PlotNFT owner key is unavailable",
    ))
}

async fn run_bls(
    args: &LoginArgs,
    config: dg_xch_farmer::farmer::config::Config,
) -> Result<(), Error> {
    use dg_xch_core::protocols::pool_web as wire;
    use rand::RngExt;
    config.validate_keys()?;
    let (pool, key) = select_bls(&config, args.launcher_id)?;
    let browser_url = bls_browser_url(pool)?;
    let genesis = config.constants()?.genesis_challenge;
    let linked = args.link_config.as_deref().map(load_config).transpose()?;
    let linked_pool = linked
        .as_ref()
        .map(|other| select_bls(other, args.link_launcher_id))
        .transpose()?;
    if let (Some(other), Some((other_pool, _))) = (&linked, &linked_pool) {
        other.validate_keys()?;
        if !other.constants()?.signature_profile.is_bls()
            || other.constants()?.genesis_challenge != genesis
            || other_pool.target_puzzle_hash != pool.target_puzzle_hash
            || other_pool.pool_url.trim_end_matches('/') != pool.pool_url.trim_end_matches('/')
        {
            return Err(Error::other(
                "linked PlotNFTs must belong to the same chain and pool",
            ));
        }
    }
    let proof = wire::BrowserProof {
        launcher_id: pool.launcher_id,
        linked_launcher_id: linked_pool.as_ref().map(|(p, _)| p.launcher_id),
        action: if linked.is_some() {
            wire::BrowserAction::Link
        } else if args.unlink {
            wire::BrowserAction::Unlink
        } else {
            wire::BrowserAction::Login
        },
        nonce: hex::encode(rand::rng().random::<[u8; 32]>()),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(Error::other)?
            .as_secs(),
    };
    let sign =
        |key: &blst::min_pk::SecretKey, operation| -> Result<wire::BlsAuthorization, Error> {
            let message = wire::bls_message(genesis, pool.target_puzzle_hash, operation, &proof)?;
            Ok(wire::BlsAuthorization {
                public_key: key.sk_to_pk().to_bytes().into(),
                signature: dg_xch_core::clvm::bls_bindings::sign(key, &message)
                    .to_bytes()
                    .into(),
            })
        };
    let request = wire::BlsBrowserAuthorization {
        owner: sign(&key, "web_login")?,
        linked_authorization: linked_pool
            .as_ref()
            .map(|(_, k)| sign(k, "web_link"))
            .transpose()?,
        proof,
    };
    let ticket = config
        .pool_client()?
        .browser_login(&browser_url, request)
        .await
        .map_err(|e| Error::other(e.error_message))?;
    print_ticket(&browser_url, &ticket)
}

fn print_ticket(
    base: &str,
    ticket: &dg_xch_core::protocols::pool_web::BrowserTicket,
) -> Result<(), Error> {
    if ticket.ticket.len() != 64 || !ticket.ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::other("invalid login ticket returned by pool"));
    }
    println!(
        "{}/login#ticket={}",
        base.trim_end_matches('/'),
        ticket.ticket
    );
    eprintln!("This private login link expires in two minutes and can be used once.");
    Ok(())
}

// V2 farmer endpoints live under /v2; the browser is shared at the pool root.
fn bls_browser_url(pool: &dg_xch_core::config::PoolWalletConfig) -> Result<String, Error> {
    let mut url = dg_xch_clients::api::pool::validate_pool_url(&pool.pool_url)?;
    if pool.pooling_version == dg_xch_core::protocols::pool::PoolVersion::V2 {
        let path = url
            .path()
            .trim_end_matches('/')
            .strip_suffix("/v2")
            .map(str::to_owned);
        if let Some(path) = path {
            url.set_path(&path);
        }
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::{config::PoolWalletConfig, protocols::pool::PoolVersion};

    #[test]
    fn browser_login_uses_the_shared_root_for_v2_farmer_urls() {
        for (version, base, expected) in [
            (
                PoolVersion::V1,
                "https://pool.example/",
                "https://pool.example",
            ),
            (
                PoolVersion::V2,
                "https://pool.example/v2/",
                "https://pool.example",
            ),
            (
                PoolVersion::V2,
                "https://pool.example/pool/v2",
                "https://pool.example/pool",
            ),
            (
                PoolVersion::V2,
                "https://pool.example",
                "https://pool.example",
            ),
        ] {
            let pool: PoolWalletConfig = serde_json::from_value(serde_json::json!({
                "pooling_version": version, "pool_url": base
            }))
            .unwrap();
            assert_eq!(bls_browser_url(&pool).unwrap(), expected);
        }
    }
}
