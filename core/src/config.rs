use crate::blockchain::sized_bytes::{Bytes32, Bytes48};
use crate::consensus::overrides::ConsensusOverrides;
use num_bigint::BigInt;
use std::collections::HashMap;

fn alerts_url() -> String {
    "https://download.chia.net/notify/mainnet_alert.txt".to_string()
}
fn chia_alerts_pubkey() -> String {
    "89b7fd87cb56e926ecefb879a29aae308be01f31980569f6a75a69d2a9a69daefd71fb778d865f7c50d6c967e3025937".to_string()
}
fn chia_ssl_ca() -> CaSsl {
    CaSsl {
        crt: "config/ssl/ca/chia_ca.crt".to_string(),
        key: "config/ssl/ca/chia_ca.key".to_string(),
    }
}
fn crawler() -> CrawlerConfig {
    CrawlerConfig::default()
}
const fn crawler_port() -> u16 {
    8561
}
fn crawler_ssl() -> PrivateSsl {
    PrivateSsl {
        private_crt: "config/ssl/crawler/private_crawler.crt".to_string(),
        private_key: "config/ssl/crawler/private_crawler.key".to_string(),
    }
}
const fn daemon_port() -> u16 {
    55400
}
const fn daemon_max_message_size() -> u32 {
    50_000_000
}
const fn daemon_heartbeat() -> u32 {
    300
}
fn daemon_ssl() -> PrivateSsl {
    PrivateSsl {
        private_crt: "config/ssl/daemon/private_daemon.crt".to_string(),
        private_key: "config/ssl/daemon/private_daemon.key".to_string(),
    }
}
fn dns_servers() -> Vec<String> {
    vec![
        "dns-introducer.chia.net".to_string(),
        "chia.ctrlaltdel.ch".to_string(),
        "seeder.dexie.space".to_string(),
        "chia-seeder.h9.com".to_string(),
        "chia.hoffmang.com".to_string(),
        "seeder.xchpool.org".to_string(),
    ]
}
const fn data_layer_client_timeout() -> usize {
    15
}
fn data_layer_database_path() -> String {
    "data_layer/db/data_layer_CHALLENGE.sqlite".to_string()
}
const fn data_layer_fee() -> usize {
    1_000_000_000
}
fn data_layer_host_ip() -> String {
    "0.0.0.0".to_string()
}
const fn data_layer_host_port() -> u16 {
    8575
}
const fn data_layer_manage_data_interval() -> usize {
    60
}
const fn data_layer_rpc_port() -> u16 {
    8562
}
const fn data_layer_rpc_server_max_request_body_size() -> usize {
    26_214_400
}
fn data_layer_server_files_location() -> String {
    "data_layer/db/server_files_location_CHALLENGE".to_string()
}
fn data_layer_ssl() -> CombinedSsl {
    CombinedSsl {
        private_crt: "config/ssl/data_layer/private_data_layer.crt".to_string(),
        private_key: "config/ssl/data_layer/private_data_layer.key".to_string(),
        public_crt: "config/ssl/data_layer/public_data_layer.crt".to_string(),
        public_key: "config/ssl/data_layer/public_data_layer.key".to_string(),
    }
}
fn data_layer_wallet_peer() -> PeerConfig {
    PeerConfig {
        host: crate::config::self_hostname(),
        port: 9256,
    }
}
const fn default_true() -> bool {
    true
}
const fn farmer_pool_share_threshold() -> usize {
    1000
}
const fn farmer_port() -> u16 {
    8447
}
const fn farmer_rpc_port() -> u16 {
    8559
}
fn farmer_ssl() -> CombinedSsl {
    CombinedSsl {
        private_crt: "config/ssl/farmer/private_farmer.crt".to_string(),
        private_key: "config/ssl/farmer/private_farmer.key".to_string(),
        public_crt: "config/ssl/farmer/public_farmer.crt".to_string(),
        public_key: "config/ssl/farmer/public_farmer.key".to_string(),
    }
}
fn full_node_db_sync() -> String {
    "auto".to_string()
}
fn full_node_peers() -> Vec<PeerConfig> {
    vec![PeerConfig {
        host: self_hostname(),
        port: 8444,
    }]
}
const fn full_node_port() -> u16 {
    8444
}
const fn full_node_db_readers() -> usize {
    4
}
fn full_node_database_path() -> String {
    "db/blockchain_v2_CHALLENGE.sqlite".to_string()
}
fn full_node_peer_db_path() -> String {
    "db/peer_table_node.sqlite".to_string()
}
fn full_node_peers_file_path() -> String {
    "db/peers.dat".to_string()
}
const fn full_node_rpc_port() -> u16 {
    8555
}
const fn full_node_sync_blocks_behind_threshold() -> usize {
    300
}
const fn full_node_short_sync_blocks_behind_threshold() -> usize {
    20
}
const fn full_node_bad_peak_cache_size() -> usize {
    100
}
const fn full_node_peer_connect_interval() -> usize {
    30
}
const fn full_node_peer_connect_timeout() -> usize {
    30
}
const fn full_node_target_peer_count() -> usize {
    80
}
const fn full_node_target_outbound_peer_count() -> usize {
    8
}
const fn full_node_max_inbound_wallet() -> usize {
    20
}
const fn full_node_max_inbound_farmer() -> usize {
    10
}
const fn full_node_max_inbound_timelord() -> usize {
    5
}
const fn full_node_recent_peer_threshold() -> usize {
    6000
}
const fn full_node_target_uncompact_proofs() -> usize {
    100
}
const fn full_node_weight_proof_timeout() -> usize {
    360
}
const fn full_node_max_sync_wait() -> usize {
    30
}
const fn full_node_max_subscribe_items() -> usize {
    200_000
}
const fn full_node_max_subscribe_response_items() -> usize {
    100_000
}
const fn full_node_trusted_max_subscribe_items() -> usize {
    2_000_000
}
const fn full_node_trusted_max_subscribe_response_items() -> usize {
    500_000
}
fn full_node_ssl() -> CombinedSsl {
    CombinedSsl {
        private_crt: "config/ssl/full_node/private_full_node.crt".to_string(),
        private_key: "config/ssl/full_node/private_full_node.key".to_string(),
        public_crt: "config/ssl/full_node/public_full_node.crt".to_string(),
        public_key: "config/ssl/full_node/public_full_node.key".to_string(),
    }
}
const fn harvester_decompressor_timeout() -> usize {
    20
}
fn harvester_farmer_peers() -> Vec<PeerConfig> {
    vec![PeerConfig {
        host: self_hostname(),
        port: 8447,
    }]
}
const fn harvester_max_compression_level_allowed() -> u8 {
    7
}
const fn harvester_num_threads() -> usize {
    30
}
const fn harvester_rpc_port() -> u16 {
    8560
}
fn harvester_ssl() -> PrivateSsl {
    PrivateSsl {
        private_crt: "config/ssl/harvester/private_harvester.crt".to_string(),
        private_key: "config/ssl/harvester/private_harvester.key".to_string(),
    }
}

