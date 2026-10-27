use crate::tasks::pool_state_updater::get_farmer;
use crate::{HEADERS, PROTOCOL_VERSION};
use blst::min_pk::SecretKey;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_core::clvm::bls_bindings::sign;
use dg_xch_core::config::PoolWalletConfig;
use dg_xch_core::protocols::pool::{
    GetFarmerResponse, GetPoolInfoResponse, PutFarmerPayload, PutFarmerRequest,
    get_current_authentication_token,
};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::ChiaSerialize;
use std::collections::HashMap;
use std::io::Error;
use std::sync::Arc;

#[derive(Clone)]
pub struct PoolSettingsConfig {
    pub launcher_id: dg_xch_core::blockchain::sized_bytes::Bytes32,
    pub pool_url: String,
}
#[derive(Clone)]
pub struct PoolFarmerSettings {
    pub payout_instructions: String,
    pub current_difficulty: u64,
    pub current_points: u64,
}
#[derive(Clone)]
pub enum PoolIdentity {
    Bls(PoolWalletConfig, GetFarmerResponse),
    #[cfg(feature = "falcon")]
    Falcon(dg_xch_core::protocols::pool_falcon::PoolConfig),
}
#[derive(Clone)]
pub struct PoolSettings {
    pub config: PoolSettingsConfig,
    pub info: GetPoolInfoResponse,
    pub farmer: PoolFarmerSettings,
    pub identity: PoolIdentity,
}

pub async fn read_settings<Client: PoolClient + Send + Sync>(
    client: Arc<Client>,
    config: &PoolWalletConfig,
    authentication_key: &SecretKey,
) -> Result<PoolSettings, Error> {
    if !config.pool_url.starts_with("https://") {
        return Err(Error::other("pool management requires an HTTPS pool URL"));
    }
    let info = client
        .get_pool_info(&config.pool_url)
        .await
        .map_err(|error| Error::other(format!("{error:?}")))?;
    if info.authentication_token_timeout == 0
        || info.minimum_difficulty == 0
        || info.protocol_version != config.pooling_version.protocol_number()
        || info.target_puzzle_hash != config.target_puzzle_hash
    {
        return Err(Error::other(
            "pool information does not match the configured pool or has invalid limits",
        ));
    }
    let farmer = get_farmer(
        config,
        info.authentication_token_timeout,
        authentication_key,
        client,
        HashMap::new(),
        async || None,
    )
    .await
    .map_err(|error| Error::other(format!("{error:?}")))?;
    Ok(PoolSettings {
        config: PoolSettingsConfig {
            launcher_id: config.launcher_id,
            pool_url: config.pool_url.clone(),
        },
        info,
        farmer: PoolFarmerSettings {
            payout_instructions: farmer.payout_instructions.clone(),
            current_difficulty: farmer.current_difficulty,
            current_points: farmer.current_points,
        },
        identity: PoolIdentity::Bls(config.clone(), farmer),
    })
}

fn changed_payload(
    current: &PoolSettings,
    expected: &PoolFarmerSettings,
    payout: &str,
    difficulty: u64,
) -> Result<PutFarmerPayload, Error> {
    if current.farmer.payout_instructions != expected.payout_instructions
        || current.farmer.current_difficulty != expected.current_difficulty
    {
        return Err(Error::other(
            "pool settings changed since you loaded them; reload before saving",
        ));
    }
    if difficulty < current.info.minimum_difficulty {
        return Err(Error::other("difficulty is below the pool minimum"));
    }
    let payout = if payout == current.farmer.payout_instructions {
        None
    } else {
        let (hash, _) = dg_xch_keys::convert_address(payout, "xch")?;
        let normalized = hex::encode(hash.bytes());
        (normalized != current.farmer.payout_instructions).then_some(normalized)
    };
    Ok(PutFarmerPayload {
        launcher_id: current.config.launcher_id,
        authentication_token: get_current_authentication_token(
            current.info.authentication_token_timeout,
        )
        .map_err(|error| Error::other(format!("{error:?}")))?,
        authentication_public_key: None,
        payout_instructions: payout,
        suggested_difficulty: (difficulty != current.farmer.current_difficulty)
            .then_some(difficulty),
    })
}

