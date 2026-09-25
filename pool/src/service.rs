use crate::authentication::{
    v2_auth_key, validate_v1_token, verify, verify_v1_get, verify_v2_login,
};
use crate::store::{Farmer, PoolStore, PoolVersion};
use crate::verification::verify_partial_signature;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::protocols::{pool::*, pool_v2};
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Error;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Semaphore};

pub struct Membership {
    pub owner_public_key: Bytes48,
    pub contract_puzzle_hash: Bytes32,
}

#[async_trait]
pub trait PoolChain: Send + Sync {
    async fn membership(
        &self,
        version: PoolVersion,
        launcher: Bytes32,
        authentication_key: Bytes48,
    ) -> Result<Membership, Error>;
    async fn partial(
        &self,
        payload: &PostPartialPayload,
        farmer: &Farmer,
        now: u64,
    ) -> Result<Bytes32, Error>;
}

pub struct PoolService {
    pub info: GetPoolInfoResponse,
    pub fee_basis_points: u16,
    pub pool_memoization: String,
    pub enable_v2: bool,
    pub store: Mutex<PoolStore>,
    pub chain: Arc<dyn PoolChain>,
    pub requests: Semaphore,
}

fn failure(code: PoolErrorCode, message: impl Into<String>) -> PoolError {
    PoolError {
        error_code: code as u8,
        error_message: message.into(),
    }
}

fn storage_error(_: Error) -> PoolError {
    failure(
        PoolErrorCode::ServerException,
        "pool storage operation failed",
    )
}

fn now() -> Result<u64, PoolError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| failure(PoolErrorCode::ServerException, "system clock is invalid"))
}

fn payout(value: &str) -> Result<Bytes32, PoolError> {
    dg_xch_wallet::assets::parse_asset_hash(value).map_err(|_| {
        failure(
            PoolErrorCode::InvalidPayoutInstructions,
            "payout must be a 32-byte puzzle hash in hexadecimal",
        )
    })
}

impl PoolService {
    async fn membership(&self, farmer: &Farmer, key: Bytes48) -> Result<(), PoolError> {
        blst::min_pk::PublicKey::key_validate(key.as_ref()).map_err(|_| {
            failure(
                PoolErrorCode::InvalidSignature,
                "invalid authentication public key",
            )
        })?;
        let membership = self
            .chain
            .membership(farmer.version, farmer.launcher_id, key)
            .await
            .map_err(|error| failure(PoolErrorCode::InvalidSingleton, error.to_string()))?;
        if membership.owner_public_key != farmer.owner_public_key
            || membership.contract_puzzle_hash != farmer.contract_puzzle_hash
        {
            return Err(failure(
                PoolErrorCode::InvalidSingleton,
                "pool membership changed",
            ));
        }
        Ok(())
    }

    fn version(&self, version: PoolVersion) -> Result<(), PoolError> {
        if version == PoolVersion::V2 && !self.enable_v2 {
            return Err(failure(
                PoolErrorCode::RequestFailed,
                "experimental pooling v2 is disabled",
            ));
        }
        Ok(())
    }

    fn difficulty(&self, suggested: Option<u64>) -> Result<u64, PoolError> {
        let difficulty = suggested.unwrap_or(self.info.minimum_difficulty);
        if difficulty == 0 || difficulty < self.info.minimum_difficulty {
            return Err(failure(
                PoolErrorCode::InvalidDifficulty,
                "difficulty is below the pool minimum",
            ));
        }
        Ok(difficulty)
    }

    async fn farmer(&self, version: PoolVersion, launcher: Bytes32) -> Result<Farmer, PoolError> {
        self.version(version)?;
        self.store
            .lock()
            .await
            .farmer(launcher)
            .await
            .map_err(storage_error)?
            .filter(|farmer| farmer.version == version)
            .ok_or_else(|| {
                failure(
                    PoolErrorCode::FarmerNotKnown,
                    "farmer is not registered for this protocol version",
                )
            })
    }

