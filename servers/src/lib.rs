pub mod app_config;
pub mod chain_config;
pub mod observability;
pub mod transport;
pub mod web_theme;
pub mod websocket;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

#[cfg(feature = "discovery-postgres")]
pub mod discovery;
