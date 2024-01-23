pub mod api;
pub mod rpc;
pub mod websocket;

#[derive(Default, Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClientSSLConfig {
    pub ssl_crt_path: String,
    pub ssl_key_path: String,
    pub ssl_ca_crt_path: String,
}