    pub async fn register(
        &self,
        version: PoolVersion,
        request: PostFarmerRequest,
    ) -> Result<PostFarmerResponse, PoolError> {
        self.version(version)?;
        let payload = &request.payload;
        blst::min_pk::PublicKey::key_validate(payload.authentication_public_key.as_ref()).map_err(
            |_| {
                failure(
                    PoolErrorCode::InvalidSignature,
                    "invalid authentication public key",
                )
            },
        )?;
        let payout_puzzle_hash = payout(&payload.payout_instructions)?;
        let difficulty = self.difficulty(payload.suggested_difficulty)?;
        if version == PoolVersion::V1 {
            validate_v1_token(
                payload.authentication_token,
                self.info.authentication_token_timeout,
                now()?,
            )
            .map_err(|error| {
                failure(PoolErrorCode::InvalidAuthenticationToken, error.to_string())
            })?;
        } else if payload.authentication_token != 0 {
            return Err(failure(
                PoolErrorCode::InvalidAuthenticationToken,
                "v2 requires the legacy token to be zero",
            ));
        }
        let membership = self
            .chain
            .membership(
                version,
                payload.launcher_id,
                payload.authentication_public_key,
            )
            .await
            .map_err(|error| failure(PoolErrorCode::InvalidSingleton, error.to_string()))?;
        let key = if version == PoolVersion::V1 {
            membership.owner_public_key
        } else {
            v2_auth_key(payload.authentication_public_key)
                .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?
        };
        let message = hash_256(
            payload
                .to_bytes(ChiaProtocolVersion::Chia0_0_37)
                .map_err(storage_error)?,
        );
        verify(key, &message, request.signature)
            .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?;
        let mut store = self.store.lock().await;
        if store
            .farmer(payload.launcher_id)
            .await
            .map_err(storage_error)?
            .is_some()
        {
            return Err(failure(
                PoolErrorCode::FarmerAlreadyKnown,
                "farmer is already registered",
            ));
        }
        store
            .register(&Farmer {
                version,
                launcher_id: payload.launcher_id,
                owner_public_key: membership.owner_public_key,
                authentication_public_key: payload.authentication_public_key,
                contract_puzzle_hash: membership.contract_puzzle_hash,
                payout_puzzle_hash,
                difficulty,
            })
            .await
            .map_err(storage_error)?;
        Ok(PostFarmerResponse {
            welcome_message: format!("Registered with {}", self.info.name),
        })
    }

    async fn response(&self, farmer: &Farmer) -> Result<GetFarmerResponse, PoolError> {
        Ok(GetFarmerResponse {
            authentication_public_key: farmer.authentication_public_key,
            payout_instructions: hex::encode(farmer.payout_puzzle_hash),
            current_difficulty: farmer.difficulty,
            current_points: self
                .store
                .lock()
                .await
                .points(farmer.launcher_id)
                .await
                .map_err(storage_error)?,
        })
    }

    pub async fn get_v1(&self, request: GetFarmerRequest) -> Result<GetFarmerResponse, PoolError> {
        validate_v1_token(
            request.authentication_token,
            self.info.authentication_token_timeout,
            now()?,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidAuthenticationToken, error.to_string()))?;
        let farmer = self.farmer(PoolVersion::V1, request.launcher_id).await?;
        verify_v1_get(
            farmer.authentication_public_key,
            request.launcher_id,
            self.info.target_puzzle_hash,
            request.authentication_token,
            request.signature,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?;
        self.response(&farmer).await
    }

