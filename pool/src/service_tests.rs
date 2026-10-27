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

#[tokio::test]
async fn http_health_and_metrics_read_actual_database_counts() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = tempfile::tempdir().unwrap();
    let (mut pool, key, _) = setup(directory.path()).await;
    pool.info.name = "Configured pool name".into();
    pool.register(PoolVersion::V1, registration(PoolVersion::V1, &key))
        .await
        .unwrap();
    let signed_login = browser_request(
        &key,
        None,
        dg_xch_core::protocols::pool_web::BrowserProof {
            launcher_id: [3; 32].into(),
            linked_launcher_id: None,
            action: dg_xch_core::protocols::pool_web::BrowserAction::Login,
            nonce: "12".repeat(32),
            timestamp: now().unwrap(),
        },
        pool.chain.genesis(),
        pool.info.target_puzzle_hash,
    );
    let login_request = serde_json::to_string(&signed_login).unwrap();
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let server = portfu::prelude::ServerBuilder::new()
        .host(addr.ip().to_string())
        .port(addr.port())
        .service(crate::http::service(Arc::new(pool)))
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let start = std::time::Instant::now();
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    for (path, expected) in [
        ("/health", "pool database available"),
        ("/metrics", "chia_pool_registered_farmers 1\n"),
        ("/pool_info", "\"name\":\"Configured pool name\""),
    ] {
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        socket
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 200"));
        assert!(text.contains(expected), "{text}");
        if path == "/pool_info" {
            assert!(text.contains("\"chain_profile\":\"BLS\""), "{text}");
        }
    }
    async fn request(
        addr: std::net::SocketAddr,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> String {
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        socket.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    }
    let signed_response = request(
        addr,
        "POST",
        "/web/login",
        "Content-Type: application/json\r\n",
        &login_request,
    )
    .await;
    assert!(
        signed_response.starts_with("HTTP/1.1 200"),
        "{signed_response}"
    );
    let ticket: dg_xch_core::protocols::pool_web::BrowserTicket =
        serde_json::from_str(signed_response.split_once("\r\n\r\n").unwrap().1).unwrap();
    let unauthorized = request(addr, "GET", "/api/account/partials", "", "").await;
    assert!(unauthorized.starts_with("HTTP/1.1 401"));
    let body = serde_json::json!({"ticket":ticket.ticket}).to_string();
    let cross_origin = request(
        addr,
        "POST",
        "/api/session",
        "Origin: https://attacker.example\r\n",
        &body,
    )
    .await;
    assert!(cross_origin.starts_with("HTTP/1.1 403"));
    let login = request(
        addr,
        "POST",
        "/api/session",
        "Origin: https://localhost\r\n",
        &body,
    )
    .await;
    assert!(login.starts_with("HTTP/1.1 200"), "{login}");
    let cookie = login
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap();
    assert!(cookie.contains("Secure; HttpOnly; SameSite=Strict"));
    assert!(login.contains("cache-control: no-store"));
    let cookie = cookie.split(';').next().unwrap();
    let headers = format!("Cookie: {cookie}\r\n");
    let account = request(addr, "GET", "/api/account", &headers, "").await;
    assert!(account.starts_with("HTTP/1.1 200"));
    assert!(account.contains("payout_puzzle_hash"));
    let replay = request(
        addr,
        "POST",
        "/api/session",
        "Origin: https://localhost\r\n",
        &body,
    )
    .await;
    assert!(replay.starts_with("HTTP/1.1 401"));
    let logout = request(
        addr,
        "POST",
        "/api/logout",
        &format!("{headers}Origin: https://localhost\r\n"),
        "{}",
    )
    .await;
    assert!(logout.starts_with("HTTP/1.1 403"));
    for path in [
        "/",
        "/farmers",
        "/leaderboards",
        "/blocks",
        "/payouts",
        "/pool-status",
        "/login",
        "/account",
    ] {
        let page = request(addr, "GET", path, "", "").await;
        assert!(page.starts_with("HTTP/1.1 200"), "{path}: {page}");
        assert!(page.contains("frame-ancestors 'none'"));
    }
    handle.shutdown();
    task.await.unwrap().unwrap();
}

