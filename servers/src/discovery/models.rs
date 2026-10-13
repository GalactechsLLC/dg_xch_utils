use dg_xch_core::{blockchain::peer_info::TimestampedPeerInfo, protocols::shared::Handshake};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Error, net::SocketAddr};

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Unknown,
    TcpConnect,
    Tls,
    Websocket,
    Handshake,
    PeerRequest,
    Verification,
}
impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::TcpConnect => "tcp_connect",
            Self::Tls => "tls",
            Self::Websocket => "websocket",
            Self::Handshake => "handshake",
            Self::PeerRequest => "peer_request",
            Self::Verification => "verification",
        }
    }
    pub fn parse(value: &str) -> Result<Self, Error> {
        match value {
            "unknown" => Ok(Self::Unknown),
            "tcp_connect" => Ok(Self::TcpConnect),
            "tls" => Ok(Self::Tls),
            "websocket" => Ok(Self::Websocket),
            "handshake" => Ok(Self::Handshake),
            "peer_request" => Ok(Self::PeerRequest),
            "verification" => Ok(Self::Verification),
            _ => Err(Error::other("invalid stored probe phase")),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Crawler,
    Introducer,
}
impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crawler => "crawler",
            Self::Introducer => "introducer",
        }
    }
}
#[derive(Default, Debug)]
pub struct Observation {
    pub handshake: Option<Handshake>,
    pub node_id: Option<String>,
    pub connect_ms: Option<f64>,
    pub peer_request_ms: Option<f64>,
    pub peers: Vec<TimestampedPeerInfo>,
    pub peak_height: Option<u32>,
    pub error: Option<String>,
    pub phase: Phase,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Peer {
    pub endpoint: SocketAddr,
    pub first_seen: u64,
    pub last_seen: u64,
    pub first_gossiped: Option<u64>,
    pub last_gossiped: Option<u64>,
    pub reported_gossip_timestamp: Option<u64>,
    pub first_source: String,
    pub first_success: Option<u64>,
    pub last_success: Option<u64>,
    pub last_attempt: Option<u64>,
    pub next_attempt: u64,
    pub attempts: u64,
    pub successes: u64,
    pub consecutive_failures: u32,
    pub node_id: Option<String>,
    pub software_version: Option<String>,
    pub protocol_version: Option<String>,
    pub node_type: Option<u8>,
    pub advertised_port: Option<u16>,
    pub capabilities: Vec<(u16, String)>,
    pub connect_ms: Option<f64>,
    pub peer_request_ms: Option<f64>,
    pub last_peer_response: Option<u64>,
    pub peak_height: Option<u32>,
    pub peak_observed_at: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_phase: Option<Phase>,
    pub last_error_at: Option<u64>,
    pub verified_at: Option<u64>,
    pub verified_until: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Totals {
    pub attempts: u64,
    pub connections: u64,
    pub peer_responses: u64,
    pub failures: u64,
    pub gossip_received: u64,
    pub gossip_rejected: u64,
    pub pruned: u64,
    pub verification_expired: u64,
    pub connect_seconds_sum: f64,
    pub request_seconds_sum: f64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Point {
    pub timestamp: u64,
    pub known: usize,
    pub recent: usize,
    pub identities: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub timestamp: u64,
    pub network: String,
    pub known: usize,
    pub ever_connected: usize,
    pub recent: usize,
    pub recent_window_seconds: u64,
    pub unique_recent_identities: usize,
    pub due: usize,
    pub inflight: usize,
    pub ipv4_recent: usize,
    pub ipv6_recent: usize,
    pub versions: BTreeMap<String, usize>,
    pub protocols: BTreeMap<String, usize>,
    pub other_versions: usize,
    pub other_protocols: usize,
    pub average_connect_ms: Option<f64>,
    pub average_peer_request_ms: Option<f64>,
    pub totals: Totals,
    pub storage_error: Option<String>,
    pub seed_error: Option<String>,
    pub attempted: usize,
    pub never_attempted: usize,
    pub failed_latest: usize,
    pub pending_retry: usize,
    pub error_phases: BTreeMap<String, usize>,
    pub ipv4_known: usize,
    pub ipv6_known: usize,
    pub ipv6_attempted: usize,
    pub configured_concurrency: usize,
    pub interval_ms: u64,
    pub timeout_seconds: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub concurrency: usize,
    pub interval_ms: u64,
    pub timeout_seconds: u64,
    pub revisit_seconds: u64,
    pub cooldown_seconds: u64,
    pub allow_private: bool,
}
#[derive(Clone, Debug)]
pub struct Claim {
    pub endpoint: SocketAddr,
    pub token: uuid::Uuid,
    pub role: Role,
    pub policy: Policy,
    pub failures: u32,
    pub attempt: u64,
    pub started_at: u64,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    LastConnected,
    Endpoint,
    Version,
    FirstSeen,
    Ping,
    LastAttempt,
    NextAttempt,
    Failures,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Asc,
    #[default]
    Desc,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    All,
    Unattempted,
    Failed,
    Connected,
}
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    #[default]
    All,
    Ipv4,
    Ipv6,
}
#[derive(Default, Deserialize)]
pub struct Page {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default)]
    pub order: Direction,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub family: Family,
}
fn default_limit() -> usize {
    50
}
#[derive(Serialize)]
pub struct PeerPage {
    pub total: usize,
    pub peers: Vec<Peer>,
    pub next_cursor: Option<String>,
}
#[derive(Default, Clone)]
pub struct Counts {
    pub known: i64,
    pub verified: i64,
    pub ipv4: i64,
    pub ipv6: i64,
    pub cooldowns: i64,
    pub pending_verification: i64,
}

/// Address policy is shared by discovery ingestion and both outbound workers.
pub fn admissible(endpoint: SocketAddr, allow_private: bool) -> bool {
    if endpoint.port() < 1024 || endpoint.ip().is_unspecified() || endpoint.ip().is_multicast() {
        return false;
    }
    match endpoint.ip() {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            if ip.is_broadcast() || a == 0 || a >= 240 {
                return false;
            }
            allow_private
                || !(ip.is_private()
                    || ip.is_loopback()
                    || ip.is_link_local()
                    || ip.is_documentation()
                    || (a == 100 && (64..=127).contains(&b))
                    || (a == 198 && (18..=19).contains(&b))
                    || (a == 192 && b == 0 && c == 0))
        }
        std::net::IpAddr::V6(ip) => {
            if ip.to_ipv4_mapped().is_some() {
                return false;
            }
            let s = ip.segments();
            allow_private
                || ((s[0] & 0xe000 == 0x2000)
                    && !(ip.is_loopback()
                        || ip.is_unique_local()
                        || ip.is_unicast_link_local()
                        || (s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                        || s[0] == 0x2002
                        || s[0] == 0x3fff))
        }
    }
}
