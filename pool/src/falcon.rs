//! Falcon-native pool registration and partial authorization. Accounting remains
//! shared with the legacy protocol, but no BLS key or signature is used here.
use crate::{
    service::{PoolService, failure, now, storage_error},
    store::{Farmer, PoolVersion},
};
use dg_xch_core::{
    blockchain::sized_bytes::Bytes32,
    protocols::{
        pool::{PoolError, PoolErrorCode, PostPartialResponse},
        pool_falcon as wire,
    },
    signatures::falcon::FnDsa512V2,
};

impl PoolService {
    fn require_falcon(&self) -> Result<(), PoolError> {
        if self.chain.signature_profile().is_bls() {
            return Err(failure(
                PoolErrorCode::RequestFailed,
                "Falcon protocol is unavailable on a BLS chain",
            ));
        }
        Ok(())
    }

    fn authenticate_falcon<T: serde::Serialize>(
        &self,
        request: &wire::Authenticated<T>,
        operation: &str,
        timestamp: u64,
    ) -> Result<wire::PublicKey, PoolError> {
        self.require_falcon()?;
        wire::timestamp(timestamp, now()?)
            .map_err(|e| failure(PoolErrorCode::InvalidAuthenticationToken, e.to_string()))?;
        let key = wire::PublicKey(request.authorization.public_key);
        if !FnDsa512V2::valid_public_key(&key.0) {
            return Err(failure(
                PoolErrorCode::InvalidSignature,
                "invalid Falcon public key",
            ));
        }
        wire::verify(
            &request.authorization,
            &key,
            self.chain.genesis(),
            self.info.target_puzzle_hash,
            operation,
            &request.payload,
        )
        .map_err(|e| failure(PoolErrorCode::InvalidSignature, e.to_string()))?;
        Ok(key)
    }