const fn inbound_rate_limit_percent() -> u8 {
    100
}
fn introducer_peer() -> IntroducerPeer {
    IntroducerPeer {
        host: "introducer.chia.net".to_string(),
        port: 8444,
        enable_private_networks: false,
    }
}
const fn introducer_port() -> u16 {
    8445
}
const fn introducer_max_peers_to_send() -> usize {
    20
}
const fn introducer_recent_peer_threshold() -> usize {
    6000
}
fn introducer_ssl() -> PublicSsl {
    PublicSsl {
        public_crt: "config/ssl/full_node/public_full_node.crt".to_string(),
        public_key: "config/ssl/full_node/public_full_node.key".to_string(),
    }
}
fn logging() -> LoggingConfig {
    LoggingConfig::default()
}
const fn min_mainnet_k_size() -> u8 {
    32
}
fn multiprocessing_start_method() -> String {
    "default".to_string()
}
fn network_overrides() -> NetworkOverrides {
    NetworkOverrides::default()
}
const fn outbound_rate_limit_percent() -> u8 {
    30
}
fn plots_refresh_parameter() -> PlotRefreshParameter {
    PlotRefreshParameter::default()
}
const fn ping_interval() -> u32 {
    120
}
fn private_ssl_ca() -> CaSsl {
    CaSsl {
        crt: "config/ssl/ca/private_ca.crt".to_string(),
        key: "config/ssl/ca/private_ca.key".to_string(),
    }
}
const fn rpc_timeout() -> u32 {
    300
}
const fn seeder_port() -> u16 {
    8444
}
const fn seeder_other_peers_port() -> u16 {
    8444
}
const fn seeder_dns_port() -> u16 {
    53
}
const fn seeder_peer_connect_timeout() -> usize {
    2
}
fn seeder_crawler_db_path() -> String {
    "crawler.db".to_string()
}
fn seeder_bootstrap_peers() -> Vec<String> {
    vec!["node.chia.net".to_string()]
}
const fn seeder_minimum_height() -> usize {
    240_000
}
const fn seeder_minimum_version_count() -> usize {
    100
}
fn seeder_domain_name() -> String {
    "seeder.example.com.".to_string()
}
fn seeder_nameserver() -> String {
    "example.com.".to_string()
}
const fn seeder_ttl() -> usize {
    300
}
fn selected_network() -> String {
    "mainnet".to_string()
}
fn self_hostname() -> String {
    "localhost".to_string()
}
fn simulator_plot_directory() -> String {
    "simulator/plots".to_string()
}
fn ssh_filename() -> String {
    "config/ssh_host_key".to_string()
}
const fn timelord_max_connection_time() -> usize {
    60
}
const fn timelord_rpc_port() -> u16 {
    8557
}
const fn timelord_slow_bluebox_process_count() -> usize {
    1
}
fn timelord_ssl() -> CombinedSsl {
    CombinedSsl {
        private_crt: "config/ssl/timelord/private_timelord.crt".to_string(),
        private_key: "config/ssl/timelord/private_timelord.key".to_string(),
        public_crt: "config/ssl/timelord/public_timelord.crt".to_string(),
        public_key: "config/ssl/timelord/public_timelord.key".to_string(),
    }
}
const fn ui_rpc_port() -> u16 {
    8555
}
fn vdf_clients() -> VdfClients {
    VdfClients {
        ip: vec![
            self_hostname(),
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ],
        ips_estimate: 150_000,
    }
}
fn vdf_server() -> PeerConfig {
    PeerConfig {
        host: self_hostname(),
        port: 8000,
    }
}
const fn wallet_rpc_port() -> u16 {
    9256
}
fn wallet_db_sync() -> String {
    "auto".to_string()
}
const fn wallet_db_readers() -> usize {
    2
}
const fn wallet_initial_num_public_keys() -> usize {
    425
}
fn wallet_nft_metadata_cache_path() -> String {
    "nft_cache".to_string()
}
const fn wallet_nft_metadata_cache_hash_length() -> usize {
    3
}
fn wallet_database_path() -> String {
    "wallet/db/blockchain_wallet_v2_CHALLENGE_KEY.sqlite".to_string()
}
fn wallet_wallet_peers_path() -> String {
    "wallet/db/wallet_peers.sqlite".to_string()
}
fn wallet_wallet_peers_file_path() -> String {
    "wallet/db/wallet_peers.dat".to_string()
}
const fn wallet_target_peer_count() -> usize {
    3
}
const fn wallet_peer_connect_interval() -> usize {
    60
}
const fn wallet_recent_peer_threshold() -> usize {
    6000
}
const fn wallet_short_sync_blocks_behind_threshold() -> usize {
    20
}
const fn wallet_inbound_rate_limit_percent() -> usize {
    100
}
const fn wallet_outbound_rate_limit_percent() -> usize {
    60
}
const fn wallet_weight_proof_timeout() -> usize {
    360
}
const fn wallet_tx_resend_timeout_secs() -> usize {
    1800
}
const fn wallet_spam_filter_after_n_txs() -> usize {
    200
}
const fn wallet_xch_spam_amount() -> usize {
    1_000_000
}
const fn wallet_required_notification_amount() -> usize {
    10_000_000
}
fn wallet_ssl() -> CombinedSsl {
    CombinedSsl {
        private_crt: "config/ssl/wallet/private_wallet.crt".to_string(),
        private_key: "config/ssl/wallet/private_wallet.key".to_string(),
        public_crt: "config/ssl/wallet/public_wallet.crt".to_string(),
        public_key: "config/ssl/wallet/public_wallet.key".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChiaConfig {
    #[serde(default = "min_mainnet_k_size")]
    pub min_mainnet_k_size: u8,
    #[serde(default = "ping_interval")]
    pub ping_interval: u32,
    #[serde(default = "self_hostname")]
    pub self_hostname: String,
    #[serde(default)]
    pub prefer_ipv6: bool,
    #[serde(default = "rpc_timeout")]
    pub rpc_timeout: u32,
    #[serde(default = "daemon_port")]
    pub daemon_port: u16,
    #[serde(default = "daemon_max_message_size")]
    pub daemon_max_message_size: u32,
    #[serde(default = "daemon_heartbeat")]
    pub daemon_heartbeat: u32,
    #[serde(default)]
    pub daemon_allow_tls_1_2: bool,
    #[serde(default = "inbound_rate_limit_percent")]
    pub inbound_rate_limit_percent: u8,
    #[serde(default = "outbound_rate_limit_percent")]
    pub outbound_rate_limit_percent: u8,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "alerts_url", rename = "ALERTS_URL")]
    pub alerts_url: String,
    #[serde(default = "chia_alerts_pubkey", rename = "CHIA_ALERTS_PUBKEY")]
    pub chia_alerts_pubkey: String,
    #[serde(default = "private_ssl_ca")]
    pub private_ssl_ca: CaSsl,
    #[serde(default = "chia_ssl_ca")]
    pub chia_ssl_ca: CaSsl,
    #[serde(default = "daemon_ssl")]
    pub daemon_ssl: PrivateSsl,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub seeder: SeederConfig,
    #[serde(default)]
    pub harvester: HarvesterConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub farmer: FarmerConfig,
    #[serde(default)]
    pub timelord_launcher: TimelordLauncherConfig,
    #[serde(default)]
    pub timelord: TimelordConfig,
    #[serde(default)]
    pub full_node: FullnodeConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub wallet: WalletConfig,
    #[serde(default)]
    pub data_layer: DataLayerConfig,
    #[serde(default)]
    pub simulator: SimulatorConfig,
}
impl Default for ChiaConfig {
    fn default() -> Self {
        ChiaConfig {
            min_mainnet_k_size: min_mainnet_k_size(),
            ping_interval: ping_interval(),
            self_hostname: self_hostname(),
            prefer_ipv6: false,
            rpc_timeout: rpc_timeout(),
            daemon_port: daemon_port(),
            daemon_max_message_size: daemon_max_message_size(),
            daemon_heartbeat: daemon_heartbeat(),
            daemon_allow_tls_1_2: false,
            inbound_rate_limit_percent: inbound_rate_limit_percent(),
            outbound_rate_limit_percent: outbound_rate_limit_percent(),
            network_overrides: NetworkOverrides::default(),
            selected_network: selected_network(),
            alerts_url: alerts_url(),
            chia_alerts_pubkey: chia_alerts_pubkey(),
            private_ssl_ca: private_ssl_ca(),
            chia_ssl_ca: chia_ssl_ca(),
            daemon_ssl: daemon_ssl(),
            logging: LoggingConfig::default(),
            seeder: SeederConfig::default(),
            harvester: HarvesterConfig::default(),
            pool: PoolConfig::default(),
            farmer: FarmerConfig::default(),
            timelord_launcher: TimelordLauncherConfig::default(),
            timelord: TimelordConfig::default(),
            full_node: FullnodeConfig::default(),
            ui: UiConfig::default(),
            wallet: WalletConfig::default(),
            data_layer: DataLayerConfig::default(),
            simulator: SimulatorConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SimulatorConfig {
    #[serde(default = "default_true")]
    pub auto_farm: bool,
    #[serde(default)]
    pub key_fingerprint: Option<String>,
    #[serde(default)]
    pub farming_address: Option<String>,
    #[serde(default = "simulator_plot_directory")]
    pub plot_directory: String,
    #[serde(default = "default_true")]
    pub use_current_time: bool,
}
impl Default for SimulatorConfig {
    fn default() -> Self {
        SimulatorConfig {
            auto_farm: true,
            key_fingerprint: None,
            farming_address: None,
            plot_directory: simulator_plot_directory(),
            use_current_time: true,
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatalayerPlugin {
    pub url: String,
    pub headers: HashMap<String, String>,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatalayerPlugins {
    pub uploaders: Vec<DatalayerPlugin>,
    pub downloaders: Vec<DatalayerPlugin>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DataLayerConfig {
    #[serde(default = "data_layer_wallet_peer")]
    pub wallet_peer: PeerConfig,
    #[serde(default = "data_layer_database_path")]
    pub database_path: String,
    #[serde(default = "data_layer_server_files_location")]
    pub server_files_location: String,
    #[serde(default = "data_layer_client_timeout")]
    pub client_timeout: usize,
    #[serde(default = "data_layer_host_ip")]
    pub host_ip: String,
    #[serde(default = "data_layer_host_port")]
    pub host_port: u16,
    #[serde(default = "data_layer_manage_data_interval")]
    pub manage_data_interval: usize,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "default_true")]
    pub start_rpc_server: bool,
    #[serde(default = "data_layer_rpc_port")]
    pub rpc_port: u16,
    #[serde(default = "data_layer_rpc_server_max_request_body_size")]
    pub rpc_server_max_request_body_size: usize,
    #[serde(default = "data_layer_fee")]
    pub fee: usize,
    #[serde(default)]
    pub log_sqlite_cmds: bool,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default = "data_layer_ssl")]
    pub ssl: CombinedSsl,
    #[serde(default)]
    pub plugins: DatalayerPlugins,
}
impl Default for DataLayerConfig {
    fn default() -> Self {
        DataLayerConfig {
            wallet_peer: data_layer_wallet_peer(),
            database_path: data_layer_database_path(),
            server_files_location: data_layer_server_files_location(),
            client_timeout: data_layer_client_timeout(),
            host_ip: data_layer_host_ip(),
            host_port: data_layer_host_port(),
            manage_data_interval: data_layer_manage_data_interval(),
            selected_network: selected_network(),
            start_rpc_server: true,
            rpc_port: data_layer_rpc_port(),
            rpc_server_max_request_body_size: data_layer_rpc_server_max_request_body_size(),
            fee: data_layer_fee(),
            log_sqlite_cmds: false,
            logging: LoggingConfig::default(),
            ssl: data_layer_ssl(),
            plugins: DatalayerPlugins::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoClaimConfig {
    pub enabled: bool,
    pub tx_fee: usize,
    pub min_amount: usize,
    pub batch_size: usize,
}
impl Default for AutoClaimConfig {
    fn default() -> Self {
        AutoClaimConfig {
            enabled: false,
            tx_fee: 0,
            min_amount: 0,
            batch_size: 50,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WalletConfig {
    #[serde(default = "wallet_rpc_port")]
    pub rpc_port: u16,
    #[serde(default)]
    pub enable_profiler: bool,
    #[serde(default)]
    pub enable_memory_profiler: bool,
    #[serde(default = "wallet_db_sync")]
    pub db_sync: String,
    #[serde(default = "wallet_db_readers")]
    pub db_readers: usize,
    #[serde(default = "default_true")]
    pub connect_to_unknown_peers: bool,
    #[serde(default = "wallet_initial_num_public_keys")]
    pub initial_num_public_keys: usize,
    #[serde(default)]
    pub reuse_public_key_for_change: HashMap<String, bool>,
    #[serde(default = "dns_servers")]
    pub dns_servers: Vec<String>,
    #[serde(default = "full_node_peers")]
    pub full_node_peers: Vec<PeerConfig>,
    #[serde(default = "wallet_nft_metadata_cache_path")]
    pub nft_metadata_cache_path: String,
    #[serde(default = "wallet_nft_metadata_cache_hash_length")]
    pub nft_metadata_cache_hash_length: usize,
    #[serde(default = "multiprocessing_start_method")]
    pub multiprocessing_start_method: String,
    #[serde(default)]
    pub testing: bool,
    #[serde(default = "wallet_database_path")]
    pub database_path: String,
    #[serde(default = "wallet_wallet_peers_path")]
    pub wallet_peers_path: String,
    #[serde(default = "wallet_wallet_peers_file_path")]
    pub wallet_peers_file_path: String,
    #[serde(default)]
    pub log_sqlite_cmds: bool,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "wallet_target_peer_count")]
    pub target_peer_count: usize,
    #[serde(default = "wallet_peer_connect_interval")]
    pub peer_connect_interval: usize,
    #[serde(default = "wallet_recent_peer_threshold")]
    pub recent_peer_threshold: usize,
    #[serde(default = "introducer_peer")]
    pub introducer_peer: IntroducerPeer,
    #[serde(default = "wallet_ssl")]
    pub ssl: CombinedSsl,
    #[serde(default)]
    pub trusted_peers: HashMap<String, String>,
    #[serde(default = "wallet_short_sync_blocks_behind_threshold")]
    pub short_sync_blocks_behind_threshold: usize,
    #[serde(default = "wallet_inbound_rate_limit_percent")]
    pub inbound_rate_limit_percent: usize,
    #[serde(default = "wallet_outbound_rate_limit_percent")]
    pub outbound_rate_limit_percent: usize,
    #[serde(default = "wallet_weight_proof_timeout")]
    pub weight_proof_timeout: usize,
    #[serde(default)]
    pub automatically_add_unknown_cats: bool,
    #[serde(default = "wallet_tx_resend_timeout_secs")]
    pub tx_resend_timeout_secs: usize,
    #[serde(default)]
    pub reset_sync_for_fingerprint: Option<String>,
    #[serde(default = "wallet_spam_filter_after_n_txs")]
    pub spam_filter_after_n_txs: usize,
    #[serde(default = "wallet_xch_spam_amount")]
    pub xch_spam_amount: usize,
    #[serde(default = "default_true")]
    pub enable_notifications: bool,
    #[serde(default = "wallet_required_notification_amount")]
    pub required_notification_amount: usize,
    #[serde(default)]
    pub auto_claim: AutoClaimConfig,
}
impl Default for WalletConfig {
    fn default() -> Self {
        WalletConfig {
            rpc_port: wallet_rpc_port(),
            enable_profiler: false,
            enable_memory_profiler: false,
            db_sync: wallet_db_sync(),
            db_readers: wallet_db_readers(),
            connect_to_unknown_peers: true,
            initial_num_public_keys: wallet_initial_num_public_keys(),
            reuse_public_key_for_change: HashMap::default(),
            dns_servers: dns_servers(),
            full_node_peers: full_node_peers(),
            nft_metadata_cache_path: wallet_nft_metadata_cache_path(),
            nft_metadata_cache_hash_length: wallet_nft_metadata_cache_hash_length(),
            multiprocessing_start_method: multiprocessing_start_method(),
            testing: false,
            database_path: wallet_database_path(),
            wallet_peers_path: wallet_wallet_peers_path(),
            wallet_peers_file_path: wallet_wallet_peers_file_path(),
            log_sqlite_cmds: false,
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            target_peer_count: wallet_target_peer_count(),
            peer_connect_interval: wallet_peer_connect_interval(),
            recent_peer_threshold: wallet_recent_peer_threshold(),
            introducer_peer: introducer_peer(),
            ssl: wallet_ssl(),
            trusted_peers: HashMap::default(),
            short_sync_blocks_behind_threshold: wallet_short_sync_blocks_behind_threshold(),
            inbound_rate_limit_percent: wallet_inbound_rate_limit_percent(),
            outbound_rate_limit_percent: wallet_outbound_rate_limit_percent(),
            weight_proof_timeout: wallet_weight_proof_timeout(),
            automatically_add_unknown_cats: false,
            tx_resend_timeout_secs: wallet_tx_resend_timeout_secs(),
            reset_sync_for_fingerprint: None,
            spam_filter_after_n_txs: wallet_spam_filter_after_n_txs(),
            xch_spam_amount: wallet_xch_spam_amount(),
            enable_notifications: true,
            required_notification_amount: wallet_required_notification_amount(),
            auto_claim: AutoClaimConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IntroducerConfig {
    #[serde(default = "self_hostname")]
    pub host: String,
    #[serde(default = "introducer_port")]
    pub port: u16,
    #[serde(default = "introducer_max_peers_to_send")]
    pub max_peers_to_send: usize,
    #[serde(default = "introducer_recent_peer_threshold")]
    pub recent_peer_threshold: usize,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "introducer_ssl")]
    pub ssl: PublicSsl,
}
impl Default for IntroducerConfig {
    fn default() -> Self {
        IntroducerConfig {
            host: self_hostname(),
            port: introducer_port(),
            max_peers_to_send: introducer_max_peers_to_send(),
            recent_peer_threshold: introducer_recent_peer_threshold(),
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            ssl: introducer_ssl(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UiConfig {
    #[serde(default = "ui_rpc_port")]
    pub rpc_port: u16,
    #[serde(default = "ssh_filename")]
    pub ssh_filename: String,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "self_hostname")]
    pub daemon_host: String,
    #[serde(default = "daemon_port")]
    pub daemon_port: u16,
    #[serde(default = "daemon_ssl")]
    pub daemon_ssl: PrivateSsl,
}
impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            rpc_port: ui_rpc_port(),
            ssh_filename: ssh_filename(),
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            daemon_host: self_hostname(),
            daemon_port: daemon_port(),
            daemon_ssl: daemon_ssl(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IntroducerPeer {
    pub host: String,
    pub port: u16,
    pub enable_private_networks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FullnodeConfig {
    #[serde(default = "full_node_port")]
    pub port: u16,
    #[serde(default = "full_node_db_sync")]
    pub db_sync: String,
    #[serde(default = "full_node_db_readers")]
    pub db_readers: usize,
    #[serde(default = "full_node_database_path")]
    pub database_path: String,
    #[serde(default = "full_node_peer_db_path")]
    pub peer_db_path: String,
    #[serde(default = "full_node_peers_file_path")]
    pub peers_file_path: String,
    #[serde(default = "multiprocessing_start_method")]
    pub multiprocessing_start_method: String,
    #[serde(default = "default_true")]
    pub start_rpc_server: bool,
    #[serde(default = "full_node_rpc_port")]
    pub rpc_port: u16,
    #[serde(default = "default_true")]
    pub enable_upnp: bool,
    #[serde(default = "full_node_sync_blocks_behind_threshold")]
    pub sync_blocks_behind_threshold: usize,
    #[serde(default = "full_node_short_sync_blocks_behind_threshold")]
    pub short_sync_blocks_behind_threshold: usize,
    #[serde(default = "full_node_bad_peak_cache_size")]
    pub bad_peak_cache_size: usize,
    #[serde(default)]
    pub reserved_cores: usize,
    #[serde(default)]
    pub single_threaded: bool,
    #[serde(default = "full_node_peer_connect_interval")]
    pub peer_connect_interval: usize,
    #[serde(default = "full_node_peer_connect_timeout")]
    pub peer_connect_timeout: usize,
    #[serde(default = "full_node_target_peer_count")]
    pub target_peer_count: usize,
    #[serde(default = "full_node_target_outbound_peer_count")]
    pub target_outbound_peer_count: usize,
    #[serde(default)]
    pub exempt_peer_networks: Vec<String>,
    #[serde(default = "full_node_max_inbound_wallet")]
    pub max_inbound_wallet: usize,
    #[serde(default = "full_node_max_inbound_farmer")]
    pub max_inbound_farmer: usize,
    #[serde(default = "full_node_max_inbound_timelord")]
    pub max_inbound_timelord: usize,
    #[serde(default = "full_node_recent_peer_threshold")]
    pub recent_peer_threshold: usize,
    #[serde(default)]
    pub send_uncompact_interval: usize,
    #[serde(default = "full_node_target_uncompact_proofs")]
    pub target_uncompact_proofs: usize,
    #[serde(default)]
    pub sanitize_weight_proof_only: bool,
    #[serde(default = "full_node_weight_proof_timeout")]
    pub weight_proof_timeout: usize,
    #[serde(default = "full_node_max_sync_wait")]
    pub max_sync_wait: usize,
    #[serde(default)]
    pub enable_profiler: bool,
    #[serde(default)]
    pub enable_memory_profiler: bool,
    #[serde(default)]
    pub log_sqlite_cmds: bool,
    #[serde(default = "full_node_max_subscribe_items")]
    pub max_subscribe_items: usize,
    #[serde(default = "full_node_max_subscribe_response_items")]
    pub max_subscribe_response_items: usize,
    #[serde(default = "full_node_trusted_max_subscribe_items")]
    pub trusted_max_subscribe_items: usize,
    #[serde(default = "full_node_trusted_max_subscribe_response_items")]
    pub trusted_max_subscribe_response_items: usize,
    #[serde(default = "dns_servers")]
    pub dns_servers: Vec<String>,
    #[serde(default = "introducer_peer")]
    pub introducer_peer: IntroducerPeer,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default)]
    pub trusted_peers: HashMap<String, String>,
    #[serde(default = "full_node_ssl")]
    pub ssl: CombinedSsl,
    #[serde(default = "default_true")]
    pub use_chia_loop_policy: bool,
}
impl Default for FullnodeConfig {
    fn default() -> Self {
        FullnodeConfig {
            port: full_node_port(),
            db_sync: full_node_db_sync(),
            db_readers: full_node_db_readers(),
            database_path: full_node_database_path(),
            peer_db_path: full_node_peer_db_path(),
            peers_file_path: full_node_peers_file_path(),
            multiprocessing_start_method: multiprocessing_start_method(),
            start_rpc_server: true,
            rpc_port: full_node_rpc_port(),
            enable_upnp: true,
            sync_blocks_behind_threshold: full_node_sync_blocks_behind_threshold(),
            short_sync_blocks_behind_threshold: full_node_short_sync_blocks_behind_threshold(),
            bad_peak_cache_size: full_node_bad_peak_cache_size(),
            reserved_cores: 0,
            single_threaded: false,
            peer_connect_interval: full_node_peer_connect_interval(),
            peer_connect_timeout: full_node_peer_connect_timeout(),
            target_peer_count: full_node_target_peer_count(),
            target_outbound_peer_count: full_node_target_outbound_peer_count(),
            exempt_peer_networks: vec![],
            max_inbound_wallet: full_node_max_inbound_wallet(),
            max_inbound_farmer: full_node_max_inbound_farmer(),
            max_inbound_timelord: full_node_max_inbound_timelord(),
            recent_peer_threshold: full_node_recent_peer_threshold(),
            send_uncompact_interval: 0,
            target_uncompact_proofs: full_node_target_uncompact_proofs(),
            sanitize_weight_proof_only: false,
            weight_proof_timeout: full_node_weight_proof_timeout(),
            max_sync_wait: full_node_max_sync_wait(),
            enable_profiler: false,
            enable_memory_profiler: false,
            log_sqlite_cmds: false,
            max_subscribe_items: full_node_max_subscribe_items(),
            max_subscribe_response_items: full_node_max_subscribe_response_items(),
            trusted_max_subscribe_items: full_node_trusted_max_subscribe_items(),
            trusted_max_subscribe_response_items: full_node_trusted_max_subscribe_response_items(),
            dns_servers: dns_servers(),
            introducer_peer: introducer_peer(),
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            trusted_peers: HashMap::default(),
            ssl: full_node_ssl(),
            use_chia_loop_policy: true,
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VdfClients {
    pub ip: Vec<String>,
    pub ips_estimate: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimelordConfig {
    #[serde(default = "vdf_clients")]
    pub vdf_clients: VdfClients,
    #[serde(default = "full_node_peers")]
    pub full_node_peers: Vec<PeerConfig>,
    #[serde(default = "timelord_max_connection_time")]
    pub max_connection_time: usize,
    #[serde(default = "vdf_server")]
    pub vdf_server: PeerConfig,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default)]
    pub fast_algorithm: bool,
    #[serde(default)]
    pub bluebox_mode: bool,
    #[serde(default)]
    pub slow_bluebox: bool,
    #[serde(default = "timelord_slow_bluebox_process_count")]
    pub slow_bluebox_process_count: usize,
    #[serde(default = "multiprocessing_start_method")]
    pub multiprocessing_start_method: String,
    #[serde(default = "default_true")]
    pub start_rpc_server: bool,
    #[serde(default = "timelord_rpc_port")]
    pub rpc_port: u16,
    #[serde(default = "timelord_ssl")]
    pub ssl: CombinedSsl,
}
impl Default for TimelordConfig {
    fn default() -> Self {
        TimelordConfig {
            vdf_clients: vdf_clients(),
            full_node_peers: full_node_peers(),
            max_connection_time: timelord_max_connection_time(),
            vdf_server: vdf_server(),
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            fast_algorithm: false,
            bluebox_mode: false,
            slow_bluebox: false,
            slow_bluebox_process_count: timelord_slow_bluebox_process_count(),
            multiprocessing_start_method: multiprocessing_start_method(),
            start_rpc_server: true,
            rpc_port: timelord_rpc_port(),
            ssl: timelord_ssl(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TimelordLauncherConfig {
    #[serde(default = "self_hostname")]
    pub host: String,
    pub port: u16,
    pub process_count: usize,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
}
impl Default for TimelordLauncherConfig {
    fn default() -> Self {
        TimelordLauncherConfig {
            host: self_hostname(),
            port: 8000,
            process_count: 3,
            logging: logging(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FarmerConfig {
    #[serde(default = "full_node_peers")]
    pub full_node_peers: Vec<PeerConfig>,
    #[serde(default = "farmer_port")]
    pub port: u16,
    #[serde(default)]
    pub pool_public_keys: Vec<Bytes48>,
    #[serde(default)]
    pub xch_target_address: Bytes32,
    #[serde(default = "default_true")]
    pub start_rpc_server: bool,
    #[serde(default = "farmer_rpc_port")]
    pub rpc_port: u16,
    #[serde(default = "farmer_pool_share_threshold")]
    pub pool_share_threshold: usize,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "farmer_ssl")]
    pub ssl: CombinedSsl,
}
impl Default for FarmerConfig {
    fn default() -> Self {
        FarmerConfig {
            full_node_peers: full_node_peers(),
            port: farmer_port(),
            pool_public_keys: vec![],
            xch_target_address: Bytes32::default(),
            start_rpc_server: true,
            rpc_port: farmer_rpc_port(),
            pool_share_threshold: farmer_pool_share_threshold(),
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            ssl: farmer_ssl(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolWalletConfig {
    #[serde(default)]
    pub launcher_id: Bytes32,
    #[serde(default)]
    pub pool_url: String,
    #[serde(default)]
    pub target_puzzle_hash: Bytes32,
    #[serde(default)]
    pub payout_instructions: String,
    #[serde(default)]
    pub p2_singleton_puzzle_hash: Bytes32,
    #[serde(default)]
    pub owner_public_key: Bytes48,
    #[serde(default)]
    pub difficulty: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolConfig {
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub xch_target_address: Bytes32,
    #[serde(default)]
    pub pool_list: Vec<PoolWalletConfig>,
}
impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            xch_target_address: Bytes32::default(),
            pool_list: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlotRefreshParameter {
    #[serde(default)]
    pub interval_seconds: usize,
    #[serde(default)]
    pub retry_invalid_seconds: usize,
    #[serde(default)]
    pub batch_size: usize,
    #[serde(default)]
    pub batch_sleep_milliseconds: usize,
}
impl Default for PlotRefreshParameter {
    fn default() -> Self {
        PlotRefreshParameter {
            interval_seconds: 120,
            retry_invalid_seconds: 1200,
            batch_size: 300,
            batch_sleep_milliseconds: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HarvesterConfig {
    #[serde(default = "harvester_farmer_peers")]
    pub farmer_peers: Vec<PeerConfig>,
    #[serde(default = "default_true")]
    pub start_rpc_server: bool,
    #[serde(default = "harvester_rpc_port")]
    pub rpc_port: u16,
    #[serde(default = "harvester_num_threads")]
    pub num_threads: usize,
    #[serde(default = "plots_refresh_parameter")]
    pub plots_refresh_parameter: PlotRefreshParameter,
    #[serde(default = "default_true")]
    pub parallel_read: bool,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub plot_directories: Vec<String>,
    #[serde(default = "default_true")]
    pub recursive_plot_scan: bool,
    #[serde(default = "harvester_ssl")]
    pub ssl: PrivateSsl,
    #[serde(default = "private_ssl_ca")]
    pub private_ssl_ca: CaSsl,
    #[serde(default = "chia_ssl_ca")]
    pub chia_ssl_ca: CaSsl,
    #[serde(default)]
    pub parallel_decompressor_count: u16,
    #[serde(default)]
    pub decompressor_thread_count: u16,
    #[serde(default)]
    pub disable_cpu_affinity: bool,
    #[serde(default = "harvester_max_compression_level_allowed")]
    pub max_compression_level_allowed: u8,
    #[serde(default)]
    pub use_gpu_harvesting: bool,
    #[serde(default)]
    pub gpu_index: u8,
    #[serde(default)]
    pub enforce_gpu_index: bool,
    #[serde(default = "harvester_decompressor_timeout")]
    pub decompressor_timeout: usize,
}
impl Default for HarvesterConfig {
    fn default() -> Self {
        HarvesterConfig {
            farmer_peers: harvester_farmer_peers(),
            start_rpc_server: true,
            rpc_port: harvester_rpc_port(),
            num_threads: harvester_num_threads(),
            plots_refresh_parameter: PlotRefreshParameter::default(),
            parallel_read: true,
            logging: logging(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            plot_directories: vec![],
            recursive_plot_scan: true,
            ssl: harvester_ssl(),
            private_ssl_ca: private_ssl_ca(),
            chia_ssl_ca: chia_ssl_ca(),
            parallel_decompressor_count: 0,
            decompressor_thread_count: 0,
            disable_cpu_affinity: false,
            max_compression_level_allowed: harvester_max_compression_level_allowed(),
            use_gpu_harvesting: false,
            gpu_index: 0,
            enforce_gpu_index: false,
            decompressor_timeout: harvester_decompressor_timeout(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CrawlerConfig {
    #[serde(default = "default_true")]
    pub start_rpc_server: bool,
    #[serde(default = "crawler_port")]
    pub rpc_port: u16,
    #[serde(default = "crawler_ssl")]
    pub ssl: PrivateSsl,
}
impl Default for CrawlerConfig {
    fn default() -> Self {
        CrawlerConfig {
            start_rpc_server: true,
            rpc_port: crawler_port(),
            ssl: crawler_ssl(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Soa {
    pub rname: String,
    pub serial_number: u32,
    pub refresh: u32,
    pub retry: u32,
    pub expire: u32,
    pub minimum: u32,
}
impl Default for Soa {
    fn default() -> Self {
        Soa {
            rname: "hostmaster.example.com".to_string(),
            serial_number: 1_619_105_223,
            refresh: 10_800,
            retry: 10_800,
            expire: 604_800,
            minimum: 1_800,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeederConfig {
    #[serde(default = "seeder_port")]
    pub port: u16,
    #[serde(default = "seeder_other_peers_port")]
    pub other_peers_port: u16,
    #[serde(default = "seeder_dns_port")]
    pub dns_port: u16,
    #[serde(default = "seeder_peer_connect_timeout")]
    pub peer_connect_timeout: usize,
    #[serde(default = "seeder_crawler_db_path")]
    pub crawler_db_path: String,
    #[serde(default = "seeder_bootstrap_peers")]
    pub bootstrap_peers: Vec<String>,
    #[serde(default = "seeder_minimum_height")]
    pub minimum_height: usize,
    #[serde(default = "seeder_minimum_version_count")]
    pub minimum_version_count: usize,
    #[serde(default = "seeder_domain_name")]
    pub domain_name: String,
    #[serde(default = "seeder_nameserver")]
    pub nameserver: String,
    #[serde(default = "seeder_ttl")]
    pub ttl: usize,
    #[serde(default)]
    pub soa: Soa,
    #[serde(default = "network_overrides")]
    pub network_overrides: NetworkOverrides,
    #[serde(default = "selected_network")]
    pub selected_network: String,
    #[serde(default = "logging")]
    pub logging: LoggingConfig,
    #[serde(default = "crawler")]
    pub crawler: CrawlerConfig,
}
impl Default for SeederConfig {
    fn default() -> Self {
        SeederConfig {
            port: seeder_port(),
            other_peers_port: seeder_other_peers_port(),
            dns_port: seeder_dns_port(),
            peer_connect_timeout: seeder_peer_connect_timeout(),
            crawler_db_path: seeder_crawler_db_path(),
            bootstrap_peers: seeder_bootstrap_peers(),
            minimum_height: seeder_minimum_height(),
            minimum_version_count: seeder_minimum_version_count(),
            domain_name: seeder_domain_name(),
            nameserver: seeder_nameserver(),
            ttl: seeder_ttl(),
            soa: Soa::default(),
            network_overrides: network_overrides(),
            selected_network: selected_network(),
            logging: logging(),
            crawler: crawler(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LoggingConfig {
    pub log_stdout: bool,
    pub log_filename: String,
    pub log_level: String,
    pub log_maxfilesrotation: u8,
    pub log_maxbytesrotation: usize,
    pub log_use_gzip: bool,
    pub log_syslog: bool,
    pub log_syslog_host: String,
    pub log_syslog_port: u16,
}
impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            log_stdout: false,
            log_filename: "log/debug.log".to_string(),
            log_level: "WARNING".to_string(),
            log_maxfilesrotation: 7,
            log_maxbytesrotation: 52_428_800,
            log_use_gzip: false,
            log_syslog: false,
            log_syslog_host: "localhost".to_string(),
            log_syslog_port: 514,
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigOverride {
    pub address_prefix: Option<String>,
    pub default_full_node_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigOverrides {
    pub mainnet: Option<ConfigOverride>,
    pub testnet0: Option<ConfigOverride>,
    pub testnet2: Option<ConfigOverride>,
    pub testnet3: Option<ConfigOverride>,
    pub testnet4: Option<ConfigOverride>,
    pub testnet5: Option<ConfigOverride>,
    pub testnet7: Option<ConfigOverride>,
    pub testnet10: Option<ConfigOverride>,
    pub testnet11: Option<ConfigOverride>,
}
impl Default for ConfigOverrides {
    fn default() -> Self {
        ConfigOverrides {
            mainnet: Some(ConfigOverride {
                address_prefix: Some("xch".to_string()),
                default_full_node_port: Some(8444),
            }),
            testnet0: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: Some(58444),
            }),
            testnet2: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: None,
            }),
            testnet3: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: None,
            }),
            testnet4: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: None,
            }),
            testnet5: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: None,
            }),
            testnet7: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: Some(58444),
            }),
            testnet10: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: Some(58444),
            }),
            testnet11: Some(ConfigOverride {
                address_prefix: Some("txch".to_string()),
                default_full_node_port: Some(58444),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConstantsOverrides {
    pub mainnet: Option<ConsensusOverrides>,
    pub testnet0: Option<ConsensusOverrides>,
    pub testnet2: Option<ConsensusOverrides>,
    pub testnet3: Option<ConsensusOverrides>,
    pub testnet4: Option<ConsensusOverrides>,
    pub testnet5: Option<ConsensusOverrides>,
    pub testnet7: Option<ConsensusOverrides>,
    pub testnet10: Option<ConsensusOverrides>,
    pub testnet11: Option<ConsensusOverrides>,
}
impl Default for ConstantsOverrides {
    #[allow(clippy::too_many_lines)]
    fn default() -> Self {
        ConstantsOverrides {
            mainnet: Some(ConsensusOverrides {
                genesis_challenge: Some(Bytes32::from(
                    "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                ..Default::default()
            }),
            testnet0: Some(ConsensusOverrides {
                genesis_challenge: Some(Bytes32::from(
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                min_plot_size: Some(18),
                ..Default::default()
            }),
            testnet2: Some(ConsensusOverrides {
                difficulty_constant_factor: Some(10_052_721_566_054),
                genesis_challenge: Some(Bytes32::from(
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                min_plot_size: Some(18),
                ..Default::default()
            }),
            testnet3: Some(ConsensusOverrides {
                difficulty_constant_factor: Some(10_052_721_566_054),
                genesis_challenge: Some(Bytes32::from(
                    "ca7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                mempool_block_buffer: Some(BigInt::from(10)),
                min_plot_size: Some(18),
                ..Default::default()
            }),
            testnet4: Some(ConsensusOverrides {
                difficulty_constant_factor: Some(10_052_721_566_054),
                difficulty_starting: Some(30),
                epoch_blocks: Some(768),
                genesis_challenge: Some(Bytes32::from(
                    "dd7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                mempool_block_buffer: Some(BigInt::from(10)),
                min_plot_size: Some(18),
                ..Default::default()
            }),
            testnet5: Some(ConsensusOverrides {
                difficulty_constant_factor: Some(10_052_721_566_054),
                difficulty_starting: Some(30),
                epoch_blocks: Some(768),
                genesis_challenge: Some(Bytes32::from(
                    "ee7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                mempool_block_buffer: Some(BigInt::from(10)),
                min_plot_size: Some(18),
                ..Default::default()
            }),
            testnet7: Some(ConsensusOverrides {
                difficulty_constant_factor: Some(10_052_721_566_054),
                difficulty_starting: Some(30),
                epoch_blocks: Some(768),
                genesis_challenge: Some(Bytes32::from(
                    "117816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                mempool_block_buffer: Some(BigInt::from(50)),
                min_plot_size: Some(18),
                ..Default::default()
            }),
            testnet10: Some(ConsensusOverrides {
                agg_sig_me_additional_data: Some(Bytes32::from(
                    "ae83525ba8d1dd3f09b277de18ca3e43fc0af20d20c4b3e92ef2a48bd291ccb2",
                )),
                difficulty_constant_factor: Some(10_052_721_566_054),
                difficulty_starting: Some(30),
                epoch_blocks: Some(768),
                genesis_challenge: Some(Bytes32::from(
                    "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                mempool_block_buffer: Some(BigInt::from(10)),
                min_plot_size: Some(18),
                soft_fork2_height: Some(3_000_000),
                hard_fork_height: Some(2_997_292),
                hard_fork_fix_height: Some(3_426_000),
                plot_filter_128_height: Some(3_061_804),
                plot_filter_64_height: Some(8_010_796),
                plot_filter_32_height: Some(13_056_556),
                ..Default::default()
            }),
            testnet11: Some(ConsensusOverrides {
                agg_sig_me_additional_data: Some(Bytes32::from(
                    "37a90eb5185a9c4439a91ddc98bbadce7b4feba060d50116a067de66bf236615",
                )),
                difficulty_constant_factor: Some(10_052_721_566_054),
                difficulty_starting: Some(30),
                epoch_blocks: Some(768),
                genesis_challenge: Some(Bytes32::from(
                    "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb",
                )),
                genesis_pre_farm_pool_puzzle_hash: Some(Bytes32::from(
                    "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
                )),
                genesis_pre_farm_farmer_puzzle_hash: Some(Bytes32::from(
                    "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
                )),
                mempool_block_buffer: Some(BigInt::from(10)),
                min_plot_size: Some(18),
                hard_fork_height: Some(0),
                hard_fork_fix_height: Some(0),
                plot_filter_128_height: Some(6_029_568),
                plot_filter_64_height: Some(11_075_328),
                plot_filter_32_height: Some(16_121_088),
                ..Default::default()
            }),
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NetworkOverrides {
    pub constants: ConstantsOverrides,
    pub config: ConfigOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CombinedSsl {
    pub private_crt: String,
    pub private_key: String,
    pub public_crt: String,
    pub public_key: String,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaSsl {
    pub crt: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublicSsl {
    pub public_crt: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrivateSsl {
    pub private_crt: String,
    pub private_key: String,
}