pub async fn update_settings<Client: PoolClient + Send + Sync>(
    client: Arc<Client>,
    expected: &PoolSettings,
    authentication_key: &SecretKey,
    owner_key: &SecretKey,
    payout: &str,
    difficulty: u64,
) -> Result<PoolSettings, Error> {
    let (config, previous) = match &expected.identity {
        PoolIdentity::Bls(config, previous) => (config, previous),
        #[cfg(feature = "falcon")]
        PoolIdentity::Falcon(_) => {
            return Err(Error::other("BLS credentials cannot change a Falcon pool"));
        }
    };
    if owner_key.sk_to_pk().to_bytes() != config.owner_public_key.bytes() {
        return Err(Error::other(
            "pool owner key does not match the configured owner",
        ));
    }
    let current = read_settings(client.clone(), config, authentication_key).await?;
    #[allow(irrefutable_let_patterns)]
    if let PoolIdentity::Bls(_, current_farmer) = &current.identity
        && current_farmer.authentication_public_key != previous.authentication_public_key
    {
        return Err(Error::other(
            "pool authentication key changed; reload settings",
        ));
    }
    let payload = changed_payload(&current, &expected.farmer, payout, difficulty)?;
    if payload.payout_instructions.is_none() && payload.suggested_difficulty.is_none() {
        return Ok(current);
    }
    let signature = sign(owner_key, &hash_256(payload.to_bytes(PROTOCOL_VERSION)?))
        .to_bytes()
        .into();
    let response = client
        .put_farmer(
            &expected.config.pool_url,
            PutFarmerRequest {
                payload: payload.clone(),
                signature,
            },
            &Some(HEADERS.clone()),
        )
        .await
        .map_err(|error| Error::other(format!("{error:?}")))?;
    if (payload.payout_instructions.is_some() && response.payout_instructions != Some(true))
        || (payload.suggested_difficulty.is_some() && response.suggested_difficulty != Some(true))
    {
        return Err(Error::other(
            "pool did not accept every requested change; reload to see its current values",
        ));
    }
    read_settings(client, config, authentication_key).await
}

