use super::*;

fn ok(latency_ms: u64) -> FetchOutcome {
    FetchOutcome::Ok {
        blocks: 32,
        latency: Duration::from_millis(latency_ms),
    }
}

fn availability(pm: &PeerManager, id: u64) -> Availability {
    pm.lock().peers.get(&id).unwrap().availability
}

#[test]
fn observe_live_introduces_fresh_and_evicts_departed() {
    let pm = PeerManager::new();
    pm.observe_live(&[1, 2, 3]);
    assert_eq!(
        pm.selectable_count(),
        3,
        "new peers enter as selectable Fresh"
    );
    pm.observe_live(&[1, 2]); // peer 3 gone
    assert_eq!(
        pm.selectable_count(),
        2,
        "a departed peer with no leases is dropped"
    );
}

#[test]
fn three_strikes_demotes_to_suspect_then_a_further_failure_evicts() {
    let pm = PeerManager::new();
    pm.observe_live(&[1]);
    for _ in 0..3 {
        let l = pm.lease(LeasePriority::Fetch).unwrap();
        pm.release(l, FetchOutcome::Timeout);
    }
    assert_eq!(
        availability(&pm, 1),
        Availability::Suspect,
        "3 strikes → Suspect"
    );
    // Suspect is still eligible (hysteresis) — one more lease is grantable.
    let l = pm
        .lease(LeasePriority::Fetch)
        .expect("suspect still leasable");
    pm.release(l, FetchOutcome::Timeout);
    // Further failure evicts and, with no in-flight, drops the peer entirely.
    assert!(
        pm.lease(LeasePriority::Fetch).is_none(),
        "dead peer not selectable"
    );
    assert_eq!(pm.selectable_count(), 0);
}

#[test]
fn a_success_recovers_a_suspect_peer() {
    let pm = PeerManager::new();
    pm.observe_live(&[1]);
    for _ in 0..3 {
        let l = pm.lease(LeasePriority::Fetch).unwrap();
        pm.release(l, FetchOutcome::Reject);
    }
    assert_eq!(availability(&pm, 1), Availability::Suspect);
    let l = pm.lease(LeasePriority::Fetch).unwrap();
    pm.release(l, ok(50));
    assert_eq!(
        availability(&pm, 1),
        Availability::Live,
        "a success recovers the peer"
    );
}

#[test]
fn closed_evicts_immediately() {
    let pm = PeerManager::new();
    pm.observe_live(&[1, 2]);
    let l = pm.lease(LeasePriority::Fetch).unwrap();
    let victim = l.peer_id;
    pm.release(l, FetchOutcome::Closed);
    assert_eq!(pm.selectable_count(), 1, "closed peer evicted");
    assert!(!pm.lock().peers.contains_key(&victim));
}

#[test]
fn p2c_over_two_candidates_returns_the_higher_score_peer() {
    let pm = PeerManager::new();
    pm.observe_live(&[1, 2]);
    // Peer 1 is fast+reliable; peer 2 is slow. With exactly two candidates P2C samples both, so the
    // higher score (peer 1) is returned deterministically.
    for _ in 0..5 {
        let l = pm.lease(LeasePriority::Fetch).unwrap();
        pm.release(l, if l.peer_id == 1 { ok(20) } else { ok(400) });
    }
    // Prime both with their characteristic latencies.
    let l1 = PeerLease {
        peer_id: 1,
        lease_id: 0,
        priority: LeasePriority::Fetch,
        preempted: None,
    };
    pm.release(l1, ok(20));
    let l2 = PeerLease {
        peer_id: 2,
        lease_id: 0,
        priority: LeasePriority::Fetch,
        preempted: None,
    };
    pm.release(l2, ok(400));
    // Bind each score to a local first: `pm.lock()` twice in one expression would re-lock the
    // non-reentrant std mutex on the same thread and self-deadlock.
    let score_1 = pm.lock().peers.get(&1).unwrap().score();
    let score_2 = pm.lock().peers.get(&2).unwrap().score();
    assert!(score_1 > score_2, "the fast peer scores higher");
    let leased = pm.lease(LeasePriority::Fetch).unwrap();
    assert_eq!(
        leased.peer_id, 1,
        "P2C over two candidates picks the higher score"
    );
}

#[test]
fn per_peer_cap_stops_leasing_a_saturated_peer() {
    let pm = PeerManager::new();
    pm.observe_live(&[1]);
    let mut leases = Vec::new();
    for _ in 0..MAX_IN_TRANSIT_PER_PEER {
        leases.push(pm.lease(LeasePriority::Fetch).expect("under cap"));
    }
    assert!(
        pm.lease(LeasePriority::Fetch).is_none(),
        "at the per-peer cap, no fetch lease"
    );
}

#[test]
fn recovery_takes_a_free_fast_peer_without_preempting() {
    let pm = PeerManager::new();
    pm.observe_live(&[1, 2]);
    // Make peer 1 the fast one.
    let l1 = PeerLease {
        peer_id: 1,
        lease_id: 0,
        priority: LeasePriority::Fetch,
        preempted: None,
    };
    pm.release(l1, ok(10));
    let l2 = PeerLease {
        peer_id: 2,
        lease_id: 0,
        priority: LeasePriority::Fetch,
        preempted: None,
    };
    pm.release(l2, ok(500));
    let rec = pm.lease(LeasePriority::Recovery).unwrap();
    assert_eq!(rec.peer_id, 1, "recovery takes the highest-score free peer");
    assert!(
        rec.preempted.is_none(),
        "a free peer was available — no preemption"
    );
}

#[test]
fn recovery_preempts_the_lowest_score_fetch_lease_when_producer_saturated() {
    let pm = PeerManager::new();
    pm.observe_live(&[1, 2]);
    // Score peer 1 fast, peer 2 slow.
    pm.release(
        PeerLease {
            peer_id: 1,
            lease_id: 0,
            priority: LeasePriority::Fetch,
            preempted: None,
        },
        ok(10),
    );
    pm.release(
        PeerLease {
            peer_id: 2,
            lease_id: 0,
            priority: LeasePriority::Fetch,
            preempted: None,
        },
        ok(500),
    );
    // Producer saturates BOTH peers to the cap with FETCH leases.
    let mut fetch_leases = Vec::new();
    for _ in 0..(MAX_IN_TRANSIT_PER_PEER * 2) {
        fetch_leases.push(pm.lease(LeasePriority::Fetch).expect("fill to saturation"));
    }
    assert!(
        pm.lease(LeasePriority::Fetch).is_none(),
        "producer-saturated"
    );
    // Recovery must still obtain a peer by preemption.
    let rec = pm
        .lease(LeasePriority::Recovery)
        .expect("recovery never starves");
    assert_eq!(
        rec.peer_id, 2,
        "preempts the LOWEST-score peer (the slow one)"
    );
    assert!(
        rec.preempted.is_some(),
        "a FETCH lease was preempted for the recovery range"
    );
}

#[test]
fn ewma_smooths_toward_the_sampled_latency() {
    let pm = PeerManager::new();
    pm.observe_live(&[1]);
    for _ in 0..20 {
        let l = pm.lease(LeasePriority::Fetch).unwrap();
        pm.release(l, ok(100));
    }
    let srtt = pm.lock().peers.get(&1).unwrap().srtt;
    assert!(
        (srtt - 0.100).abs() < 0.02,
        "SRTT converges to the steady 100ms sample, got {srtt}"
    );
}
