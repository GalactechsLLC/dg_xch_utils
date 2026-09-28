use dg_xch_core::blockchain::sized_bytes::Bytes32;
use ipnet::IpNet;
use log::warn;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

// Stock full-node subscription caps.
const MAX_SUBSCRIBE_ITEMS: usize = 200_000;
const TRUSTED_MAX_SUBSCRIBE_ITEMS: usize = 2_000_000;
const MAX_SUBSCRIBE_RESPONSE_ITEMS: usize = 100_000;
const TRUSTED_MAX_SUBSCRIBE_RESPONSE_ITEMS: usize = 500_000;

/// The trusted-peer policy: the set of trusted peer node ids (cert hashes), the list of trusted
/// CIDR networks, and the untrusted/trusted cap pairs. Shared as an `Arc` between the
/// [`crate::wallet::WalletNotifier`] (subscription caps), the full-node api (response-item cap), and
/// the gossip transaction queue (priority tier). The default (empty node-id set, empty CIDR
/// list): every REMOTE peer untrusted, but a LOCALHOST peer auto-trusted.
#[derive(Debug, Clone)]
pub struct TrustPolicy {
    trusted: HashSet<Bytes32>,
    trusted_cidrs: Vec<IpNet>,
    max_subscribe_items: usize,
    trusted_max_subscribe_items: usize,
    max_subscribe_response_items: usize,
    trusted_max_subscribe_response_items: usize,
}

impl Default for TrustPolicy {
    /// The default policy: an empty trusted node-id set and empty CIDR list at the stock caps.
    /// Every remote peer resolves untrusted; a localhost peer resolves trusted.
    fn default() -> Self {
        Self {
            trusted: HashSet::new(),
            trusted_cidrs: Vec::new(),
            max_subscribe_items: MAX_SUBSCRIBE_ITEMS,
            trusted_max_subscribe_items: TRUSTED_MAX_SUBSCRIBE_ITEMS,
            max_subscribe_response_items: MAX_SUBSCRIBE_RESPONSE_ITEMS,
            trusted_max_subscribe_response_items: TRUSTED_MAX_SUBSCRIBE_RESPONSE_ITEMS,
        }
    }
}

impl TrustPolicy {
    /// Build a policy from a set of trusted node ids at the stock caps (no trusted CIDRs).
    #[must_use]
    pub fn new(trusted: HashSet<Bytes32>) -> Self {
        Self {
            trusted,
            ..Self::default()
        }
    }

    /// Build a policy from the runtime config's `trusted_peers` list of hex node-id (cert-hash)
    /// strings. A malformed hex entry is skipped (logged by the caller); an empty list yields the
    /// default (localhost-only trust).
    #[must_use]
    pub fn from_hex_ids(ids: &[String]) -> Self {
        let trusted = ids
            .iter()
            .filter_map(|s| Bytes32::from_str(s.trim()).ok())
            .collect();
        Self::new(trusted)
    }

    /// The production constructor — the two config inputs: `trusted_peers` (hex node-id /
    /// cert-hash strings) AND `trusted_cidrs` (CIDR strings). A malformed node-id is skipped
    /// silently; a malformed CIDR is logged and skipped. Empty lists yield the default
    /// (localhost auto-trusted, every other peer untrusted).
    #[must_use]
    pub fn from_config(trusted_peers: &[String], trusted_cidrs: &[String]) -> Self {
        let mut policy = Self::from_hex_ids(trusted_peers);
        policy.trusted_cidrs = Self::parse_cidrs(trusted_cidrs);
        policy
    }

