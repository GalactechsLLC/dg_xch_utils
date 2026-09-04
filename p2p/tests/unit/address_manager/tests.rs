use super::*;

fn peer(host: &str, port: u16, ts: u64) -> TimestampedPeerInfo {
    TimestampedPeerInfo {
        host: host.to_string(),
        port,
        timestamp: ts,
    }
}

#[test]
fn gossiped_peers_are_stored_and_deduped_on_intake() {
    let mut book = AddressBook::new(&P2pSettings::default());
    let accepted = book.insert_many(&[peer("1.1.1.1", 8444, 10), peer("2.2.2.2", 8444, 10)]);
    assert_eq!(accepted, 2);
    // re-gossip of the same endpoints is fully deduped
    let again = book.insert_many(&[peer("1.1.1.1", 8444, 99), peer("2.2.2.2", 8444, 99)]);
    assert_eq!(again, 0);
    assert_eq!(book.len(), 2);
}

#[test]
fn a_flood_of_junk_holds_the_ring_at_its_cap() {
    let settings = P2pSettings {
        host_pool_capacity: 64,
        ..P2pSettings::default()
    };
    let mut book = AddressBook::new(&settings);
    let flood: Vec<_> = (0..100_000u32)
        .map(|i| {
            let o = i.to_le_bytes();
            peer(&format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3]), 8444, 1)
        })
        .collect();
    let accepted = book.insert_many(&flood);
    assert_eq!(book.len(), 64, "ring bounded at capacity under flood");
    assert!(accepted <= 100_000);
}

#[test]
fn take_moves_to_reserved_and_reclaim_returns() {
    let mut book = AddressBook::new(&P2pSettings::default());
    book.insert_many(&[peer("1.1.1.1", 8444, 10)]);
    let taken = book.take().expect("a candidate");
    assert!(book.is_empty(), "taken address leaves the pool");
    // a duplicate gossip while reserved is skipped
    assert_eq!(book.insert_many(&[peer("1.1.1.1", 8444, 20)]), 0);
    book.reclaim(&taken, false);
    assert_eq!(book.len(), 1, "clean disconnect returns to pool");
}

#[test]
fn violation_reclaim_forgets_the_peer() {
    let mut book = AddressBook::new(&P2pSettings::default());
    book.insert_many(&[peer("1.1.1.1", 8444, 10)]);
    let taken = book.take().expect("a candidate");
    book.reclaim(&taken, true);
    assert!(book.is_empty(), "a violating peer is not returned");
}

#[test]
fn self_authority_is_never_stored() {
    let mut book = AddressBook::new(&P2pSettings::default());
    book.add_self("9.9.9.9", 8444);
    let accepted = book.insert_many(&[peer("9.9.9.9", 8444, 10), peer("1.1.1.1", 8444, 10)]);
    assert_eq!(accepted, 1, "own advertised authority is skipped on intake");
}

#[test]
fn age_drops_stale_entries() {
    let mut book = AddressBook::new(&P2pSettings::default());
    book.insert_many(&[peer("1.1.1.1", 8444, 100), peer("2.2.2.2", 8444, 9000)]);
    book.age(10_000, 6000);
    assert_eq!(book.len(), 1, "entry older than threshold aged out");
}

#[test]
fn persist_round_trips_and_dedups_on_reload() {
    let mut a = AddressBook::new(&P2pSettings::default());
    a.insert_many(&[peer("1.1.1.1", 8444, 10), peer("2.2.2.2", 8444, 20)]);
    let blob = a.serialize();
    let mut b = AddressBook::new(&P2pSettings::default());
    assert_eq!(b.load_str(&blob), 2, "restart reloads the persisted pool");
    assert_eq!(b.load_str(&blob), 0, "reload is deduped on intake");
    assert_eq!(b.len(), 2);
}

#[test]
fn fetch_returns_a_bounded_random_subset() {
    let settings = P2pSettings::default();
    let mut book = AddressBook::new(&settings);
    let many: Vec<_> = (0..50u16).map(|i| peer("1.1.1.1", 8000 + i, 1)).collect();
    book.insert_many(&many);
    let sample = book.fetch();
    assert!(sample.len() >= settings.address_lower && sample.len() <= settings.address_upper);
}
