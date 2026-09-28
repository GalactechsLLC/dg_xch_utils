use super::*;

fn id(byte: u8) -> Bytes32 {
    Bytes32::from([byte; 32])
}

fn ip(s: &str) -> IpAddr {
    s.parse().expect("test IP literal")
}

#[test]
fn is_trusted_resolves_on_node_id_membership() {
    let policy = TrustPolicy::new(HashSet::from([id(0xaa)]));
    assert!(
        policy.is_trusted(&id(0xaa), None),
        "configured node id is trusted"
    );
    assert!(
        !policy.is_trusted(&id(0xbb), None),
        "non-member is untrusted"
    );
}

// Node-id trust is independent of host: trusted with no host AND with an arbitrary
// non-loopback host, while a non-member stays untrusted at that same host.
#[test]
fn node_id_trust_is_independent_of_host() {
    let policy = TrustPolicy::new(HashSet::from([id(0xaa)]));
    assert!(policy.is_trusted(&id(0xaa), None));
    assert!(policy.is_trusted(&id(0xaa), Some(ip("8.8.8.8"))));
    assert!(!policy.is_trusted(&id(0xbb), Some(ip("8.8.8.8"))));
}

// Case 4: a non-localhost, non-CIDR, non-node-id peer is untrusted — the default is unchanged for
// remote peers, and an absent host cannot be trusted.
#[test]
fn empty_config_leaves_remote_peers_untrusted() {
    let policy = TrustPolicy::default();
    assert!(!policy.is_trusted(&id(0xaa), None));
    assert!(!policy.is_trusted(&id(0x00), Some(ip("10.0.0.1"))));
    // 127.0.0.2 is is_loopback() but NOT in the literal localhost set — stays untrusted.
    assert!(!policy.is_trusted(&id(0x00), Some(ip("127.0.0.2"))));
    assert_eq!(
        policy.max_subscriptions(&id(0xaa), Some(ip("10.0.0.1"))),
        MAX_SUBSCRIBE_ITEMS
    );
    assert_eq!(
        policy.max_subscribe_response_items(&id(0xaa), Some(ip("10.0.0.1"))),
        MAX_SUBSCRIBE_RESPONSE_ITEMS
    );
}

// Case 1: a localhost peer (127.0.0.1 / ::1) is trusted even with empty trusted_peers/cidrs, and
// that lifts BOTH caps to the trusted tier — is_localhost short-circuits before the node-id
// map.
#[test]
fn localhost_is_trusted_with_empty_config() {
    let policy = TrustPolicy::default();
    assert!(policy.is_trusted(&id(0x00), Some(ip("127.0.0.1"))));
    assert!(policy.is_trusted(&id(0x00), Some(ip("::1"))));
    assert_eq!(
        policy.max_subscriptions(&id(0x00), Some(ip("127.0.0.1"))),
        TRUSTED_MAX_SUBSCRIBE_ITEMS
    );
    assert_eq!(
        policy.max_subscribe_response_items(&id(0x00), Some(ip("::1"))),
        TRUSTED_MAX_SUBSCRIBE_RESPONSE_ITEMS
    );
}

// Case 2: a peer whose host IP is inside a configured trusted_cidr is trusted (IPv4 and IPv6);
// outside is untrusted; the caps lift for an in-CIDR peer.
#[test]
fn host_in_trusted_cidr_is_trusted() {
    let policy = TrustPolicy::from_config(&[], &["10.0.0.0/8".into(), "2001:db8::/32".into()]);
    assert!(policy.is_trusted(&id(0xbb), Some(ip("10.1.2.3"))));
    assert!(policy.is_trusted(&id(0xbb), Some(ip("2001:db8::dead"))));
    assert!(!policy.is_trusted(&id(0xbb), Some(ip("11.0.0.1"))));
    assert!(!policy.is_trusted(&id(0xbb), Some(ip("2001:dead::1"))));
    assert_eq!(
        policy.max_subscriptions(&id(0xbb), Some(ip("10.9.9.9"))),
        TRUSTED_MAX_SUBSCRIBE_ITEMS
    );
}

// Case 5: malformed --trusted-cidr entries are skipped non-fatally; the surviving valid CIDR
// (here IPv4) still matches. IPv4 + IPv6 matching is covered by `host_in_trusted_cidr_is_trusted`.
#[test]
fn malformed_cidr_entries_skipped_non_fatally() {
    let policy = TrustPolicy::from_config(
        &[],
        &[
            "not-a-cidr".into(),
            "999.0.0.0/8".into(),
            "10.0.0.0/33".into(),
            "192.168.0.0/16".into(),
        ],
    );
    assert!(policy.is_trusted(&id(0x00), Some(ip("192.168.4.4"))));
    assert!(!policy.is_trusted(&id(0x00), Some(ip("172.16.0.1"))));
}

#[test]
fn trusted_peer_gets_trusted_caps_untrusted_gets_untrusted() {
    let policy = TrustPolicy::new(HashSet::from([id(0xaa)]));
    assert_eq!(
        policy.max_subscriptions(&id(0xaa), None),
        TRUSTED_MAX_SUBSCRIBE_ITEMS
    );
    assert_eq!(
        policy.max_subscriptions(&id(0xbb), None),
        MAX_SUBSCRIBE_ITEMS
    );
    assert_eq!(
        policy.max_subscribe_response_items(&id(0xaa), None),
        TRUSTED_MAX_SUBSCRIBE_RESPONSE_ITEMS
    );
    assert_eq!(
        policy.max_subscribe_response_items(&id(0xbb), None),
        MAX_SUBSCRIBE_RESPONSE_ITEMS
    );
}

// The config surface: hex node-id strings parse to the trusted set; a malformed entry is skipped,
// not fatal.
#[test]
fn from_hex_ids_parses_valid_and_skips_malformed() {
    let good = "aa".repeat(32); // 64 hex chars = 32 bytes
    let policy = TrustPolicy::from_hex_ids(&[good, "not-hex".to_string()]);
    assert!(policy.is_trusted(&id(0xaa), None));
    assert!(!policy.is_trusted(&id(0xbb), None));
}