    async fn falcon_farmer(
        &self,
        launcher: Bytes32,
        key: &wire::PublicKey,
    ) -> Result<Farmer<wire::PublicKey>, PoolError> {
        let farmer = self
            .store
            .lock()
            .await
            .farmer_typed::<wire::PublicKey>(launcher)
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                failure(
                    PoolErrorCode::FarmerNotKnown,
                    "Falcon farmer is not registered",
                )
            })?;
        if farmer.version != PoolVersion::Falcon
            || farmer.owner_public_key != *key
            || farmer.authentication_public_key != *key
        {
            return Err(failure(
                PoolErrorCode::InvalidSignature,
                "Falcon owner does not match registration",
            ));
        }
        let membership = self
            .chain
            .falcon_membership(launcher, key.clone())
            .await
            .map_err(|e| failure(PoolErrorCode::InvalidSingleton, e.to_string()))?;
        if membership.owner_public_key != *key
            || membership.contract_puzzle_hash != farmer.contract_puzzle_hash
        {
            return Err(failure(
                PoolErrorCode::InvalidSingleton,
                "Falcon pool membership changed",
            ));
        }
        Ok(farmer)
    }

    async fn falcon_info(
        &self,
        farmer: &Farmer<wire::PublicKey>,
    ) -> Result<wire::FarmerInfo, PoolError> {
        Ok(wire::FarmerInfo {
            launcher_id: farmer.launcher_id,
            contract_puzzle_hash: farmer.contract_puzzle_hash,
            payout_puzzle_hash: farmer.payout_puzzle_hash,
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

    pub async fn browser_login(
        &self,
        request: wire::BrowserAuthorization,
    ) -> Result<wire::BrowserTicket, PoolError> {
        let proof = &request.owner.payload;
        let key = self.authenticate_falcon(&request.owner, "web_login", proof.timestamp)?;
        self.falcon_farmer(proof.launcher_id, &key).await?;
        match (
            &proof.action,
            proof.linked_launcher_id,
            &request.linked_authorization,
        ) {
            (wire::BrowserAction::Link, Some(other), Some(authorization))
                if other != proof.launcher_id =>
            {
                let other_key = wire::PublicKey(authorization.public_key);
                wire::verify(
                    authorization,
                    &other_key,
                    self.chain.genesis(),
                    self.info.target_puzzle_hash,
                    "web_link",
                    proof,
                )
                .map_err(|error| failure(PoolErrorCode::InvalidSignature, error.to_string()))?;
                self.falcon_farmer(other, &other_key).await?;
            }
            (wire::BrowserAction::Login | wire::BrowserAction::Unlink, None, None) => {}
            _ => {
                return Err(failure(
                    PoolErrorCode::InvalidSignature,
                    "both PlotNFT owners must authorize linking",
                ));
            }
        }
        self.store
            .lock()
            .await
            .issue_browser_ticket(proof, now()?)
            .await
            .map_err(|error| failure(PoolErrorCode::InvalidAuthenticationToken, error.to_string()))
    }

    pub async fn register_falcon(
        &self,
        request: wire::Authenticated<wire::Registration>,
    ) -> Result<wire::FarmerInfo, PoolError> {
        let key = self.authenticate_falcon(&request, "register", request.payload.timestamp)?;
        if request.payload.payout_puzzle_hash == Bytes32::default() {
            return Err(failure(
                PoolErrorCode::InvalidPayoutInstructions,
                "empty payout puzzle hash",
            ));
        }
        let membership = self
            .chain
            .falcon_membership(request.payload.launcher_id, key.clone())
            .await
            .map_err(|e| failure(PoolErrorCode::InvalidSingleton, e.to_string()))?;
        if membership.owner_public_key != key {
            return Err(failure(
                PoolErrorCode::InvalidSignature,
                "PlotNFT owner mismatch",
            ));
        }
        let mut farmer = Farmer {
            version: PoolVersion::Falcon,
            launcher_id: request.payload.launcher_id,
            owner_public_key: key.clone(),
            authentication_public_key: key,
            contract_puzzle_hash: membership.contract_puzzle_hash,
            payout_puzzle_hash: request.payload.payout_puzzle_hash,
            difficulty: self.info.minimum_difficulty,
        };
        {
            let mut store = self.store.lock().await;
            if let Some(existing) = store
                .farmer_typed::<wire::PublicKey>(farmer.launcher_id)
                .await
                .map_err(storage_error)?
            {
                if existing.version != farmer.version
                    || existing.owner_public_key != farmer.owner_public_key
                    || existing.contract_puzzle_hash != farmer.contract_puzzle_hash
                    || existing.payout_puzzle_hash != farmer.payout_puzzle_hash
                {
                    return Err(failure(
                        PoolErrorCode::FarmerAlreadyKnown,
                        "registration exists with different settings",
                    ));
                }
                farmer = existing;
            } else {
                store.register(&farmer).await.map_err(storage_error)?;
            }
        }
        self.falcon_info(&farmer).await
    }

    pub async fn get_falcon(
        &self,
        request: wire::Authenticated<wire::FarmerQuery>,
    ) -> Result<wire::FarmerInfo, PoolError> {
        let key = self.authenticate_falcon(&request, "get_farmer", request.payload.timestamp)?;
        let farmer = self
            .falcon_farmer(request.payload.launcher_id, &key)
            .await?;
        self.falcon_info(&farmer).await
    }

    pub async fn update_falcon(
        &self,
        request: wire::Authenticated<wire::FarmerUpdate>,
    ) -> Result<wire::FarmerInfo, PoolError> {
        let key = self.authenticate_falcon(&request, "update_farmer", request.payload.timestamp)?;
        let farmer = self
            .falcon_farmer(request.payload.launcher_id, &key)
            .await?;
        if request
            .payload
            .suggested_difficulty
            .is_some_and(|difficulty| difficulty < self.info.minimum_difficulty)
        {
            return Err(failure(
                PoolErrorCode::RequestFailed,
                "difficulty is below pool minimum",
            ));
        }
        let updated = self
            .store
            .lock()
            .await
            .update_falcon_payout(
                &farmer,
                request.payload.payout_puzzle_hash,
                request.payload.timestamp,
                request.payload.suggested_difficulty,
            )
            .await
            .map_err(|e| failure(PoolErrorCode::RequestFailed, e.to_string()))?;
        self.falcon_info(&updated).await
    }

    pub async fn partial_falcon(
        &self,
        request: wire::PartialRequest,
    ) -> Result<PostPartialResponse, PoolError> {
        let key = self.authenticate_falcon(
            &request.owner,
            "partial_owner",
            request.owner.payload.timestamp,
        )?;
        let payload = &request.owner.payload.partial;
        if payload.authentication_token != 0 {
            return Err(failure(
                PoolErrorCode::InvalidAuthenticationToken,
                "legacy pool token is not valid for Falcon",
            ));
        }
        let farmer = self.falcon_farmer(payload.launcher_id, &key).await?;
        let started = std::time::Instant::now();
        let outcome = self.validate_falcon_partial(&request, &farmer).await;
        self.record_partial_outcome(payload, farmer.difficulty, started, &outcome)
            .await;
        outcome.map(|(response, _)| response)
    }

    async fn validate_falcon_partial(
        &self,
        request: &wire::PartialRequest,
        farmer: &Farmer<wire::PublicKey>,
    ) -> Result<(PostPartialResponse, Bytes32), PoolError> {
        let payload = &request.owner.payload.partial;
        let plot_key = wire::PublicKey(request.plot_authorization.public_key);
        if dg_xch_core::signatures::falcon_block::public_key_id(&plot_key.0)
            != payload.proof_of_space.plot_public_key
        {
            return Err(failure(
                PoolErrorCode::InvalidSignature,
                "Falcon plot key does not match proof",
            ));
        }
        wire::verify(
            &request.plot_authorization,
            &plot_key,
            self.chain.genesis(),
            self.info.target_puzzle_hash,
            "partial_plot",
            &request.owner.payload,
        )
        .map_err(|e| failure(PoolErrorCode::InvalidSignature, e.to_string()))?;
        let timestamp = now()?;
        let id = self
            .chain
            .falcon_partial(payload, farmer, timestamp)
            .await
            .map_err(|e| failure(PoolErrorCode::InvalidProof, e.to_string()))?;
        self.store
            .lock()
            .await
            .credit_partial(id, farmer, timestamp)
            .await
            .map_err(|_| {
                failure(
                    PoolErrorCode::RequestFailed,
                    "duplicate partial or changed farmer registration",
                )
            })?;
        Ok((
            PostPartialResponse {
                new_difficulty: farmer.difficulty,
            },
            id,
        ))
    }
}

#[cfg(all(test, feature = "falcon"))]
mod tests {
    use super::*;
    use crate::service::{Membership, PoolChain};
    use crate::store::PoolStore;
    use async_trait::async_trait;
    use dg_xch_core::{
        blockchain::{proof_of_space::ProofOfSpace, sized_bytes::Bytes48},
        protocols::pool::{GetPoolInfoResponse, PostPartialPayload},
        signatures::{
            SignatureProfile, SignatureSigner, falcon::FalconSigner, falcon_block::public_key_id,
        },
    };
    use std::{
        io::Error,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    use tokio::sync::{Mutex, Semaphore};

    struct Chain {
        owner: wire::PublicKey,
        second_owner: wire::PublicKey,
        active: AtomicBool,
    }
    #[async_trait]
    impl PoolChain for Chain {
        fn genesis(&self) -> Bytes32 {
            [1; 32].into()
        }
        fn signature_profile(&self) -> SignatureProfile {
            SignatureProfile::FnDsa512V2
        }
        async fn membership(
            &self,
            _: PoolVersion,
            _: Bytes32,
            _: Bytes48,
        ) -> Result<Membership, Error> {
            Err(Error::other("BLS is disabled"))
        }
        async fn partial(
            &self,
            _: &PostPartialPayload,
            _: &Farmer,
            _: u64,
        ) -> Result<Bytes32, Error> {
            Err(Error::other("BLS is disabled"))
        }
        async fn falcon_membership(
            &self,
            launcher: Bytes32,
            key: wire::PublicKey,
        ) -> Result<Membership<wire::PublicKey>, Error> {
            if !((key == self.owner && launcher == [3; 32].into())
                || (key == self.second_owner && launcher == [11; 32].into()))
                || !self.active.load(Ordering::Relaxed)
            {
                return Err(Error::other("inactive or wrong owner"));
            }
            Ok(Membership {
                owner_public_key: key,
                contract_puzzle_hash: [4; 32].into(),
            })
        }
        async fn falcon_partial(
            &self,
            payload: &PostPartialPayload,
            farmer: &Farmer<wire::PublicKey>,
            _: u64,
        ) -> Result<Bytes32, Error> {
            // This backend isolates authentication/accounting from proof validation.
            // Native PoS2 verification is exercised by the plot/consensus tests.
            if payload.proof_of_space.pool_contract_puzzle_hash != Some(farmer.contract_puzzle_hash)
            {
                return Err(Error::other("wrong contract"));
            }
            Ok(payload.sp_hash)
        }
    }

    #[tokio::test]
    async fn falcon_registration_partials_settings_and_restart_preserve_authentication() {
        let directory = tempfile::tempdir().unwrap();
        let owner = FalconSigner::generate().unwrap();
        let plot = FalconSigner::generate().unwrap();
        let stranger = FalconSigner::generate().unwrap();
        let genesis = [1; 32].into();
        let target = [2; 32].into();
        let launcher = [3; 32].into();
        let chain = Arc::new(Chain {
            owner: wire::PublicKey(owner.public_key()),
            second_owner: wire::PublicKey(stranger.public_key()),
            active: AtomicBool::new(true),
        });
        let pool = PoolService {
            info: GetPoolInfoResponse {
                name: "Falcon test".into(),
                description: String::new(),
                logo_url: String::new(),
                minimum_difficulty: 2,
                relative_lock_height: 100,
                protocol_version: 3,
                fee: "0".into(),
                target_puzzle_hash: target,
                authentication_token_timeout: 5,
            },
            fee_basis_points: 0,
            pool_memoization: "80".into(),
            enable_v2: false,
            store: Mutex::new(
                PoolStore::open(&directory.path().join("pool.sqlite"), genesis, target)
                    .await
                    .unwrap(),
            ),
            chain: chain.clone(),
            requests: Semaphore::new(4),
        };
        let registration = wire::Registration {
            launcher_id: launcher,
            payout_puzzle_hash: [5; 32].into(),
            timestamp: now().unwrap(),
        };
        let bad = wire::sign(&stranger, genesis, target, "register", registration.clone()).unwrap();
        assert!(pool.register_falcon(bad).await.is_err());
        let signed = wire::sign(&owner, genesis, target, "register", registration.clone()).unwrap();
        assert_eq!(
            pool.register_falcon(signed.clone())
                .await
                .unwrap()
                .current_difficulty,
            2
        );
        pool.register_falcon(signed).await.unwrap();
        let mut stale = registration.clone();
        stale.timestamp = 1;
        assert!(
            pool.register_falcon(wire::sign(&owner, genesis, target, "register", stale).unwrap())
                .await
                .is_err()
        );
        let login_proof = wire::BrowserProof {
            launcher_id: launcher,
            linked_launcher_id: None,
            action: wire::BrowserAction::Login,
            nonce: "ab".repeat(32),
            timestamp: now().unwrap(),
        };
        let login = wire::BrowserAuthorization {
            owner: wire::sign(&owner, genesis, target, "web_login", login_proof.clone()).unwrap(),
            linked_authorization: None,
        };
        let mut bad_login = login.clone();
        bad_login.owner.payload.action = wire::BrowserAction::Unlink;
        assert!(pool.browser_login(bad_login).await.is_err());
        let mut wrong_domain = login.clone();
        wrong_domain.owner =
            wire::sign(&owner, genesis, target, "get_farmer", login_proof.clone()).unwrap();
        assert!(pool.browser_login(wrong_domain).await.is_err());
        let ticket = pool.browser_login(login.clone()).await.unwrap();
        assert!(pool.browser_login(login).await.is_err());
        assert!(
            pool.store
                .lock()
                .await
                .exchange_browser_ticket(&ticket.ticket, now().unwrap())
                .await
                .is_ok()
        );
        let mut link_proof = login_proof;
        link_proof.nonce = "cd".repeat(32);
        link_proof.action = wire::BrowserAction::Link;
        link_proof.linked_launcher_id = Some([11; 32].into());
        let missing_second_owner = wire::BrowserAuthorization {
            owner: wire::sign(&owner, genesis, target, "web_login", link_proof.clone()).unwrap(),
            linked_authorization: None,
        };
        assert!(
            pool.browser_login(missing_second_owner.clone())
                .await
                .is_err()
        );
        pool.register_falcon(
            wire::sign(
                &stranger,
                genesis,
                target,
                "register",
                wire::Registration {
                    launcher_id: [11; 32].into(),
                    payout_puzzle_hash: [12; 32].into(),
                    timestamp: now().unwrap(),
                },
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let mut link = missing_second_owner;
        link.linked_authorization = Some(
            wire::sign(&owner, genesis, target, "web_link", link_proof.clone())
                .unwrap()
                .authorization,
        );
        assert!(pool.browser_login(link.clone()).await.is_err());
        link.linked_authorization = Some(
            wire::sign(&stranger, genesis, target, "web_link", link_proof)
                .unwrap()
                .authorization,
        );
        let linked_ticket = pool.browser_login(link).await.unwrap();
        assert_eq!(linked_ticket.linked_farmers, 2);
        let partial = wire::Partial {
            timestamp: now().unwrap(),
            partial: PostPartialPayload {
                launcher_id: launcher,
                authentication_token: 0,
                proof_of_space: ProofOfSpace::v2(
                    [6; 32].into(),
                    None,
                    Some([4; 32].into()),
                    public_key_id(&plot.public_key()),
                    0,
                    0,
                    2,
                    Vec::new().into(),
                ),
                sp_hash: [7; 32].into(),
                end_of_sub_slot: false,
                harvester_id: [8; 32].into(),
            },
        };
        let request = wire::PartialRequest {
            owner: wire::sign(&owner, genesis, target, "partial_owner", partial.clone()).unwrap(),
            plot_authorization: wire::sign(&plot, genesis, target, "partial_plot", partial.clone())
                .unwrap()
                .authorization,
        };
        let mut wrong = request.clone();
        wrong.plot_authorization = wire::sign(&stranger, genesis, target, "partial_plot", partial)
            .unwrap()
            .authorization;
        assert!(pool.partial_falcon(wrong).await.is_err());
        pool.partial_falcon(request.clone()).await.unwrap();
        assert!(pool.partial_falcon(request).await.is_err());
        assert_eq!(pool.store.lock().await.points(launcher).await.unwrap(), 2);
        let update = wire::sign(
            &owner,
            genesis,
            target,
            "update_farmer",
            wire::FarmerUpdate {
                suggested_difficulty: Some(3),
                payout_puzzle_hash: [9; 32].into(),
                launcher_id: registration.launcher_id,
                timestamp: registration.timestamp,
            },
        )
        .unwrap();
        assert_eq!(
            pool.update_falcon(update.clone())
                .await
                .unwrap()
                .payout_puzzle_hash,
            [9; 32].into()
        );
        assert!(pool.update_falcon(update).await.is_err());
        let invalid = wire::sign(
            &owner,
            genesis,
            target,
            "update_farmer",
            wire::FarmerUpdate {
                launcher_id: launcher,
                payout_puzzle_hash: [9; 32].into(),
                timestamp: registration.timestamp + 1,
                suggested_difficulty: Some(1),
            },
        )
        .unwrap();
        assert!(pool.update_falcon(invalid).await.is_err());
        let query = wire::sign(
            &owner,
            genesis,
            target,
            "get_farmer",
            wire::FarmerQuery {
                launcher_id: launcher,
                timestamp: now().unwrap(),
            },
        )
        .unwrap();
        assert_eq!(
            pool.get_falcon(query.clone()).await.unwrap().current_points,
            2
        );
        chain.active.store(false, Ordering::Relaxed);
        assert!(pool.get_falcon(query).await.is_err());
        drop(pool);
        let mut store = PoolStore::open(&directory.path().join("pool.sqlite"), genesis, target)
            .await
            .unwrap();
        let saved = store
            .farmer_typed::<wire::PublicKey>(launcher)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.owner_public_key, wire::PublicKey(owner.public_key()));
        assert_eq!(saved.payout_puzzle_hash, [9; 32].into());
        assert_eq!(store.stats().await.unwrap().accepted_partials, 1);
        let distribution = store
            .prepare_distribution([10; 32].into(), 100, 0, 0, now().unwrap())
            .await
            .unwrap();
        assert_eq!(distribution.payouts[0].puzzle_hash, [9; 32].into());
        assert_eq!(distribution.payouts[0].amount, 100);
    }
}