fn browser_request(
    key: &SecretKey,
    linked: Option<&SecretKey>,
    proof: dg_xch_core::protocols::pool_web::BrowserProof,
    genesis: Bytes32,
    target: Bytes32,
) -> dg_xch_core::protocols::pool_web::BlsBrowserAuthorization {
    use dg_xch_core::protocols::pool_web as web;
    let authorize = |key: &SecretKey, operation| web::BlsAuthorization {
        public_key: key.sk_to_pk().to_bytes().into(),
        signature: sign(
            key,
            &web::bls_message(genesis, target, operation, &proof).unwrap(),
        )
        .to_bytes()
        .into(),
    };
    web::BlsBrowserAuthorization {
        owner: authorize(key, "web_login"),
        linked_authorization: linked.map(|key| authorize(key, "web_link")),
        proof,
    }
}

#[tokio::test]
async fn bls_browser_login_links_two_owners_and_rejects_replay_and_cross_profile_proofs() {
    use dg_xch_core::{protocols::pool_web as web, signatures::SignatureProfile};
    struct BrowserChain {
        owners: std::collections::HashMap<Bytes32, Bytes48>,
        active: AtomicBool,
        profile: SignatureProfile,
    }
    #[async_trait]
    impl PoolChain for BrowserChain {
        fn signature_profile(&self) -> SignatureProfile {
            self.profile
        }
        async fn membership(
            &self,
            _: PoolVersion,
            launcher: Bytes32,
            _: Bytes48,
        ) -> Result<Membership, Error> {
            if !self.active.load(Ordering::Relaxed) {
                return Err(Error::other("membership changed"));
            }
            Ok(Membership {
                owner_public_key: *self
                    .owners
                    .get(&launcher)
                    .ok_or_else(|| Error::other("unknown owner"))?,
                contract_puzzle_hash: [4; 32].into(),
            })
        }
        async fn partial(
            &self,
            _: &PostPartialPayload,
            _: &Farmer,
            _: u64,
        ) -> Result<Bytes32, Error> {
            Ok([9; 32].into())
        }
    }
    for version in [PoolVersion::V1, PoolVersion::V2] {
        let directory = tempfile::tempdir().unwrap();
        let (mut service, first, _) = setup(directory.path()).await;
        let second = SecretKey::key_gen(&[43; 32], &[]).unwrap();
        let ids = [Bytes32::from([3; 32]), Bytes32::from([7; 32])];
        let owners = [
            (ids[0], first.sk_to_pk().to_bytes().into()),
            (ids[1], second.sk_to_pk().to_bytes().into()),
        ]
        .into_iter()
        .collect();
        let chain = Arc::new(BrowserChain {
            owners,
            active: AtomicBool::new(true),
            profile: SignatureProfile::Bls12381Aug,
        });
        service.chain = chain.clone();
        for (launcher, key) in [(ids[0], &first), (ids[1], &second)] {
            service
                .store
                .lock()
                .await
                .register(&Farmer::<Bytes48> {
                    version,
                    launcher_id: launcher,
                    owner_public_key: key.sk_to_pk().to_bytes().into(),
                    authentication_public_key: key.sk_to_pk().to_bytes().into(),
                    contract_puzzle_hash: [4; 32].into(),
                    payout_puzzle_hash: launcher,
                    difficulty: 1,
                })
                .await
                .unwrap();
        }
        // Both BLS protocols must feed the same diagnostics as Falcon.
        let payload = PostPartialPayload {
            launcher_id: ids[0],
            authentication_token: 0,
            proof_of_space: dg_xch_core::blockchain::proof_of_space::ProofOfSpace::v2(
                [6; 32].into(),
                None,
                Some([4; 32].into()),
                first.sk_to_pk().to_bytes().into(),
                0,
                0,
                2,
                Vec::new().into(),
            ),
            sp_hash: [7; 32].into(),
            end_of_sub_slot: false,
            harvester_id: [8; 32].into(),
        };
        let message = hash_256(payload.to_bytes(ChiaProtocolVersion::Chia0_0_37).unwrap());
        let signature = sign(&first, &message);
        let request = PostPartialRequest {
            payload,
            aggregate_signature: blst::min_pk::AggregateSignature::aggregate(
                &[&signature, &signature],
                true,
            )
            .unwrap()
            .to_signature()
            .to_bytes()
            .into(),
        };
        let farmer = service
            .store
            .lock()
            .await
            .farmer(ids[0])
            .await
            .unwrap()
            .unwrap();
        service
            .partial(farmer.clone(), request.clone())
            .await
            .unwrap();
        assert!(service.partial(farmer, request).await.is_err());
        let overview = service
            .store
            .lock()
            .await
            .dashboard(
                "overview",
                &crate::store::DashboardQuery::default(),
                None,
                now().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            overview["diagnostics"],
            serde_json::json!([
                {"status":"accepted", "count":1}, {"status":"duplicate", "count":1}
            ])
        );
        let timestamp = now().unwrap();
        let proof = web::BrowserProof {
            launcher_id: ids[0],
            linked_launcher_id: None,
            action: web::BrowserAction::Login,
            nonce: "11".repeat(32),
            timestamp,
        };
        let request = browser_request(
            &first,
            None,
            proof.clone(),
            service.chain.genesis(),
            service.info.target_puzzle_hash,
        );
        let ticket = service.browser_login_bls(request.clone()).await.unwrap();
        assert!(service.browser_login_bls(request).await.is_err());
        let (old_token, session) = service
            .store
            .lock()
            .await
            .exchange_browser_ticket(&ticket.ticket, timestamp)
            .await
            .unwrap();
        assert_eq!(
            service
                .store
                .lock()
                .await
                .account_farmers(&session)
                .await
                .unwrap()
                .len(),
            1
        );
        let link = web::BrowserProof {
            action: web::BrowserAction::Link,
            linked_launcher_id: Some(ids[1]),
            nonce: "22".repeat(32),
            ..proof.clone()
        };
        let signed = browser_request(
            &first,
            Some(&second),
            link.clone(),
            service.chain.genesis(),
            service.info.target_puzzle_hash,
        );
        let mut missing = signed.clone();
        missing.linked_authorization = None;
        assert!(service.browser_login_bls(missing).await.is_err());
        let wrong_owner = browser_request(
            &first,
            Some(&first),
            link.clone(),
            service.chain.genesis(),
            service.info.target_puzzle_hash,
        );
        assert!(service.browser_login_bls(wrong_owner).await.is_err());
        for (genesis, target) in [
            ([9; 32].into(), service.info.target_puzzle_hash),
            (service.chain.genesis(), [9; 32].into()),
        ] {
            assert!(
                service
                    .browser_login_bls(browser_request(
                        &first,
                        Some(&second),
                        link.clone(),
                        genesis,
                        target
                    ))
                    .await
                    .is_err()
            );
        }
        let mut tampered = signed.clone();
        tampered.proof.nonce = "33".repeat(32);
        assert!(service.browser_login_bls(tampered).await.is_err());
        let ticket = service.browser_login_bls(signed.clone()).await.unwrap();
        assert_eq!(ticket.linked_farmers, 2);
        assert!(
            service
                .store
                .lock()
                .await
                .browser_session(&old_token, timestamp)
                .await
                .unwrap()
                .is_none()
        );
        assert!(service.browser_login_bls(signed).await.is_err());
        let (_, session) = service
            .store
            .lock()
            .await
            .exchange_browser_ticket(&ticket.ticket, timestamp)
            .await
            .unwrap();
        assert_eq!(
            service
                .store
                .lock()
                .await
                .account_farmers(&session)
                .await
                .unwrap()
                .len(),
            2
        );
        let stale = web::BrowserProof {
            timestamp: timestamp - 120,
            nonce: "44".repeat(32),
            ..proof.clone()
        };
        assert!(
            service
                .browser_login_bls(browser_request(
                    &first,
                    None,
                    stale,
                    service.chain.genesis(),
                    service.info.target_puzzle_hash
                ))
                .await
                .is_err()
        );
        let fresh = web::BrowserProof {
            nonce: "55".repeat(32),
            ..proof
        };
        let request = browser_request(
            &first,
            None,
            fresh,
            service.chain.genesis(),
            service.info.target_puzzle_hash,
        );
        chain.active.store(false, Ordering::Relaxed);
        assert!(service.browser_login_bls(request.clone()).await.is_err());
        service.chain = Arc::new(BrowserChain {
            owners: chain.owners.clone(),
            active: AtomicBool::new(true),
            profile: SignatureProfile::FnDsa512V2,
        });
        assert!(service.browser_login_bls(request).await.is_err());
    }
}
