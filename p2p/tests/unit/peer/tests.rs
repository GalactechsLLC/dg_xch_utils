use super::*;

fn ep(p: u16) -> Endpoint {
    ("10.0.0.1".to_string(), p)
}

#[tokio::test]
async fn inbound_cap_is_enforced() {
    let reg = PeerRegistry::new(P2pSettings {
        target_peer_count: 2,
        ..P2pSettings::default()
    });
    assert!(reg.admit_inbound(&ep(1)).await.is_ok());
    assert!(reg.admit_inbound(&ep(2)).await.is_ok());
    assert_eq!(
        reg.admit_inbound(&ep(3)).await.unwrap_err(),
        AdmitError::InboundCapReached
    );
    reg.release_inbound(&ep(1)).await;
    assert!(reg.admit_inbound(&ep(3)).await.is_ok());
}

#[tokio::test]
async fn inbound_flood_plateaus_flat_at_the_cap() {
    let reg = PeerRegistry::new(P2pSettings {
        target_peer_count: 80,
        ..P2pSettings::default()
    });
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for i in 0..10_000u32 {
        let o = i.to_le_bytes();
        let e = (format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3]), 8444);
        match reg.admit_inbound(&e).await {
            Ok(()) => accepted += 1,
            Err(AdmitError::InboundCapReached) => rejected += 1,
            Err(_) => {}
        }
    }
    // the tracked set never grows past the cap: bounded memory under flood (flat RSS)
    assert_eq!(accepted, 80);
    assert_eq!(reg.inbound_count().await, 80);
    assert_eq!(rejected, 10_000 - 80);
}

#[tokio::test]
async fn duplicate_endpoint_is_rejected() {
    let reg = PeerRegistry::new(P2pSettings::default());
    assert!(reg.admit_inbound(&ep(1)).await.is_ok());
    assert_eq!(
        reg.admit_inbound(&ep(1)).await.unwrap_err(),
        AdmitError::DuplicateEndpoint
    );
}

#[tokio::test]
async fn self_authority_is_rejected_both_directions() {
    let reg = PeerRegistry::new(P2pSettings::default());
    reg.add_self(ep(8444)).await;
    assert_eq!(
        reg.admit_inbound(&ep(8444)).await.unwrap_err(),
        AdmitError::SelfConnection
    );
    assert_eq!(
        reg.reserve_outbound(&ep(8444)).await.unwrap_err(),
        AdmitError::SelfConnection
    );
}
