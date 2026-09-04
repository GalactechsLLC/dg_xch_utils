#[cfg(feature = "pos2")]
pub mod chain;
pub mod config;
pub mod error;
#[cfg(feature = "pos2")]
pub mod factory;
pub mod plots;
#[cfg(feature = "pos2")]
pub mod pos2;
pub mod rpc;
#[cfg(feature = "server")]
pub mod server;
pub mod stats;
pub mod step;
#[cfg(feature = "server")]
mod tasks;
pub mod timelord;

pub use config::{HarnessConfig, SimConfig};
pub use error::{ConfigError, SimError, ValidationTier};
pub use plots::PlotKeys;
#[cfg(feature = "pos2")]
pub use pos2::{Plot, PlotSet};
pub use rpc::start_simulator;
pub use step::{TimestampEmitter, reorg_seed};
pub use timelord::prove_vdf;

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
