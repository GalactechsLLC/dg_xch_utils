use super::*;
use blst::min_pk::SecretKey;
use dg_xch_core::clvm::bls_bindings::sign;
use std::sync::atomic::{AtomicBool, Ordering};

struct Chain {
    owner: Bytes48,
    active: AtomicBool,
}

#[async_trait]
impl PoolChain for Chain {
    async fn membership(
        &self,
        _version: PoolVersion,
        _launcher: Bytes32,
        _key: Bytes48,
    ) -> Result<Membership, Error> {
        if !self.active.load(Ordering::Relaxed) {
            return Err(Error::other("membership no longer active"));
        }
        Ok(Membership {
            owner_public_key: self.owner,
            contract_puzzle_hash: [4; 32].into(),
        })
    }
    async fn partial(
        &self,
        _payload: &PostPartialPayload,
        _farmer: &Farmer,
        _now: u64,
    ) -> Result<Bytes32, Error> {
        Err(Error::other("invalid proof"))
    }
}

async fn setup(directory: &std::path::Path) -> (PoolService, SecretKey, Arc<Chain>) {
    let key = SecretKey::key_gen(&[42; 32], &[]).unwrap();
    let chain = Arc::new(Chain {
        owner: key.sk_to_pk().to_bytes().into(),
        active: AtomicBool::new(true),
    });
    let store = PoolStore::open(
        &directory.join("pool.sqlite"),
        [1; 32].into(),
        [2; 32].into(),
    )
    .await
    .unwrap();
    (
        PoolService {
            info: GetPoolInfoResponse {
                name: "test".into(),
                logo_url: String::new(),
                description: String::new(),
                minimum_difficulty: 1,
                relative_lock_height: 100,
                protocol_version: 1,
                fee: "0".into(),
                target_puzzle_hash: [2; 32].into(),
                authentication_token_timeout: 5,
            },
            fee_basis_points: 0,
            pool_memoization: "80".into(),
            enable_v2: true,
            store: Mutex::new(store),
            chain: chain.clone(),
            requests: Semaphore::new(4),
        },
        key,
        chain,
    )
}

fn registration(version: PoolVersion, key: &SecretKey) -> PostFarmerRequest {
    let payload = PostFarmerPayload {
        launcher_id: [3; 32].into(),
        authentication_token: if version == PoolVersion::V1 {
            now().unwrap() / 60 / 5
        } else {
            0
        },
        authentication_public_key: key.sk_to_pk().to_bytes().into(),
        payout_instructions: hex::encode([5; 32]),
        suggested_difficulty: Some(2),
    };
    let signing_key = if version == PoolVersion::V2 {
        dg_xch_keys::derive_path_unhardened(key, vec![12381]).unwrap()
    } else {
        key.clone()
    };
    let signature = sign(
        &signing_key,
        &hash_256(payload.to_bytes(ChiaProtocolVersion::Chia0_0_37).unwrap()),
    )
    .to_bytes()
    .into();
    PostFarmerRequest { payload, signature }
}

#[tokio::test]
async fn v1_registration_and_owner_changes_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let (service, key, chain) = setup(directory.path()).await;
    let request = registration(PoolVersion::V1, &key);
    service
        .register(PoolVersion::V1, request.clone())
        .await
        .unwrap();
    assert!(service.register(PoolVersion::V1, request).await.is_err());
    let payload = PutFarmerPayload {
        launcher_id: [3; 32].into(),
        authentication_token: now().unwrap() / 60 / 5,
        authentication_public_key: None,
        payout_instructions: Some(hex::encode([6; 32])),
        suggested_difficulty: None,
    };
    let signature = sign(
        &key,
        &hash_256(payload.to_bytes(ChiaProtocolVersion::Chia0_0_37).unwrap()),
    )
    .to_bytes()
    .into();
    chain.active.store(false, Ordering::Relaxed);
    assert!(
        service
            .put_v1(PutFarmerRequest { payload, signature })
            .await
            .is_err()
    );
    assert_eq!(
        service
            .store
            .lock()
            .await
            .farmer([3; 32].into())
            .await
            .unwrap()
            .unwrap()
            .payout_puzzle_hash,
        Bytes32::from([5; 32])
    );
}

#[tokio::test]
async fn v2_login_uses_client_signatures_and_rechecks_membership() {
    let directory = tempfile::tempdir().unwrap();
    let (mut service, key, chain) = setup(directory.path()).await;
    let request = registration(PoolVersion::V2, &key);
    service.enable_v2 = false;
    assert!(
        service
            .register(PoolVersion::V2, request.clone())
            .await
            .is_err()
    );
    service.enable_v2 = true;
    service.register(PoolVersion::V2, request).await.unwrap();
    let request = dg_xch_clients::api::pool_v2::login_request(
        [3; 32].into(),
        [2; 32].into(),
        now().unwrap(),
        &key,
    )
    .unwrap();
    let authentication = service.authenticate(request.clone()).await.unwrap();
    let response = service
        .get_v2(pool_v2::GetFarmerRequest {
            launcher_id: [3; 32].into(),
            authentication_token: 0,
            authentication_token_v2: authentication.authentication_token,
            signature: None,
        })
        .await
        .unwrap();
    assert_eq!(response.current_difficulty, 2);
    assert!(
        service
            .farmer(PoolVersion::V1, [3; 32].into())
            .await
            .is_err()
    );
    chain.active.store(false, Ordering::Relaxed);
    assert!(service.authenticate(request).await.is_err());
}