#[cfg(feature = "falcon")]
pub async fn read_falcon_settings(
    client: &dg_xch_clients::api::pool::DefaultPoolClient,
    config: &dg_xch_core::protocols::pool_falcon::PoolConfig,
    key: &dg_xch_core::signatures::falcon::FalconSigner,
    constants: &dg_xch_core::consensus::constants::ConsensusConstants,
) -> Result<PoolSettings, Error> {
    use dg_xch_core::protocols::pool_falcon as wire;
    if constants.signature_profile.is_bls() {
        return Err(Error::other("Falcon pool requires Falcon consensus"));
    }
    let info = client
        .get_pool_info(&config.pool_url)
        .await
        .map_err(|e| Error::other(e.error_message))?;
    if info.protocol_version != 3
        || info.target_puzzle_hash != config.target_puzzle_hash
        || info.minimum_difficulty == 0
    {
        return Err(Error::other("Falcon pool profile or target mismatch"));
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(Error::other)?
        .as_secs();
    let request = wire::sign(
        key,
        constants.genesis_challenge,
        config.target_puzzle_hash,
        "get_farmer",
        wire::FarmerQuery {
            launcher_id: config.launcher_id,
            timestamp,
        },
    )?;
    let farmer: wire::FarmerInfo = client
        .falcon_request(&config.pool_url, "farmer/info", request)
        .await
        .map_err(|e| Error::other(e.error_message))?;
    if farmer.launcher_id != config.launcher_id
        || farmer.contract_puzzle_hash != config.contract_puzzle_hash
        || farmer.current_difficulty < info.minimum_difficulty
    {
        return Err(Error::other(
            "pool returned inconsistent Falcon farmer settings",
        ));
    }
    Ok(PoolSettings {
        config: PoolSettingsConfig {
            launcher_id: config.launcher_id,
            pool_url: config.pool_url.clone(),
        },
        info,
        farmer: PoolFarmerSettings {
            payout_instructions: farmer.payout_puzzle_hash.to_string(),
            current_difficulty: farmer.current_difficulty,
            current_points: farmer.current_points,
        },
        identity: PoolIdentity::Falcon(config.clone()),
    })
}

#[cfg(feature = "falcon")]
pub async fn update_falcon_settings(
    client: &dg_xch_clients::api::pool::DefaultPoolClient,
    expected: &PoolSettings,
    key: &dg_xch_core::signatures::falcon::FalconSigner,
    constants: &dg_xch_core::consensus::constants::ConsensusConstants,
    payout: &str,
    difficulty: u64,
) -> Result<PoolSettings, Error> {
    use dg_xch_core::protocols::pool_falcon as wire;
    let PoolIdentity::Falcon(config) = &expected.identity else {
        return Err(Error::other("Falcon credentials cannot change a BLS pool"));
    };
    let current = read_falcon_settings(client, config, key, constants).await?;
    if current.farmer.payout_instructions != expected.farmer.payout_instructions
        || current.farmer.current_difficulty != expected.farmer.current_difficulty
    {
        return Err(Error::other(
            "pool settings changed since loading; reload before saving",
        ));
    }
    if difficulty < current.info.minimum_difficulty {
        return Err(Error::other("difficulty is below pool minimum"));
    }
    let (payout, _) = dg_xch_keys::convert_address(payout, constants.bech32_prefix)?;
    if hex::encode(payout.bytes()) == current.farmer.payout_instructions
        && difficulty == current.farmer.current_difficulty
    {
        return Ok(current);
    }
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(Error::other)?
        .as_secs();
    let request = wire::sign(
        key,
        constants.genesis_challenge,
        config.target_puzzle_hash,
        "update_farmer",
        wire::FarmerUpdate {
            launcher_id: config.launcher_id,
            payout_puzzle_hash: payout,
            timestamp,
            suggested_difficulty: Some(difficulty),
        },
    )?;
    let result: wire::FarmerInfo = client
        .falcon_request(&config.pool_url, "farmer/update", request)
        .await
        .map_err(|e| Error::other(e.error_message))?;
    if result.launcher_id != config.launcher_id
        || result.payout_puzzle_hash != payout
        || result.current_difficulty != difficulty
    {
        return Err(Error::other(
            "pool did not accept requested settings; reload current values",
        ));
    }
    read_falcon_settings(client, config, key, constants).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use dg_xch_core::protocols::pool::*;
    use std::sync::Mutex;

    struct MockPool {
        info: GetPoolInfoResponse,
        farmer: Mutex<GetFarmerResponse>,
        puts: Mutex<Vec<PutFarmerRequest>>,
        failure: Option<PoolErrorCode>,
    }

    #[async_trait]
    impl PoolClient for MockPool {
        async fn get_pool_info(&self, _: &str) -> Result<GetPoolInfoResponse, PoolError> {
            Ok(self.info.clone())
        }
        async fn get_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
            &self,
            _: &str,
            _: GetFarmerRequest,
            _: &Option<HashMap<String, String, S>>,
        ) -> Result<GetFarmerResponse, PoolError> {
            if let Some(code) = &self.failure {
                return Err(PoolError {
                    error_code: *code as u8,
                    error_message: "test failure".into(),
                });
            }
            Ok(self.farmer.lock().unwrap().clone())
        }
        async fn put_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
            &self,
            _: &str,
            request: PutFarmerRequest,
            _: &Option<HashMap<String, String, S>>,
        ) -> Result<PutFarmerResponse, PoolError> {
            let payload = &request.payload;
            let mut farmer = self.farmer.lock().unwrap();
            if let Some(payout) = &payload.payout_instructions {
                farmer.payout_instructions = payout.clone();
            }
            if let Some(difficulty) = payload.suggested_difficulty {
                farmer.current_difficulty = difficulty;
            }
            let response = PutFarmerResponse {
                authentication_public_key: None,
                payout_instructions: payload.payout_instructions.as_ref().map(|_| true),
                suggested_difficulty: payload.suggested_difficulty.map(|_| true),
            };
            self.puts.lock().unwrap().push(request);
            Ok(response)
        }
        async fn post_farmer<S: std::hash::BuildHasher + Sync + Send + 'static>(
            &self,
            _: &str,
            _: PostFarmerRequest,
            _: &Option<HashMap<String, String, S>>,
        ) -> Result<PostFarmerResponse, PoolError> {
            panic!("background registration must never happen")
        }
        async fn post_partial<S: std::hash::BuildHasher + Sync + Send + 'static>(
            &self,
            _: &str,
            _: PostPartialRequest,
            _: &Option<HashMap<String, String, S>>,
        ) -> Result<PostPartialResponse, PoolError> {
            panic!("unexpected partial")
        }
    }

    fn fixture() -> (MockPool, PoolWalletConfig, SecretKey) {
        let key = SecretKey::key_gen_v3(&[42; 32], &[]).unwrap();
        let config = PoolWalletConfig {
            pooling_version: Default::default(),
            launcher_id: [1; 32].into(),
            pool_url: "https://pool.example".into(),
            target_puzzle_hash: [2; 32].into(),
            payout_instructions: hex::encode([99; 32]),
            p2_singleton_puzzle_hash: [3; 32].into(),
            owner_public_key: key.sk_to_pk().to_bytes().into(),
            difficulty: Some(9999),
        };
        let pool = MockPool {
            info: GetPoolInfoResponse {
                name: "Test".into(),
                logo_url: String::new(),
                minimum_difficulty: 10,
                relative_lock_height: 100,
                protocol_version: 1,
                fee: "0.01".into(),
                description: String::new(),
                target_puzzle_hash: config.target_puzzle_hash,
                authentication_token_timeout: 10,
            },
            farmer: Mutex::new(GetFarmerResponse {
                authentication_public_key: config.owner_public_key,
                payout_instructions: hex::encode([4; 32]),
                current_difficulty: 20,
                current_points: 1,
            }),
            puts: Mutex::new(Vec::new()),
            failure: None,
        };
        (pool, config, key)
    }

    #[tokio::test]
    async fn polling_never_overwrites_pool_values_or_registers_on_error() {
        for failure in [
            None,
            Some(PoolErrorCode::FarmerNotKnown),
            Some(PoolErrorCode::InvalidSignature),
        ] {
            let (mut pool, config, key) = fixture();
            pool.failure = failure;
            let client = Arc::new(pool);
            let state = Arc::new(dg_xch_core::protocols::farmer::FarmerSharedState::<()> {
                owner_secret_keys: Arc::new(HashMap::from([(
                    config.owner_public_key,
                    key.clone(),
                )])),
                owner_public_keys_to_auth_secret_keys: Arc::new(HashMap::from([(
                    config.owner_public_key,
                    key,
                )])),
                ..Default::default()
            });
            let mut settings = crate::farmer::config::Config::<()>::default();
            settings.pool_info.push(config);
            crate::tasks::pool_state_updater::update_pool_state(client.clone(), &settings, state)
                .await
                .unwrap();
            assert!(client.puts.lock().unwrap().is_empty());
            assert_eq!(client.farmer.lock().unwrap().current_difficulty, 20);
            assert_eq!(
                client.farmer.lock().unwrap().payout_instructions,
                hex::encode([4; 32])
            );
        }
    }

    #[tokio::test]
    async fn explicit_changes_are_selective_and_stale_edits_fail() {
        let (pool, config, key) = fixture();
        let client = Arc::new(pool);
        let snapshot = read_settings(client.clone(), &config, &key).await.unwrap();
        update_settings(
            client.clone(),
            &snapshot,
            &key,
            &key,
            &snapshot.farmer.payout_instructions,
            20,
        )
        .await
        .unwrap();
        assert!(client.puts.lock().unwrap().is_empty());
        assert!(
            update_settings(client.clone(), &snapshot, &key, &key, "invalid", 20)
                .await
                .is_err()
        );
        assert!(
            update_settings(
                client.clone(),
                &snapshot,
                &key,
                &key,
                &snapshot.farmer.payout_instructions,
                1
            )
            .await
            .is_err()
        );
        let updated = update_settings(
            client.clone(),
            &snapshot,
            &key,
            &key,
            &snapshot.farmer.payout_instructions,
            30,
        )
        .await
        .unwrap();
        assert_eq!(updated.farmer.current_difficulty, 30);
        {
            let writes = client.puts.lock().unwrap();
            assert_eq!(writes.len(), 1);
            assert!(writes[0].payload.authentication_public_key.is_none());
            assert!(writes[0].payload.payout_instructions.is_none());
        }
        assert!(
            update_settings(
                client.clone(),
                &snapshot,
                &key,
                &key,
                &hex::encode([5; 32]),
                40
            )
            .await
            .is_err()
        );
        let current = update_settings(
            client.clone(),
            &updated,
            &key,
            &key,
            &hex::encode([5; 32]),
            30,
        )
        .await
        .unwrap();
        assert_eq!(current.farmer.payout_instructions, hex::encode([5; 32]));
        assert!(
            client.puts.lock().unwrap()[1]
                .payload
                .suggested_difficulty
                .is_none()
        );
    }
}