    pub async fn authenticate(
        &self,
        request: pool_v2::GetAuthRequest,
    ) -> Result<pool_v2::GetAuthResponse, PoolError> {
        let farmer = self.farmer(PoolVersion::V2, request.launcher_id).await?;
        let now = now()?;
        verify_v2_login(
            farmer.authentication_public_key,
            request.launcher_id,
            self.info.target_puzzle_hash,
            request.timestamp,
            request.signature,
            now,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?;
        self.membership(&farmer, farmer.authentication_public_key)
            .await?;
        let (authentication_token, expiration) = self
            .store
            .lock()
            .await
            .issue_token(
                &farmer,
                now,
                u64::from(self.info.authentication_token_timeout) * 60,
            )
            .await
            .map_err(storage_error)?;
        Ok(pool_v2::GetAuthResponse {
            authentication_token,
            expiration,
        })
    }

    async fn token_farmer(
        &self,
        launcher: Bytes32,
        legacy: u64,
        token: &str,
    ) -> Result<Farmer, PoolError> {
        if legacy != 0 {
            return Err(failure(
                PoolErrorCode::InvalidAuthenticationToken,
                "v2 legacy token must be zero",
            ));
        }
        let farmer = self.farmer(PoolVersion::V2, launcher).await?;
        self.store
            .lock()
            .await
            .verify_token(&farmer, token, now()?)
            .await
            .map_err(|error| {
                failure(PoolErrorCode::InvalidAuthenticationToken, error.to_string())
            })?;
        Ok(farmer)
    }

    pub async fn get_v2(
        &self,
        request: pool_v2::GetFarmerRequest,
    ) -> Result<GetFarmerResponse, PoolError> {
        let farmer = self
            .token_farmer(
                request.launcher_id,
                request.authentication_token,
                &request.authentication_token_v2,
            )
            .await?;
        self.response(&farmer).await
    }

    async fn update(
        &self,
        farmer: Farmer,
        payload: PutFarmerPayload,
    ) -> Result<PutFarmerResponse, PoolError> {
        self.membership(
            &farmer,
            payload
                .authentication_public_key
                .unwrap_or(farmer.authentication_public_key),
        )
        .await?;
        let mut replacement = farmer.clone();
        if let Some(value) = &payload.payout_instructions {
            replacement.payout_puzzle_hash = payout(value)?;
        }
        if payload.suggested_difficulty.is_some() {
            replacement.difficulty = self.difficulty(payload.suggested_difficulty)?;
        }
        if let Some(key) = payload.authentication_public_key {
            replacement.authentication_public_key = key;
        }
        self.store
            .lock()
            .await
            .update(&farmer, &replacement)
            .await
            .map_err(storage_error)?;
        Ok(PutFarmerResponse {
            authentication_public_key: payload.authentication_public_key.map(|_| true),
            payout_instructions: payload.payout_instructions.map(|_| true),
            suggested_difficulty: payload.suggested_difficulty.map(|_| true),
        })
    }

    pub async fn put_v1(&self, request: PutFarmerRequest) -> Result<PutFarmerResponse, PoolError> {
        let payload = &request.payload;
        validate_v1_token(
            payload.authentication_token,
            self.info.authentication_token_timeout,
            now()?,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidAuthenticationToken, error.to_string()))?;
        let farmer = self.farmer(PoolVersion::V1, payload.launcher_id).await?;
        verify(
            farmer.owner_public_key,
            &hash_256(
                payload
                    .to_bytes(ChiaProtocolVersion::Chia0_0_37)
                    .map_err(storage_error)?,
            ),
            request.signature,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?;
        self.update(farmer, request.payload).await
    }

    pub async fn put_v2(
        &self,
        request: pool_v2::PutFarmerRequest,
    ) -> Result<PutFarmerResponse, PoolError> {
        let payload = request.payload;
        let farmer = self
            .token_farmer(
                payload.farmer.launcher_id,
                payload.farmer.authentication_token,
                &payload.authentication_token_v2,
            )
            .await?;
        self.update(farmer, payload.farmer).await
    }

    async fn partial(
        &self,
        farmer: Farmer,
        request: PostPartialRequest,
    ) -> Result<PostPartialResponse, PoolError> {
        verify_partial_signature(
            &request.payload,
            farmer.authentication_public_key,
            request.aggregate_signature,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?;
        let membership = self
            .chain
            .membership(
                farmer.version,
                farmer.launcher_id,
                farmer.authentication_public_key,
            )
            .await
            .map_err(|error| failure(PoolErrorCode::InvalidSingleton, error.to_string()))?;
        if membership.contract_puzzle_hash != farmer.contract_puzzle_hash
            || membership.owner_public_key != farmer.owner_public_key
        {
            return Err(failure(
                PoolErrorCode::InvalidSingleton,
                "farmer is no longer a member of this pool",
            ));
        }
        let now = now()?;
        let id = self
            .chain
            .partial(&request.payload, &farmer, now)
            .await
            .map_err(|error| failure(PoolErrorCode::InvalidProof, error.to_string()))?;
        self.store
            .lock()
            .await
            .credit_partial(id, &farmer, now)
            .await
            .map_err(|_| {
                failure(
                    PoolErrorCode::InvalidProof,
                    "duplicate partial or changed farmer settings",
                )
            })?;
        Ok(PostPartialResponse {
            new_difficulty: farmer.difficulty,
        })
    }

    pub async fn partial_v1(
        &self,
        request: PostPartialRequest,
    ) -> Result<PostPartialResponse, PoolError> {
        validate_v1_token(
            request.payload.authentication_token,
            self.info.authentication_token_timeout,
            now()?,
        )
        .map_err(|error| failure(PoolErrorCode::InvalidAuthenticationToken, error.to_string()))?;
        let farmer = self
            .farmer(PoolVersion::V1, request.payload.launcher_id)
            .await?;
        self.partial(farmer, request).await
    }

    pub async fn partial_v2(
        &self,
        request: pool_v2::PostPartialRequest,
    ) -> Result<PostPartialResponse, PoolError> {
        let farmer = self
            .token_farmer(
                request.payload.launcher_id,
                request.payload.authentication_token,
                &request.authentication_token_v2,
            )
            .await?;
        self.partial(
            farmer,
            PostPartialRequest {
                payload: request.payload,
                aggregate_signature: request.aggregate_signature,
            },
        )
        .await
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