    /// Parse `--trusted-cidr` strings into IPv4/IPv6 CIDR matchers. A malformed entry is logged
    /// and skipped — one bad config line does not sink the node. Both IPv4 (`10.0.0.0/8`) and
    /// IPv6 (`2001:db8::/32`) forms parse.
    fn parse_cidrs(cidrs: &[String]) -> Vec<IpNet> {
        cidrs
            .iter()
            .filter_map(|c| match c.trim().parse::<IpNet>() {
                Ok(net) => Some(net),
                Err(e) => {
                    warn!("skipping malformed --trusted-cidr {c:?}: {e}");
                    None
                }
            })
            .collect()
    }

    /// A policy with explicit caps — the test seam for driving the trusted/untrusted split at
    /// small scale (production uses [`TrustPolicy::from_config`] / [`TrustPolicy::default`]).
    #[must_use]
    pub fn with_caps(
        trusted: HashSet<Bytes32>,
        max_subscribe_items: usize,
        trusted_max_subscribe_items: usize,
        max_subscribe_response_items: usize,
        trusted_max_subscribe_response_items: usize,
    ) -> Self {
        Self {
            trusted,
            trusted_cidrs: Vec::new(),
            max_subscribe_items,
            trusted_max_subscribe_items,
            max_subscribe_response_items,
            trusted_max_subscribe_response_items,
        }
    }

    /// The loopback identities. `SocketPeer.host` is an already-resolved `IpAddr`, so the
    /// hostname form (`"localhost"`) never reaches this layer — only the two literal loopback
    /// IPs, 127.0.0.1 and ::1. Matched EXACTLY (not the whole 127.0.0.0/8 block): 127.0.0.2 is
    /// loopback but is not trusted.
    #[must_use]
    fn is_localhost(host: IpAddr) -> bool {
        host == IpAddr::V4(Ipv4Addr::LOCALHOST) || host == IpAddr::V6(Ipv6Addr::LOCALHOST)
    }

    /// Whether `host` falls inside any configured trusted CIDR. `IpNet::contains` masks host
    /// bits by the network prefix, so an IPv4 host is only ever tested against IPv4 networks and
    /// likewise for IPv6.
    #[must_use]
    fn host_in_trusted_cidrs(&self, host: IpAddr) -> bool {
        self.trusted_cidrs.iter().any(|net| net.contains(&host))
    }

    #[must_use]
    pub fn host_trusted(&self, host: Option<IpAddr>) -> bool {
        matches!(host, Some(ip) if Self::is_localhost(ip) || self.host_in_trusted_cidrs(ip))
    }

    /// Whether `peer` is trusted:
    /// `is_localhost(host) || node_id.hex() in trusted_peers || is_trusted_cidr(host, trusted_cidrs)`.
    /// `host` is the peer's remote IP; `None` (an outbound dial to an unresolved name) means only
    /// the node-id path can grant trust. With empty config only localhost is trusted.
    #[must_use]
    pub fn is_trusted(&self, peer: &Bytes32, host: Option<IpAddr>) -> bool {
        if let Some(ip) = host
            && (Self::is_localhost(ip) || self.host_in_trusted_cidrs(ip))
        {
            return true;
        }
        self.trusted.contains(peer)
    }

    /// The per-peer combined subscription cap: the trusted `trusted_max_subscribe_items`
    /// (2,000,000) for a trusted peer, else the untrusted `max_subscribe_items` (200,000).
    #[must_use]
    pub fn max_subscriptions(&self, peer: &Bytes32, host: Option<IpAddr>) -> usize {
        if self.is_trusted(peer, host) {
            self.trusted_max_subscribe_items
        } else {
            self.max_subscribe_items
        }
    }

    /// The initial-state response-item budget: the trusted `trusted_max_subscribe_response_items`
    /// (500,000) for a trusted peer, else the untrusted `max_subscribe_response_items` (100,000).
    #[must_use]
    pub fn max_subscribe_response_items(&self, peer: &Bytes32, host: Option<IpAddr>) -> usize {
        if self.is_trusted(peer, host) {
            self.trusted_max_subscribe_response_items
        } else {
            self.max_subscribe_response_items
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/trust.rs"]
mod tests;
